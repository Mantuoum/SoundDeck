use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "SoundDeck";
pub const WINDOW_TITLE: &str = "SoundDeck - Soundboard";
/// Legacy single-file config, imported once into `config/`.
pub const CONFIG_FILE: &str = "sounddeck.json";
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "ogg", "flac", "m4a", "aac", "opus", "wma", "aif", "aiff",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    Grid,
    List,
}

/// Directory the app lives in. Everything (config) is stored here: fully portable.
pub fn app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    app_dir().join("config")
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
    fn duration_formatting() {
        assert_eq!(format_duration(0.0), "0:00");
        assert_eq!(format_duration(3.2), "0:03");
        assert_eq!(format_duration(75.0), "1:15");
    }
}
