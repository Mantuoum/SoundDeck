//! Portable storage: `config/settings.json` + `config/libraries.json`.
//!
//! Both files are JSON, written atomically with one rolling `.bak` each. The
//! legacy single-file `sounddeck.json` is imported once and kept as
//! `sounddeck.v1.json.bak` (numbered if that name is already taken).

pub mod bundle;
pub mod io;
pub mod migrate;

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::audio::engine::clamp_gain_db;
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
    pub animate_ui: bool,
    /// Minimize hides to the tray instead of the taskbar.
    pub minimize_to_tray: bool,
    /// Show the master-gain slider in the board sidebar.
    pub show_gain_slider: bool,
    /// Ctrl+Tab / Ctrl+Shift+Tab cycle the library tabs. Off by default; while
    /// off the chords are not consumed (F2).
    pub cycle_tabs_ctrl_tab: bool,
    /// Show RAM/disk state (card chips, counts, toolbar pill). Off by default.
    pub show_ram_state: bool,
    /// Enables the global stop-all hotkey and its sidebar button.
    pub stop_all_hotkey_enabled: bool,
    /// Label form of the stop-all hotkey, e.g. "Ctrl+Alt+S".
    pub stop_all_hotkey: Option<String>,
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
            animate_ui: true,
            minimize_to_tray: false,
            show_gain_slider: true,
            cycle_tabs_ctrl_tab: false,
            show_ram_state: false,
            stop_all_hotkey_enabled: false,
            stop_all_hotkey: None,
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        if self.version < SETTINGS_VERSION {
            self.version = SETTINGS_VERSION;
        }
        self.monitor_volume = finite_or(self.monitor_volume, 1.0).clamp(0.0, 1.0);
        self.cable_volume = finite_or(self.cable_volume, 1.0).clamp(0.0, 1.0);
        self.master_gain_db = finite_or(self.master_gain_db, 0.0).clamp(0.0, 12.0);
        self.loop_path = self.loop_path.take().and_then(|path| {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        self.loop_volume = finite_or(self.loop_volume, 1.0).clamp(0.0, 1.0);
        self.stop_all_hotkey = self.stop_all_hotkey.take().and_then(|label| {
            let trimmed = label.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
    }

    /// Every field back to its default. Settings only: libraries, sounds and
    /// audio files are a different file and are never part of a reset.
    pub fn reset_all(&mut self) {
        *self = Self::default();
        self.normalize();
    }

    /// Keep `active_library` on a tab that exists; the first one is the fallback.
    pub fn clamp_active_library(&mut self, library_ids: &[u64]) {
        if library_ids.contains(&self.active_library) {
            return;
        }
        if let Some(first) = library_ids.first() {
            self.active_library = *first;
        }
    }
}

/// NaN and infinity from a hand-edited file would otherwise reach the mixer and
/// the meters, where they turn into silent audio and broken layout.
fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// First id at or above `from` that no entry holds, falling back to the low ids
/// once the counter saturates. A hostile `u64::MAX` id from a hand-edited file
/// must neither overflow the increment nor spin the search (D14/B38 class):
/// `seen` is finite, so one of the first `len + 1` ids is always free.
fn next_free_id(seen: &HashSet<u64>, from: u64) -> u64 {
    if from != 0 && !seen.contains(&from) {
        return from;
    }
    (1..=seen.len() as u64 + 1)
        .find(|id| !seen.contains(id))
        .unwrap_or(1)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub hotkey: Option<String>,
    pub volume: f32,
    /// Per-sound trim in dB, applied on top of `volume` (FR-5.3.17).
    pub gain_db: f32,
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
            gain_db: 0.0,
            ram: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct LibraryConfig {
    pub id: u64,
    pub name: String,
    pub sounds: Vec<SoundConfig>,
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
                library.id = next_free_id(&seen_libraries, next_library);
                seen_libraries.insert(library.id);
            }
            next_library = next_library.max(library.id.saturating_add(1));
            if library.name.trim().is_empty() {
                library.name = format!("Library {}", library.id);
            }
        }

        // Sound ids must be globally unique: playback, hotkeys and undo key on them.
        // The first generated id is 1 so an empty board starts from 1, matching
        // `App::next_id`.
        let mut max_sound = 0u64;
        for library in &self.libraries {
            for sound in &library.sounds {
                max_sound = max_sound.max(sound.id);
            }
        }
        let mut next_sound = max_sound.saturating_add(1);
        let mut seen_sounds = HashSet::new();
        for library in &mut self.libraries {
            for sound in &mut library.sounds {
                if sound.id == 0 || !seen_sounds.insert(sound.id) {
                    // Same saturation rule as the library ids: a hand-edited
                    // `u64::MAX` plus a duplicate must not overflow the counter.
                    sound.id = next_free_id(&seen_sounds, next_sound);
                    seen_sounds.insert(sound.id);
                    next_sound = next_sound.max(sound.id.saturating_add(1));
                }
                sound.volume = finite_or(sound.volume, 1.0).clamp(0.0, 1.0);
                // The trim range lives with the engine (FR-5.3.17); a file
                // written before the range shrank loads at the new floor.
                sound.gain_db = clamp_gain_db(sound.gain_db);
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
        if let Some(loaded) = migrate::import(base, &mut warnings) {
            io::sweep_tmp(&dir);
            // `import` returns its own warnings inside `loaded`.
            return loaded;
        }
    }

    let mut settings: Settings = io::load_json(&settings_file, "settings.json", &mut warnings);
    let mut libraries: LibrariesFile =
        io::load_json(&libraries_file, "libraries.json", &mut warnings);

    settings.normalize();
    libraries.normalize();
    fix_active_library(&mut settings, &libraries.libraries);

    io::sweep_tmp(&dir);

    Loaded {
        settings,
        libraries: libraries.libraries,
        warnings,
    }
}

pub fn save_settings(path: &Path, settings: &Settings) -> anyhow::Result<()> {
    io::write_json_atomic(path, &serialize_settings(settings)?)
}

pub fn save_libraries(path: &Path, libraries: &[LibraryConfig]) -> anyhow::Result<()> {
    io::write_json_atomic(path, &serialize_libraries(libraries)?)
}

pub fn serialize_settings(settings: &Settings) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(settings)?)
}

