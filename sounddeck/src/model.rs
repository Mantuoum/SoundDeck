use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "SoundDeck";
pub const WINDOW_TITLE: &str = "SoundDeck - Soundboard";
pub const CONFIG_FILE: &str = "sounddeck.json";
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "ogg", "flac", "m4a", "aac", "opus", "wma", "aif", "aiff",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppTheme {
    #[default]
    Macchiato,
    Mocha,
    Frappe,
    Latte,
    Graphite,
}

impl AppTheme {
    pub const ALL: [Self; 5] = [
        Self::Macchiato,
        Self::Mocha,
        Self::Frappe,
        Self::Latte,
        Self::Graphite,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Macchiato => "Catppuccin Macchiato",
            Self::Mocha => "Catppuccin Mocha",
            Self::Frappe => "Catppuccin Frappe",
            Self::Latte => "Catppuccin Latte",
            Self::Graphite => "Graphite",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    Grid,
    List,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub hotkey: Option<String>,
    pub volume: f32,
    /// Desired RAM state for sounds that have a hotkey. `None` on old configs
    /// means "inherit the global preload setting".
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
pub struct Config {
    pub version: u32,
    pub sounds: Vec<SoundConfig>,
    pub monitor_device: Option<String>,
    pub cable_device: Option<String>,
    pub monitor_name: Option<String>,
    pub cable_name: Option<String>,
    pub monitor_volume: f32,
    pub cable_volume: f32,
    pub master_gain_db: f32,
    pub link_volumes: bool,
    pub preload_hotkey_sounds: bool,
    pub theme: AppTheme,
    pub layout: Layout,
    pub close_to_tray: bool,
    pub window_size: Option<[f32; 2]>,
    pub next_id: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            sounds: Vec::new(),
            monitor_device: None,
            cable_device: None,
            monitor_name: None,
            cable_name: None,
            monitor_volume: 1.0,
            cable_volume: 1.0,
            master_gain_db: 0.0,
            link_volumes: true,
            preload_hotkey_sounds: true,
            theme: AppTheme::Macchiato,
            layout: Layout::Grid,
            close_to_tray: true,
            window_size: None,
            next_id: 1,
        }
    }
}

impl Config {
    pub fn normalize(&mut self) {
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let max_id = self.sounds.iter().map(|s| s.id).max().unwrap_or(0);
        if self.next_id <= max_id {
            self.next_id = max_id + 1;
        }
        self.monitor_volume = self.monitor_volume.clamp(0.0, 1.0);
        self.cable_volume = self.cable_volume.clamp(0.0, 1.0);
        self.master_gain_db = self.master_gain_db.clamp(-60.0, 12.0);
        for sound in &mut self.sounds {
            sound.volume = sound.volume.clamp(0.0, 2.0);
        }
    }
}

/// Directory the app lives in. Everything (config) is stored here: fully portable.
pub fn app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    app_dir().join(CONFIG_FILE)
}

/// Store paths inside the app folder as relative, so the whole folder can be moved.
pub fn portable_path(path: &Path) -> String {
    let base = app_dir();
    if let (Ok(base), Ok(full)) = (base.canonicalize(), path.canonicalize()) {
        if let Ok(rel) = full.strip_prefix(&base) {
            return rel.to_string_lossy().to_string();
        }
    }
    path.to_string_lossy().to_string()
}

pub fn resolve_path(stored: &str) -> PathBuf {
    let path = PathBuf::from(stored);
    if path.is_absolute() {
        path
    } else {
        app_dir().join(path)
    }
}

pub struct LoadOutcome {
    pub config: Config,
    pub warning: Option<String>,
}

pub fn load_config(path: &Path) -> LoadOutcome {
    let mut warning = None;
    let existed = path.exists();
    let config = if existed {
        match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(mut cfg) => {
                    cfg.normalize();
                    cfg
                }
                Err(err) => {
                    let backup = path.with_extension("json.bak");
                    let _ = fs::copy(path, &backup);
                    warning = Some(format!(
                        "Config was unreadable ({err}). Started fresh; old file saved as {}.",
                        backup
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "sounddeck.json.bak".into())
                    ));
                    Config::default()
                }
            },
            Err(err) => {
                warning = Some(format!("Could not read config: {err}"));
                Config::default()
            }
        }
    } else {
        Config::default()
    };

    LoadOutcome {
        config,
        warning,
    }
}

pub fn save_config(path: &Path, config: &Config) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(config)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    // Windows: rename over an existing file works with fs::rename.
    if let Err(first) = fs::rename(&tmp, path) {
        // Anti-virus / indexer may briefly hold the file; retry once via copy.
        let _ = fs::copy(&tmp, path);
        let _ = fs::remove_file(&tmp);
        if !path.exists() {
            return Err(first.into());
        }
    }
    Ok(())
}

pub fn is_supported_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            SUPPORTED_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

pub fn display_name(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Untitled sound".to_string())
}

pub fn format_duration(seconds: f32) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00".to_string();
    }
    let total = seconds.round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

pub fn format_bytes(bytes: usize) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 100.0 {
        format!("{mb:.0} MB")
    } else if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_config() {
        let mut cfg = Config::default();
        cfg.sounds.push(SoundConfig {
            id: 7,
            name: "Airhorn".into(),
            path: "sounds/airhorn.mp3".into(),
            hotkey: Some("Alt+1".into()),
            volume: 0.8,
            ram: Some(true),
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sounds.len(), 1);
        assert_eq!(back.sounds[0].name, "Airhorn");
    }

    #[test]
    fn normalizes_ids() {
        let mut cfg = Config::default();
        cfg.sounds.push(SoundConfig {
            id: 99,
            ..Default::default()
        });
        cfg.normalize();
        assert_eq!(cfg.next_id, 100);
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(0.0), "0:00");
        assert_eq!(format_duration(3.2), "0:03");
        assert_eq!(format_duration(75.0), "1:15");
    }
}
