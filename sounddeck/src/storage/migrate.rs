//! One-time import of the legacy single-file `sounddeck.json`.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::{save_libraries, save_settings, LibrariesFile, LibraryConfig, Loaded, Settings};
use crate::model::{Layout, CONFIG_FILE};

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
struct LegacySound {
    id: u64,
    name: String,
    path: String,
    hotkey: Option<String>,
    volume: f32,
    ram: Option<bool>,
    library_id: u64,
}

impl Default for LegacySound {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            path: String::new(),
            hotkey: None,
            volume: 1.0,
            ram: None,
            library_id: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
struct LegacyLibrary {
    id: u64,
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
struct LegacyConfig {
    version: u32,
    sounds: Vec<LegacySound>,
    libraries: Vec<LegacyLibrary>,
    active_library: u64,
    monitor_device: Option<String>,
    cable_device: Option<String>,
    monitor_name: Option<String>,
    cable_name: Option<String>,
    monitor_volume: f32,
    cable_volume: f32,
    master_gain_db: f32,
    link_volumes: bool,
    preload_hotkey_sounds: bool,
    theme: Option<String>,
    layout: Option<String>,
    window_size: Option<[f32; 2]>,
}

impl Default for LegacyConfig {
    fn default() -> Self {
        Self {
            version: 1,
            sounds: Vec::new(),
            libraries: Vec::new(),
            active_library: 0,
            monitor_device: None,
            cable_device: None,
            monitor_name: None,
            cable_name: None,
            monitor_volume: 1.0,
            cable_volume: 1.0,
            master_gain_db: 0.0,
            link_volumes: true,
            preload_hotkey_sounds: true,
            theme: None,
            layout: None,
            window_size: None,
        }
    }
}

/// Import `sounddeck.json` into the new config folder.
///
/// Returns `None` when there is nothing usable to import. The legacy file is
/// only renamed after both new files are written.
pub fn import(base: &Path, warnings: &mut Vec<String>) -> Option<Loaded> {
    let legacy_path = base.join(CONFIG_FILE);
    let text = fs::read_to_string(&legacy_path).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    let legacy: LegacyConfig = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            let corrupt = base.join("sounddeck.json.corrupt");
            let _ = fs::rename(&legacy_path, &corrupt);
            warnings.push(format!(
                "sounddeck.json was unreadable ({err}). Kept as sounddeck.json.corrupt; starting fresh."
            ));
            return None;
        }
    };

    // Tabs, or a single Main tab for old files.
    let mut libraries: Vec<LibraryConfig> = if legacy.libraries.is_empty() {
        vec![LibraryConfig {
            id: 1,
            name: "Main".into(),
            sounds: Vec::new(),
        }]
    } else {
        legacy
            .libraries
            .iter()
            .map(|library| LibraryConfig {
                id: library.id,
                name: library.name.clone(),
                sounds: Vec::new(),
            })
            .collect()
    };

    let fallback_id = libraries[0].id;
    let mut imported_sounds = 0usize;
    for sound in &legacy.sounds {
        let target_id = if libraries
            .iter()
            .any(|library| library.id == sound.library_id)
        {
            sound.library_id
        } else {
            fallback_id
        };
        if let Some(library) = libraries.iter_mut().find(|library| library.id == target_id) {
            library.sounds.push(super::SoundConfig {
                id: sound.id,
                name: sound.name.clone(),
                path: sound.path.clone(),
                hotkey: sound.hotkey.clone(),
                volume: sound.volume,
                ram: sound.ram,
            });
            imported_sounds += 1;
        }
    }

    let mut file = LibrariesFile {
        version: super::LIBRARIES_VERSION,
        libraries,
    };
    file.normalize();

    let active_library = if file
        .libraries
        .iter()
        .any(|library| library.id == legacy.active_library)
    {
        legacy.active_library
    } else {
        file.libraries[0].id
    };

    let mut settings = Settings {
        active_library,
        theme: legacy
            .theme
            .as_deref()
            .map(|theme| theme.trim().to_ascii_lowercase())
            .filter(|theme| !theme.is_empty())
            .unwrap_or_else(|| "graphite".to_string()),
        layout: match legacy.layout.as_deref() {
            Some("list") => Layout::List,
            _ => Layout::Grid,
        },
        monitor_device: legacy.monitor_device.clone(),
        cable_device: legacy.cable_device.clone(),
        monitor_name: legacy.monitor_name.clone(),
        cable_name: legacy.cable_name.clone(),
        monitor_volume: legacy.monitor_volume,
        cable_volume: legacy.cable_volume,
        master_gain_db: legacy.master_gain_db,
        link_volumes: legacy.link_volumes,
        preload_hotkey_sounds: legacy.preload_hotkey_sounds,
        window_size: legacy.window_size,
        // Everything else takes the current default, so a new field never
        // silently imports as zero from a legacy file.
        ..Settings::default()
    };
    // The legacy file is unvalidated JSON: clamp it exactly like a normal load.
    settings.normalize();

    let libraries_path = super::libraries_path(base);
    let settings_path = super::settings_path(base);

    // Write libraries first. Settings is the "migration finished" marker, so a
    // partial write re-runs the import next launch instead of losing data.
    if let Err(err) = save_libraries(&libraries_path, &file.libraries) {
        warnings.push(format!(
            "Could not import libraries into config/libraries.json: {err}"
        ));
        return Some(Loaded {
            settings,
            libraries: file.libraries,
            warnings: Vec::new(),
        });
    }
    if let Err(err) = save_settings(&settings_path, &settings) {
        warnings.push(format!(
            "Could not import settings into config/settings.json: {err}"
        ));
        return Some(Loaded {
            settings,
            libraries: file.libraries,
            warnings: Vec::new(),
        });
    }

    let backup = base.join("sounddeck.v1.json.bak");
    if let Err(err) = fs::rename(&legacy_path, &backup) {
        warnings.push(format!(
            "Imported the old config, but could not rename sounddeck.json: {err}"
        ));
    } else {
        warnings.push(format!(
            "Imported {imported_sounds} sound(s) into config/. The old file is sounddeck.v1.json.bak."
        ));
    }

    Some(Loaded {
        settings,
        libraries: file.libraries,
        warnings: Vec::new(),
    })
}