/// The libraries file exactly as it is written. Borrowing the tabs keeps one
/// serialization path for the fingerprint and the write, with no `to_vec()`.
/// Field order must match [`LibrariesFile`] so the bytes are identical.
#[derive(Serialize)]
pub(crate) struct LibrariesRef<'a> {
    version: u32,
    libraries: &'a [LibraryConfig],
}

impl<'a> LibrariesRef<'a> {
    pub(crate) fn new(libraries: &'a [LibraryConfig]) -> Self {
        Self {
            version: LIBRARIES_VERSION,
            libraries,
        }
    }
}

pub fn serialize_libraries(libraries: &[LibraryConfig]) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&LibrariesRef::new(libraries))?)
}

/// Keep `active_library` on a real tab. The config load and an imported bundle
/// both go through this one rule.
pub fn fix_active_library(settings: &mut Settings, libraries: &[LibraryConfig]) {
    let ids: Vec<u64> = libraries.iter().map(|library| library.id).collect();
    settings.clamp_active_library(&ids);
}

/// Fresh scratch directory for one storage test, keyed by test name.
#[cfg(test)]
pub fn test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sounddeck-store-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    fn temp_base(name: &str) -> PathBuf {
        test_dir(name)
    }

    fn sound(id: u64, name: &str) -> SoundConfig {
        SoundConfig {
            id,
            name: name.into(),
            path: format!("audio/{name}.wav"),
            hotkey: None,
            volume: 1.0,
            gain_db: 0.0,
            ram: None,
        }
    }

    #[test]
    fn roundtrip_settings_and_libraries() {
        let base = temp_base("roundtrip");
        let settings = Settings {
            active_library: 2,
            master_gain_db: 3.5,
            animate_ui: false,
            loop_enabled: true,
            loop_path: Some("C:\\loops\\rain.flac".to_string()),
            loop_volume: 0.42,
            loop_on_monitor: false,
            loop_on_cable: true,
            minimize_to_tray: true,
            show_gain_slider: false,
            ..Settings::default()
        };

        let libraries = vec![
            LibraryConfig {
                id: 1,
                name: "Main".into(),
                sounds: vec![sound(1, "airhorn")],
            },
            LibraryConfig {
                id: 2,
                name: "Voices".into(),
                sounds: vec![SoundConfig {
                    gain_db: -6.5,
                    ..sound(2, "hello")
                }],
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
        assert!((loaded.libraries[1].sounds[0].gain_db + 6.5).abs() < 1e-6);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    }

    /// The fingerprint and the write share one serializer; its bytes must stay
    /// identical to the owned `LibrariesFile` form readers already expect.
    #[test]
    fn libraries_serialize_to_the_owned_file_form() {
        let libraries = vec![LibraryConfig {
            id: 3,
            name: "Voices".into(),
            sounds: vec![sound(9, "hello")],
        }];
        let owned = LibrariesFile {
            version: LIBRARIES_VERSION,
            libraries: libraries.clone(),
        };
        assert_eq!(
            serialize_libraries(&libraries).unwrap(),
            serde_json::to_string_pretty(&owned).unwrap()
        );
    }

    #[test]
    fn loop_outputs_default_to_on_for_old_files() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.loop_on_monitor);
        assert!(settings.loop_on_cable);
        assert!(!settings.minimize_to_tray);
        assert!(settings.show_gain_slider);
        assert!(
            !settings.cycle_tabs_ctrl_tab,
            "Ctrl+Tab stays opt-in for old files"
        );
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
        let settings = Settings {
            master_gain_db: 1.0,
            ..Settings::default()
        };
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

    /// The trim is a sound field like any other: defaulted on old files, clamped
    /// on load, and written back through the same struct serde already handles.
    #[test]
    fn a_sound_trim_defaults_to_unity_and_clamps_on_load() {
        let old: SoundConfig = serde_json::from_str(
            "{\"id\": 1, \"name\": \"old\", \"path\": \"a.wav\", \"volume\": 0.5}",
        )
        .unwrap();
        assert_eq!(old.gain_db, 0.0, "a file from before the feature is unity");

        let mut file = LibrariesFile {
            version: LIBRARIES_VERSION,
            libraries: vec![LibraryConfig {
                id: 1,
                name: "Main".into(),
                sounds: vec![
                    SoundConfig {
                        gain_db: 30.0,
                        ..sound(1, "loud")
                    },
                    SoundConfig {
                        gain_db: -90.0,
                        ..sound(2, "quiet")
                    },
                    SoundConfig {
                        gain_db: -20.0,
                        ..sound(3, "old-floor")
                    },
                    SoundConfig {
                        gain_db: f32::NAN,
                        ..sound(4, "nan")
                    },
                    SoundConfig {
                        gain_db: -7.5,
                        ..sound(5, "kept")
                    },
                ],
            }],
        };
        file.normalize();

        let sounds = &file.libraries[0].sounds;
        assert_eq!(sounds[0].gain_db, 12.0);
        assert_eq!(sounds[1].gain_db, -12.0, "the floor is -12 dB now");
        assert_eq!(
            sounds[2].gain_db, -12.0,
            "a trim stored under the old -24 dB floor loads at the new one"
        );
        assert_eq!(sounds[3].gain_db, 0.0, "a hand-edited NaN becomes unity");
        assert_eq!(sounds[4].gain_db, -7.5, "an in-range trim survives");
    }

    #[test]
    fn generated_sound_ids_should_start_at_one() {
        // Ids are generated in document order, so the first missing id is the
        // first slot: an empty board must not start at 2.
        let mut file = LibrariesFile::default();
        file.libraries[0].sounds.push(SoundConfig::default());
        file.libraries[0].sounds.push(SoundConfig::default());
        file.normalize();

        assert_eq!(file.libraries[0].sounds[0].id, 1);
        assert_eq!(file.libraries[0].sounds[1].id, 2);
    }

    #[test]
    fn generated_sound_ids_should_stay_above_existing_ones() {
        let mut file = LibrariesFile {
            version: LIBRARIES_VERSION,
            libraries: vec![
                LibraryConfig {
                    id: 1,
                    name: "Main".into(),
                    sounds: vec![sound(4, "four"), SoundConfig::default()],
                },
                LibraryConfig {
                    id: 2,
                    name: "Other".into(),
                    sounds: vec![SoundConfig {
                        id: 4,
                        name: "dup".into(),
                        ..Default::default()
                    }],
                },
            ],
        };
        file.normalize();

        assert_eq!(file.libraries[0].sounds[0].id, 4);
        assert_eq!(file.libraries[0].sounds[1].id, 5);
        assert_eq!(file.libraries[1].sounds[0].id, 6);
    }

    #[test]
    fn generated_library_ids_should_saturate_on_a_hostile_max_id() {
        // `u64::MAX` from a hand-edited file used to overflow `id + 1` (a debug
        // panic) and to spin the "next free id" search.
        let mut file = LibrariesFile {
            version: LIBRARIES_VERSION,
            libraries: vec![
                LibraryConfig {
                    id: u64::MAX,
                    name: "Max".into(),
                    sounds: Vec::new(),
                },
                LibraryConfig {
                    id: 0,
                    name: "Zero".into(),
                    sounds: Vec::new(),
                },
                LibraryConfig {
                    id: u64::MAX,
                    name: "Dup".into(),
                    sounds: Vec::new(),
                },
            ],
        };
        file.normalize();

        assert_eq!(file.libraries[0].id, u64::MAX);
        assert_eq!(
            file.libraries[1].id, 1,
            "the search falls back to the low ids"
        );
        assert_eq!(file.libraries[2].id, 2);
    }

    #[test]
    fn hostile_sound_ids_should_normalize_unique_when_counter_saturates() {
        // A hand-edited file with `u64::MAX` plus duplicates and zeros: the old
        // `next_sound += 1` assigned a duplicate and then overflowed (debug
        // panic on user data). Ids must come out unique and never wrap.
        let mut file = LibrariesFile {
            version: LIBRARIES_VERSION,
            libraries: vec![
                LibraryConfig {
                    id: 1,
                    name: "Main".into(),
                    sounds: vec![
                        sound(u64::MAX, "max"),
                        SoundConfig::default(),
                        sound(u64::MAX, "dup"),
                    ],
                },
                LibraryConfig {
                    id: 2,
                    name: "Other".into(),
                    sounds: vec![SoundConfig::default(), sound(1, "one")],
                },
            ],
        };
        file.normalize();

        let ids: Vec<u64> = file
            .libraries
            .iter()
            .flat_map(|library| library.sounds.iter().map(|sound| sound.id))
            .collect();
        assert_eq!(ids.len(), 5);
        assert_eq!(ids[0], u64::MAX, "existing ids are preserved");
        assert!(!ids.contains(&0), "no sound keeps the reserved zero id");
        let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "ids collide: {ids:?}");
        assert!(
            ids.iter().filter(|id| **id != u64::MAX).all(|id| *id < 8),
            "the saturated counter falls back to the low ids: {ids:?}"
        );
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

    /// Every field away from its default, so a reset has something to change.
    fn non_default_settings() -> Settings {
        Settings {
            version: SETTINGS_VERSION,
            active_library: 7,
            theme: "mocha".into(),
            layout: Layout::List,
            monitor_device: Some("mon-a".into()),
            cable_device: Some("cable-a".into()),
            monitor_name: Some("Headphones".into()),
            cable_name: Some("Cable".into()),
            monitor_volume: 0.5,
            cable_volume: 0.4,
            master_gain_db: 3.0,
            link_volumes: false,
            preload_hotkey_sounds: false,
            loop_enabled: true,
            loop_path: Some("audio/rain.flac".into()),
            loop_volume: 0.6,
            loop_on_monitor: false,
            loop_on_cable: false,
            animate_ui: false,
            minimize_to_tray: true,
            show_gain_slider: false,
            cycle_tabs_ctrl_tab: true,
            show_ram_state: true,
            stop_all_hotkey_enabled: true,
            stop_all_hotkey: Some("Ctrl+Alt+S".into()),
        }
    }

    /// Fields with no settings page: they live on the board (devices, volumes,
    /// layout, active tab) or on the loop card, and only "Reset all settings"
    /// restores them.
    const BOARD_OWNED: &[&str] = &[
        "version",
        "active_library",
        "layout",
        "monitor_device",
        "cable_device",
        "monitor_name",
        "cable_name",
        "monitor_volume",
        "cable_volume",
        "master_gain_db",
        "link_volumes",
        "loop_volume",
    ];

    /// Fields with a control on a Settings page. Reset-all is the only reset now,
    /// so this list is the drift guard's other half: a field added to `Settings`
    /// must either get a row here or be board-owned.
    const SETTINGS_PAGE_FIELDS: &[&str] = &[
        "theme",
        "preload_hotkey_sounds",
        "loop_enabled",
        "loop_path",
        "loop_on_monitor",
        "loop_on_cable",
        "animate_ui",
        "minimize_to_tray",
        "show_gain_slider",
        "cycle_tabs_ctrl_tab",
        "show_ram_state",
        "stop_all_hotkey_enabled",
        "stop_all_hotkey",
    ];

    /// Drift guard: a settings field must be shown on a Settings page or be
    /// board-owned. A new field added to `Settings` and forgotten in both fails
    /// here with its own name; a stale name in either list fails too.
    #[test]
    fn every_settings_field_is_owned_by_a_control_or_the_board() {
        let all_keys: HashSet<String> = serde_json::from_str::<serde_json::Value>(
            &serialize_settings(&Settings::default()).unwrap(),
        )
        .unwrap()
        .as_object()
        .expect("settings serialize to an object")
        .keys()
        .cloned()
        .collect();

        let mut covered: HashSet<String> = HashSet::new();
        for key in BOARD_OWNED.iter().chain(SETTINGS_PAGE_FIELDS) {
            assert!(
                all_keys.contains(*key),
                "{key} is classified but is not a Settings field any more"
            );
            covered.insert((*key).to_string());
        }

        let missing: Vec<&String> = all_keys.difference(&covered).collect();
        assert!(
            missing.is_empty(),
            "settings fields have no Settings control and are not board-owned: {missing:?}"
        );
    }

    #[test]
    fn reset_all_should_equal_the_defaults() {
        let mut settings = non_default_settings();
        settings.reset_all();

        let mut expected = Settings::default();
        expected.normalize();
        assert_eq!(
            serialize_settings(&settings).unwrap(),
            serialize_settings(&expected).unwrap()
        );

        // The libraries file is a separate value: a settings reset has no path
        // to it, so its bytes stay exactly as they were (SPEC FR-5.9.12).
        let libraries = vec![LibraryConfig {
            id: 9,
            name: "Keep".into(),
            sounds: vec![sound(9, "nine")],
        }];
        let before = serialize_libraries(&libraries).unwrap();
        settings.reset_all();
        assert_eq!(serialize_libraries(&libraries).unwrap(), before);
    }

    #[test]
    fn active_library_clamps_to_an_existing_tab() {
        let mut settings = Settings {
            active_library: 99,
            ..Settings::default()
        };
        settings.clamp_active_library(&[4, 5]);
        assert_eq!(settings.active_library, 4);
        // A tab that exists is left alone, and an empty board keeps the value.
        settings.active_library = 5;
        settings.clamp_active_library(&[4, 5]);
        assert_eq!(settings.active_library, 5);
        settings.clamp_active_library(&[]);
        assert_eq!(settings.active_library, 5);
    }
}
