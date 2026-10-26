//! File-level helpers for the portable config folder.
//!
//! Every JSON file is written atomically and keeps exactly one rolling backup:
//! `name.json` -> `name.json.bak`, with `name.json.tmp` transient. A failed read
//! moves the bad file aside as `name.json.corrupt`, then tries `.tmp`, `.bak`
//! and finally caller defaults. Nothing accumulates over time.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

pub fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.json".to_string());
    path.with_file_name(format!("{name}.{suffix}"))
}

fn read_text(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    // Some editors prepend a UTF-8 BOM; serde_json rejects it.
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

fn parse<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = read_text(path).ok_or_else(|| "missing".to_string())?;
    serde_json::from_str(&text).map_err(|err| err.to_string())
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Load a JSON file, recovering from a leftover temp file or the rolling backup.
pub fn load_json<T: DeserializeOwned + Default>(
    path: &Path,
    label: &str,
    warnings: &mut Vec<String>,
) -> T {
    let tmp = sidecar(path, "tmp");
    let bak = sidecar(path, "bak");
    let corrupt = sidecar(path, "corrupt");

    let existed = path.exists() || tmp.exists() || bak.exists();

    if path.exists() {
        match parse::<T>(path) {
            Ok(value) => {
                let _ = fs::remove_file(&tmp);
                return value;
            }
            Err(err) => {
                let _ = fs::rename(path, &corrupt);
                warnings.push(format!(
                    "{label} was unreadable ({err}). Kept as {} and trying a backup.",
                    file_label(&corrupt)
                ));
            }
        }
    }

    if let Ok(value) = parse::<T>(&tmp) {
        match fs::rename(&tmp, path) {
            Ok(()) => warnings.push(format!("{label} recovered from an interrupted save.")),
            Err(_) => {
                // Leave the temp file in place; it is still valid if we need it.
                return value;
            }
        }
        return value;
    }

    if let Ok(value) = parse::<T>(&bak) {
        let restored = fs::copy(&bak, path).is_ok();
        warnings.push(if restored {
            format!("{label} restored from {}.", file_label(&bak))
        } else {
            format!(
                "{label} loaded from {}, but restoring it failed.",
                file_label(&bak)
            )
        });
        return value;
    }

    if existed {
        warnings.push(format!("{label} could not be recovered; using defaults."));
    }
    T::default()
}

/// Atomic write with a rolling single backup.
pub fn write_json_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp = sidecar(path, "tmp");
    let bak = sidecar(path, "bak");
    fs::write(&tmp, text)?;

    if path.exists() {
        // If the rotation fails (AV/indexer lock), keep going: the new save is
        // more important than the backup.
        let _ = fs::rename(path, &bak);
    }

    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(first) => {
            // Windows sometimes holds the target briefly. Retry via copy.
            match fs::copy(&tmp, path) {
                Ok(_) => {
                    let _ = fs::remove_file(&tmp);
                    Ok(())
                }
                Err(second) => Err(anyhow::anyhow!(
                    "writing {}: rename failed ({first}); copy failed ({second})",
                    file_label(path)
                )),
            }
        }
    }
}

/// Remove leftover temp files after a successful load.
pub fn sweep_tmp(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .map(|name| name.to_string_lossy().ends_with(".tmp"))
            .unwrap_or(false)
        {
            let _ = fs::remove_file(path);
        }
    }
}
