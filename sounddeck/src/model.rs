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

/// What "Show in Explorer" hands the shell for one sound path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExplorerTarget {
    /// Select this file in its folder.
    File(PathBuf),
    /// The file is gone: open the folder that should hold it.
    Folder(PathBuf),
    /// Nothing to show: the file is gone and the path names no folder.
    Nothing,
}

/// The file when it is there, otherwise its folder (B3). The shell call takes
/// this as a *path*, never as part of a command line, so spaces, commas,
/// unicode and names past `MAX_PATH` need no quoting rules.
pub fn explorer_target(path: &Path, exists: bool) -> ExplorerTarget {
    if exists {
        return ExplorerTarget::File(path.to_path_buf());
    }
    match path.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => {
            ExplorerTarget::Folder(folder.to_path_buf())
        }
        _ => ExplorerTarget::Nothing,
    }
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

    /// The path reaches the shell verbatim: nothing about it is ever
    /// re-quoted or re-parsed, so an awkward name is not an awkward argument.
    #[test]
    fn explorer_target_should_keep_an_awkward_file_name_verbatim() {
        let names = [
            // Long name: 70-character stem, the case the /select, form broke on.
            format!("C:\\Music\\{}.wav", "a".repeat(70)),
            "C:\\Music\\my sound, take 2.wav".to_string(),
            "C:\\Music\\klang \u{1f514} \u{fc}n\u{ef}code.wav".to_string(),
        ];
        for path in names {
            let target = explorer_target(Path::new(&path), true);
            assert_eq!(
                target,
                ExplorerTarget::File(PathBuf::from(&path)),
                "{path} must reach the shell unchanged"
            );
        }
    }

    #[test]
    fn explorer_target_should_open_the_folder_when_the_file_is_gone() {
        assert_eq!(
            explorer_target(Path::new("C:\\Music\\gone sound.wav"), false),
            ExplorerTarget::Folder(PathBuf::from("C:\\Music"))
        );
        // A bare file name has no folder to open: say so instead of opening
        // the current directory.
        assert_eq!(
            explorer_target(Path::new("gone.wav"), false),
            ExplorerTarget::Nothing
        );
    }
}
