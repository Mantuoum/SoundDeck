//! File-level helpers for the portable config folder.
//!
//! Every JSON file is written atomically and keeps exactly one rolling backup:
//! `name.json` -> `name.json.bak`, with `name.json.tmp` transient. A failed read
//! moves the bad file aside as `name.json.corrupt`, then tries `.tmp`, `.bak`
//! and finally caller defaults. A taken snapshot name gets a numbered suffix
//! (`name.json.corrupt.2`, …) so an earlier snapshot is never overwritten.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// Highest numbered suffix `free_path` tries before giving up.
const MAX_SIDECAR_SUFFIX: u32 = 99;

pub fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.json".to_string());
    path.with_file_name(format!("{name}.{suffix}"))
}

/// `preferred` when nothing is there, otherwise `preferred` with a numbered
/// suffix (`.2`, `.3`, …), or `None` when every candidate is taken.
///
/// Used for sidecars that must never clobber an earlier one: a second corrupt
/// read or a second legacy import keeps the previous snapshot intact.
pub fn free_path(preferred: &Path) -> Option<PathBuf> {
    if !preferred.exists() {
        return Some(preferred.to_path_buf());
    }
    let name = preferred
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| file_label(preferred));
    (2..=MAX_SIDECAR_SUFFIX)
        .map(|index| preferred.with_file_name(format!("{name}.{index}")))
        .find(|candidate| !candidate.exists())
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

/// Label used in warnings; a bare file name even for an absolute path.
pub(crate) fn file_label(path: &Path) -> String {
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

    let existed = path.exists() || tmp.exists() || bak.exists();

    if path.exists() {
        match parse::<T>(path) {
            Ok(value) => {
                let _ = fs::remove_file(&tmp);
                return value;
            }
            Err(err) => {
                // Keep every snapshot: a `.corrupt` name that is already taken
                // gets a numbered suffix instead of being overwritten.
                let kept = match free_path(&sidecar(path, "corrupt")) {
                    Some(corrupt) => {
                        let _ = fs::rename(path, &corrupt);
                        format!("Kept as {}.", file_label(&corrupt))
                    }
                    None => "It was left in place: no free snapshot name.".to_string(),
                };
                warnings.push(format!(
                    "{label} was unreadable ({err}). {kept} Trying a backup."
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_dir;

    #[test]
    fn corrupt_snapshot_should_not_clobber_an_earlier_one() {
        let dir = test_dir("io-corrupt-twice");
        let path = dir.join("settings.json");
        let mut warnings = Vec::new();

        fs::write(&path, "{ broken one").unwrap();
        let _: serde_json::Value = load_json(&path, "settings.json", &mut warnings);
        fs::write(&path, "{ broken two").unwrap();
        let _: serde_json::Value = load_json(&path, "settings.json", &mut warnings);

        let first = sidecar(&path, "corrupt");
        let second = path.with_file_name("settings.json.corrupt.2");
        assert_eq!(fs::read_to_string(&first).unwrap(), "{ broken one");
        assert_eq!(fs::read_to_string(&second).unwrap(), "{ broken two");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("settings.json.corrupt.2")),
            "{warnings:?}"
        );
    }

    #[test]
    fn free_path_should_prefer_the_plain_name_then_number_it() {
        let dir = test_dir("io-free-path");
        let preferred = dir.join("libraries.json.corrupt");

        assert_eq!(free_path(&preferred).as_deref(), Some(preferred.as_path()));

        fs::write(&preferred, "first").unwrap();
        let numbered = free_path(&preferred).unwrap();
        assert_eq!(
            numbered.file_name().unwrap().to_string_lossy(),
            "libraries.json.corrupt.2"
        );
    }
}
