//! Portable storage: `config/settings.json` + `config/libraries.json`.
//!
//! Both files are JSON, written atomically with one rolling `.bak` each. The
//! legacy single-file `sounddeck.json` is imported once and kept as
//! `sounddeck.v1.json.bak`.

pub mod io;
pub mod migrate;

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::Layout;

pub const SETTINGS_VERSION: u32 = 2;
pub const LIBRARIES_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub active_library: u64,
    pub theme: String,
    pub layout: Layout,
    pub monitor_device: Option<String>,
    pub cable_device: Option<String>,
    pub monitor_name: Option<String>,
    pub cable_name: Option<String>,
    pub monitor_volume: f32,
    pub cable_volume: f32,
    pub master_gain_db: f32,
    pub link_volumes: bool,
    pub preload_hotkey_sounds: bool,
    /// Shows the loop player in the sidebar. Does not auto-start playback.
    pub loop_enabled: bool,
    /// Referenced file for the looping track; same path rules as sounds.
    pub loop_path: Option<String>,
    /// Linear loop level, independent of the Monitor/Cable volumes.
    pub loop_volume: f32,
    /// Route the loop to the monitor output.
    pub loop_on_monitor: bool,
    /// Route the loop to the cable output.
    pub loop_on_cable: bool,
    /// Parsed for older config files, but the window always starts at a fixed
    /// size, so this is no longer read or written.
    #[allow(dead_code)]
    pub window_size: Option<[f32; 2]>,
    pub animate_ui: bool,
    /// Minimize hides to the tray instead of the taskbar.
    pub minimize_to_tray: bool,
    /// Show the master-gain slider in the board sidebar.
    pub show_gain_slider: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            active_library: 1,
            theme: "graphite".to_string(),
            layout: Layout::Grid,
            monitor_device: None,
            cable_device: None,
            monitor_name: None,
            cable_name: None,
            monitor_volume: 1.0,
            cable_volume: 1.0,
            master_gain_db: 0.0,
            link_volumes: true,
            preload_hotkey_sounds: true,
            loop_enabled: false,
            loop_path: None,
            loop_volume: 1.0,
            loop_on_monitor: true,
            loop_on_cable: true,
            window_size: None,
            animate_ui: true,
            minimize_to_tray: false,
            show_gain_slider: true,
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        if self.version < SETTINGS_VERSION {
            self.version = SETTINGS_VERSION;
        }
        self.monitor_volume = self.monitor_volume.clamp(0.0, 1.0);
        self.cable_volume = self.cable_volume.clamp(0.0, 1.0);
        self.master_gain_db = self.master_gain_db.clamp(0.0, 12.0);
        self.loop_path = self.loop_path.take().and_then(|path| {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        self.loop_volume = self.loop_volume.clamp(0.0, 1.0);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub hotkey: Option<String>,
    pub volume: f32,
    /// `None` on old files means "inherit the global preload setting".
    pub ram: Option<bool>,
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            path: String::new(),
            hotkey: None,
            volume: 1.0,
            ram: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryConfig {
    pub id: u64,
    pub name: String,
    pub sounds: Vec<SoundConfig>,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            sounds: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LibrariesFile {
    pub version: u32,
    pub libraries: Vec<LibraryConfig>,
}

impl Default for LibrariesFile {
    fn default() -> Self {
        Self {
            version: LIBRARIES_VERSION,
            libraries: vec![LibraryConfig {
                id: 1,
                name: "Main".into(),
                sounds: Vec::new(),
            }],
        }
    }
}

impl LibrariesFile {
    pub fn normalize(&mut self) {
        if self.version < LIBRARIES_VERSION {
            self.version = LIBRARIES_VERSION;
        }
        if self.libraries.is_empty() {
            self.libraries.push(LibraryConfig {
                id: 1,
                name: "Main".into(),
                sounds: Vec::new(),
            });
        }

        // Unique, non-zero library ids and names.
        let mut seen_libraries = HashSet::new();
        let mut next_library = 1u64;
        for library in &mut self.libraries {
            if library.id == 0 || !seen_libraries.insert(library.id) {
                while seen_libraries.contains(&next_library) {
                    next_library += 1;
                }
                library.id = next_library;
                seen_libraries.insert(library.id);
            }
            next_library = next_library.max(library.id + 1);
            if library.name.trim().is_empty() {
                library.name = format!("Library {}", library.id);
            }
        }

        // Sound ids must be globally unique: playback, hotkeys and undo key on them.
        let mut max_sound = 0u64;
        for library in &self.libraries {
            for sound in &library.sounds {
                max_sound = max_sound.max(sound.id);
            }
        }
        let mut next_sound = max_sound.max(1) + 1;
        let mut seen_sounds = HashSet::new();
        for library in &mut self.libraries {
            for sound in &mut library.sounds {
                if sound.id == 0 || !seen_sounds.insert(sound.id) {
                    sound.id = next_sound;
                    next_sound += 1;
                    seen_sounds.insert(sound.id);
                }
                sound.volume = sound.volume.clamp(0.0, 1.0);
                if sound.name.trim().is_empty() {
                    sound.name = "Untitled sound".to_string();
                }
            }
        }
    }
}

pub struct Loaded {
    pub settings: Settings,
    pub libraries: Vec<LibraryConfig>,
    pub warnings: Vec<String>,
}

fn dir_of(base: &Path) -> std::path::PathBuf {
    base.join("config")
}

pub fn settings_path(base: &Path) -> std::path::PathBuf {
    dir_of(base).join("settings.json")
}

pub fn libraries_path(base: &Path) -> std::path::PathBuf {
    dir_of(base).join("libraries.json")
}

/// Load both files, importing the legacy single config on first run.
pub fn load(base: &Path) -> Loaded {
    let dir = dir_of(base);
    let settings_file = settings_path(base);
    let libraries_file = libraries_path(base);
    let legacy = base.join(crate::model::CONFIG_FILE);
    let mut warnings = Vec::new();

    if legacy.exists() && !(settings_file.exists() && libraries_file.exists()) {
        if let Some(mut loaded) = migrate::import(base, &mut warnings) {
            io::sweep_tmp(&dir);
            loaded.warnings.splice(0..0, warnings);
            return loaded;
        }
    }

    let mut settings: Settings = io::load_json(&settings_file, "settings.json", &mut warnings);
    let mut libraries: LibrariesFile =
        io::load_json(&libraries_file, "libraries.json", &mut warnings);

    settings.normalize();
    libraries.normalize();

    if !libraries
        .libraries
        .iter()
        .any(|library| library.id == settings.active_library)
    {
        settings.active_library = libraries.libraries[0].id;
    }

    io::sweep_tmp(&dir);

    Loaded {
        settings,
        libraries: libraries.libraries,
        warnings,
    }
}

pub fn save_settings(path: &Path, settings: &Settings) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(settings)?;
    io::write_json_atomic(path, &json)
}

pub fn save_libraries(path: &Path, libraries: &[LibraryConfig]) -> anyhow::Result<()> {
    let file = LibrariesFile {
        version: LIBRARIES_VERSION,
        libraries: libraries.to_vec(),
    };
    let json = serde_json::to_string_pretty(&file)?;
    io::write_json_atomic(path, &json)
}

pub fn serialize_settings(settings: &Settings) -> String {
    serde_json::to_string_pretty(settings).unwrap_or_default()
}

pub fn serialize_libraries(libraries: &[LibraryConfig]) -> String {
    let file = LibrariesFile {
        version: LIBRARIES_VERSION,
        libraries: libraries.to_vec(),
    };
    serde_json::to_string_pretty(&file).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    fn temp_base(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sounddeck-store-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sound(id: u64, name: &str) -> SoundConfig {
        SoundConfig {
            id,
            name: name.into(),
            path: format!("audio/{name}.wav"),
            hotkey: None,
            volume: 1.0,
            ram: None,
        }
    }

    #[test]
    fn roundtrip_settings_and_libraries() {
        let base = temp_base("roundtrip");
        let mut settings = Settings::default();
        settings.active_library = 2;
        settings.master_gain_db = 3.5;
        settings.animate_ui = false;
        settings.loop_enabled = true;
        settings.loop_path = Some("C:\\loops\\rain.flac".to_string());
        settings.loop_volume = 0.42;
        settings.loop_on_monitor = false;
        settings.loop_on_cable = true;
        settings.minimize_to_tray = true;
        settings.show_gain_slider = false;

        let libraries = vec![
            LibraryConfig {
                id: 1,
                name: "Main".into(),
                sounds: vec![sound(1, "airhorn")],
            },
            LibraryConfig {
                id: 2,
                name: "Voices".into(),
                sounds: vec![sound(2, "hello")],
            },
        ];

        save_settings(&settings_path(&base), &settings).unwrap();
        save_libraries(&libraries_path(&base), &libraries).unwrap();

        let loaded = load(&base);
        assert_eq!(loaded.settings.active_library, 2);
        assert_eq!(loaded.settings.master_gain_db, 3.5);
        assert!(!loaded.settings.animate_ui);
        assert!(loaded.settings.loop_enabled);
        assert_eq!(
            loaded.settings.loop_path.as_deref(),
            Some("C:\\loops\\rain.flac")
        );
        assert!((loaded.settings.loop_volume - 0.42).abs() < 1e-6);
        assert!(!loaded.settings.loop_on_monitor);
        assert!(loaded.settings.loop_on_cable);
        assert!(loaded.settings.minimize_to_tray);
        assert!(!loaded.settings.show_gain_slider);
        assert_eq!(loaded.libraries.len(), 2);
        assert_eq!(loaded.libraries[1].sounds[0].name, "hello");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    }

    #[test]
    fn loop_outputs_default_to_on_for_old_files() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.loop_on_monitor);
        assert!(settings.loop_on_cable);
        assert!(!settings.minimize_to_tray);
        assert!(settings.show_gain_slider);
    }

    #[test]
    fn loop_settings_normalize() {
        let mut settings = Settings::default();
        assert!(!settings.loop_enabled);
        assert!(settings.loop_path.is_none());

        settings.loop_path = Some("   ".into());
        settings.normalize();
        assert!(settings.loop_path.is_none());

        settings.loop_path = Some(" C:\\loops\\rain.mp3 ".into());
        settings.normalize();
        assert_eq!(settings.loop_path.as_deref(), Some("C:\\loops\\rain.mp3"));

        // Disabling the feature keeps the path.
        settings.loop_enabled = false;
        settings.normalize();
        assert_eq!(settings.loop_path.as_deref(), Some("C:\\loops\\rain.mp3"));

        settings.loop_volume = 2.5;
        settings.normalize();
        assert!((settings.loop_volume - 1.0).abs() < 1e-6);
        settings.loop_volume = -1.0;
        settings.normalize();
        assert_eq!(settings.loop_volume, 0.0);
    }

    #[test]
    fn migrates_legacy_single_file() {
        let base = temp_base("migrate");
        let legacy = json!({
            "version": 1,
            "sounds": [
                { "id": 7, "name": "Airhorn", "path": "audio/airhorn.wav", "hotkey": "Alt+1", "volume": 0.8, "ram": true, "library_id": 1 },
                { "id": 8, "name": "Hello", "path": "audio/hello.wav", "hotkey": null, "volume": 1.0, "ram": null, "library_id": 99 }
            ],
            "libraries": [ { "id": 1, "name": "Main" }, { "id": 2, "name": "Voices" } ],
            "active_library": 2,
            "monitor_device": "dev-a",
            "cable_device": "dev-b",
            "monitor_name": "Headphones",
            "cable_name": "Cable",
            "monitor_volume": 0.8,
            "cable_volume": 0.6,
            "master_gain_db": 2.0,
            "link_volumes": false,
            "preload_hotkey_sounds": true,
            "theme": "macchiato",
            "layout": "list",
            "window_size": [1000.0, 700.0],
            "next_id": 9
        });
        fs::write(base.join(crate::model::CONFIG_FILE), legacy.to_string()).unwrap();

        let loaded = load(&base);
        assert_eq!(loaded.libraries.len(), 2);
        // Library 99 does not exist, so Hello joins the first tab.
        assert_eq!(loaded.libraries[0].sounds.len(), 2);
        assert_eq!(loaded.libraries[1].sounds.len(), 0);
        assert_eq!(loaded.settings.active_library, 2);
        assert_eq!(loaded.settings.theme, "macchiato");
        assert_eq!(loaded.settings.layout, Layout::List);
        assert!(!loaded.settings.link_volumes);

        assert!(settings_path(&base).exists());
        assert!(libraries_path(&base).exists());
        assert!(!base.join(crate::model::CONFIG_FILE).exists());
        assert!(base.join("sounddeck.v1.json.bak").exists());

        // Re-loading uses the new files, not the backup.
        let again = load(&base);
        assert_eq!(again.libraries[0].sounds.len(), 2);
    }

    #[test]
    fn restores_from_backup_when_main_is_corrupt() {
        let base = temp_base("bak");
        let first = vec![LibraryConfig {
            id: 1,
            name: "Main".into(),
            sounds: vec![sound(1, "first")],
        }];
        let second = vec![LibraryConfig {
            id: 1,
            name: "Main".into(),
            sounds: vec![sound(2, "second")],
        }];
        save_libraries(&libraries_path(&base), &first).unwrap();
        save_libraries(&libraries_path(&base), &second).unwrap();

        fs::write(libraries_path(&base), "{ broken").unwrap();

        let loaded = load(&base);
        assert_eq!(loaded.libraries[0].sounds[0].name, "first");
        assert!(libraries_path(&base)
            .with_file_name("libraries.json.corrupt")
            .exists());
        assert!(loaded.warnings.iter().any(|w| w.contains("restored")));
    }

    #[test]
    fn promotes_tmp_after_interrupted_save() {
        let base = temp_base("tmp");
        let libraries = vec![LibraryConfig {
            id: 1,
            name: "Main".into(),
            sounds: vec![sound(3, "three")],
        }];
        save_libraries(&libraries_path(&base), &libraries).unwrap();
        let main = libraries_path(&base);
        let tmp = io::sidecar(&main, "tmp");
        fs::copy(&main, &tmp).unwrap();
        fs::remove_file(&main).unwrap();

        let loaded = load(&base);
        assert_eq!(loaded.libraries[0].sounds[0].name, "three");
        assert!(main.exists());
        assert!(!tmp.exists());
    }

    #[test]
    fn tolerates_bom_and_keeps_one_backup() {
        let base = temp_base("bom");
        let mut settings = Settings::default();
        settings.master_gain_db = 1.0;
        let json = format!(
            "\u{feff}{}",
            serde_json::to_string_pretty(&settings).unwrap()
        );
        fs::create_dir_all(base.join("config")).unwrap();
        fs::write(settings_path(&base), json).unwrap();

        let loaded = load(&base);
        assert_eq!(loaded.settings.master_gain_db, 1.0);
        let tmp = settings_path(&base).with_file_name("settings.json.tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn normalizes_duplicate_ids_and_empty_names() {
        let mut file = LibrariesFile {
            version: 1,
            libraries: vec![
                LibraryConfig {
                    id: 0,
                    name: String::new(),
                    sounds: vec![
                        SoundConfig {
                            id: 0,
                            name: String::new(),
                            ..Default::default()
                        },
                        SoundConfig {
                            id: 0,
                            name: "dup".into(),
                            ..Default::default()
                        },
                    ],
                },
                LibraryConfig {
                    id: 0,
                    name: "Second".into(),
                    sounds: Vec::new(),
                },
            ],
        };
        file.normalize();

        assert_eq!(file.libraries.len(), 2);
        assert_ne!(file.libraries[0].id, file.libraries[1].id);
        assert_ne!(
            file.libraries[0].sounds[0].id,
            file.libraries[0].sounds[1].id
        );
        assert!(!file.libraries[0].name.is_empty());
        assert!(!file.libraries[0].sounds[0].name.is_empty());
    }

    #[test]
    fn empty_library_file_gets_main() {
        let mut file = LibrariesFile {
            version: 1,
            libraries: Vec::new(),
        };
        file.normalize();
        assert_eq!(file.libraries.len(), 1);
        assert_eq!(file.libraries[0].name, "Main");
    }
}
