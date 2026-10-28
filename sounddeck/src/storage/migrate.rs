//! One-time import of the legacy single-file `sounddeck.json`.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::io;
use super::{save_libraries, save_settings, LibrariesFile, LibraryConfig, Loaded, Settings};
use crate::model::{Layout, CONFIG_FILE};

/// Where the imported legacy file is kept once the new files exist. A taken
/// name gets a numbered suffix (`sounddeck.v1.json.bak.2`, …).
const LEGACY_BACKUP_FILE: &str = "sounddeck.v1.json.bak";

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
        }
    }
}

/// Import `sounddeck.json` into the new config folder.
///
/// Returns `None` when there is nothing usable to import. The legacy file is
/// only renamed after both new files are written.
///
/// On the success paths every collected warning travels back inside
/// [`Loaded::warnings`] and the caller's `warnings` vec is left empty. On the
/// `None` paths there is no `Loaded` to carry them, so they go into `warnings`,
/// which the caller then keeps collecting for the normal load.
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
                // The legacy config predates the trim: unity, like any new sound.
                gain_db: 0.0,
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
            warnings: std::mem::take(warnings),
        });
    }
    if let Err(err) = save_settings(&settings_path, &settings) {
        warnings.push(format!(
            "Could not import settings into config/settings.json: {err}"
        ));
        return Some(Loaded {
            settings,
            libraries: file.libraries,
            warnings: std::mem::take(warnings),
        });
    }

    // Keep any earlier backup: the rename below must not overwrite one.
    match io::free_path(&base.join(LEGACY_BACKUP_FILE)) {
        Some(backup) => match fs::rename(&legacy_path, &backup) {
            Ok(()) => warnings.push(format!(
                "Imported {imported_sounds} sound(s) into config/. The old file is {}.",
                io::file_label(&backup)
            )),
            Err(err) => warnings.push(format!(
                "Imported the old config, but could not rename {CONFIG_FILE}: {err}"
            )),
        },
        None => warnings.push(format!(
            "Imported the old config, but every {LEGACY_BACKUP_FILE} name is taken; \
             {CONFIG_FILE} was left in place."
        )),
    }

    Some(Loaded {
        settings,
        libraries: file.libraries,
        warnings: std::mem::take(warnings),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{libraries_path, settings_path, test_dir};

    fn legacy_json() -> String {
        serde_json::json!({
            "version": 1,
            "sounds": [{
                "id": 3,
                "name": "Airhorn",
                "path": "audio/airhorn.wav",
                "hotkey": "Alt+1",
                "volume": 0.8,
                "ram": null,
                "library_id": 1
            }],
            "libraries": [{ "id": 1, "name": "Main" }],
            "active_library": 1,
        })
        .to_string()
    }

    #[test]
    fn import_should_return_its_own_warnings() {
        let base = test_dir("migrate-warnings");
        fs::write(base.join(CONFIG_FILE), legacy_json()).unwrap();

        let mut warnings = Vec::new();
        let loaded = import(&base, &mut warnings).expect("import");

        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains("Imported 1 sound")),
            "{:?}",
            loaded.warnings
        );
        // The success path hands its warnings back inside `Loaded`, so the
        // caller's vec must not be used as a second sink.
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(settings_path(&base).exists());
        assert!(libraries_path(&base).exists());
        assert!(!base.join(CONFIG_FILE).exists());
    }

    #[test]
    fn backup_rename_should_not_clobber_an_existing_backup() {
        let base = test_dir("migrate-backup");
        fs::write(base.join(CONFIG_FILE), legacy_json()).unwrap();
        let existing = base.join(LEGACY_BACKUP_FILE);
        fs::write(&existing, "previous backup").unwrap();

        let mut warnings = Vec::new();
        let loaded = import(&base, &mut warnings).expect("import");

        assert_eq!(fs::read_to_string(&existing).unwrap(), "previous backup");
        let numbered = base.join(format!("{LEGACY_BACKUP_FILE}.2"));
        assert!(numbered.exists());
        assert_eq!(fs::read_to_string(&numbered).unwrap(), legacy_json());
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains(&format!("{LEGACY_BACKUP_FILE}.2"))),
            "{:?}",
            loaded.warnings
        );
        assert!(!base.join(CONFIG_FILE).exists());
    }

    #[test]
    fn unreadable_legacy_file_should_report_through_the_caller() {
        let base = test_dir("migrate-corrupt");
        fs::write(base.join(CONFIG_FILE), "{ broken").unwrap();

        let mut warnings = Vec::new();
        assert!(import(&base, &mut warnings).is_none());

        assert!(
            warnings.iter().any(|w| w.contains("unreadable")),
            "{warnings:?}"
        );
        assert!(!base.join(CONFIG_FILE).exists());
    }
}
