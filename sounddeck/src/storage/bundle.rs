//! One-file configuration bundle: `settings` + `libraries` in a single JSON
//! document, with a kind and a version stamp so an import can refuse anything
//! that is not ours or is from a newer build.
//!
//! The bundle is not a second storage format: its two members are exactly the
//! shapes of `settings.json` and `libraries.json` (same structs, same single
//! serializer), so export → import restores the board a plain file copy would.
//! Audio files are references and are never copied into the bundle (FR-5.3.14).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::io::strip_bom;
use super::{fix_active_library, LibrariesFile, LibrariesRef, LibraryConfig, Settings};

/// Stamped into every bundle; a document without it is not a configuration.
pub const BUNDLE_KIND: &str = "sounddeck";
/// Bundle layout version. A higher number means a newer build wrote it.
pub const BUNDLE_VERSION: u32 = 1;

/// What an imported bundle carries, already normalized.
pub struct Bundle {
    pub settings: Settings,
    pub libraries: Vec<LibraryConfig>,
}

/// Why a bundle was refused. One owner for the wording of every rejection.
#[derive(Debug, PartialEq, Eq)]
pub enum BundleError {
    /// The file could not be read at all (missing, locked, not text).
    Read(String),
    /// Not JSON, or JSON that is not a bundle.
    Parse(String),
    /// A JSON document from something else (`app` missing or different).
    Foreign,
    /// Written by a newer SoundDeck.
    Newer(u32),
}

impl BundleError {
    /// The one-line message the activity log shows.
    pub fn message(&self) -> String {
        match self {
            Self::Read(err) => format!("Could not read the file: {err}"),
            Self::Parse(err) => format!("Not a SoundDeck configuration file: {err}"),
            Self::Foreign => "Not a SoundDeck configuration file.".to_string(),
            Self::Newer(version) => format!(
                "That configuration is from a newer SoundDeck (bundle {version}); \
                 update SoundDeck to import it."
            ),
        }
    }
}

/// The bundle as written: the config shapes plus the stamps. Borrowed, so an
/// export serializes the live settings without cloning the library list.
#[derive(Serialize)]
struct BundleOut<'a> {
    app: &'static str,
    bundle: u32,
    settings: &'a Settings,
    libraries: LibrariesRef<'a>,
}

/// The bundle as read. `settings`/`libraries` are required: a document missing
/// either is not a bundle and must not silently import as an empty board.
#[derive(Deserialize)]
struct BundleIn {
    #[serde(default)]
    app: String,
    #[serde(default)]
    bundle: u32,
    settings: Settings,
    libraries: LibrariesFile,
}

/// Serialize the whole configuration as one bundle.
pub fn to_json(settings: &Settings, libraries: &[LibraryConfig]) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&BundleOut {
        app: BUNDLE_KIND,
        bundle: BUNDLE_VERSION,
        settings,
        libraries: LibrariesRef::new(libraries),
    })?)
}

/// Read, parse and validate a bundle file.
pub fn read(path: &Path) -> Result<Bundle, BundleError> {
    let text = std::fs::read_to_string(path).map_err(|err| BundleError::Read(err.to_string()))?;
    parse(strip_bom(&text))
}

/// Parse and validate bundle text. Pure: no I/O, so the reject paths are cheap
/// to test.
pub fn parse(text: &str) -> Result<Bundle, BundleError> {
    let bundle: BundleIn =
        serde_json::from_str(text).map_err(|err| BundleError::Parse(err.to_string()))?;
    if bundle.app != BUNDLE_KIND {
        return Err(BundleError::Foreign);
    }
    if bundle.bundle == 0 {
        return Err(BundleError::Parse(
            "the bundle version is missing".to_string(),
        ));
    }
    if bundle.bundle > BUNDLE_VERSION {
        return Err(BundleError::Newer(bundle.bundle));
    }

    let BundleIn {
        mut settings,
        mut libraries,
        ..
    } = bundle;
    // Same normalization as a config load: a bundle is not trusted more than a
    // hand-edited file.
    settings.normalize();
    libraries.normalize();
    fix_active_library(&mut settings, &libraries.libraries);

    Ok(Bundle {
        settings,
        libraries: libraries.libraries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SoundConfig, LIBRARIES_VERSION};

    fn sound(id: u64, name: &str) -> SoundConfig {
        SoundConfig {
            id,
            name: name.into(),
            path: format!("audio/{name}.wav"),
            hotkey: Some("Ctrl+1".into()),
            volume: 0.8,
            gain_db: 0.0,
            ram: Some(true),
        }
    }

    fn board() -> Vec<LibraryConfig> {
        vec![
            LibraryConfig {
                id: 1,
                name: "Main".into(),
                sounds: vec![sound(1, "airhorn")],
            },
            LibraryConfig {
                id: 2,
                name: "Voices".into(),
                sounds: vec![SoundConfig {
                    gain_db: 3.5,
                    ..sound(2, "hello")
                }],
            },
        ]
    }

    #[test]
    fn a_bundle_round_trip_restores_settings_and_libraries() {
        let settings = Settings {
            active_library: 2,
            theme: "mocha".into(),
            master_gain_db: 4.5,
            loop_enabled: true,
            loop_path: Some("audio/rain.flac".into()),
            loop_volume: 0.3,
            minimize_to_tray: true,
            ..Settings::default()
        };
        let libraries = board();

        let text = to_json(&settings, &libraries).expect("serialize bundle");
        let bundle = parse(&text).expect("parse bundle");

        // The members are the config shapes, byte for byte.
        assert!(text.contains("\"app\": \"sounddeck\""));
        assert!(text.contains(&format!("\"bundle\": {BUNDLE_VERSION}")));
        assert_eq!(
            serde_json::to_string_pretty(&LibrariesFile {
                version: LIBRARIES_VERSION,
                libraries: bundle.libraries.clone(),
            })
            .unwrap(),
            crate::storage::serialize_libraries(&libraries).unwrap(),
            "the bundle's libraries member is the libraries file"
        );

        assert_eq!(bundle.settings.theme, "mocha");
        assert_eq!(bundle.settings.active_library, 2);
        assert!((bundle.settings.master_gain_db - 4.5).abs() < 1e-6);
        assert!(bundle.settings.loop_enabled);
        assert_eq!(
            bundle.settings.loop_path.as_deref(),
            Some("audio/rain.flac")
        );
        assert!(bundle.settings.minimize_to_tray);
        assert_eq!(bundle.libraries.len(), 2);
        assert_eq!(bundle.libraries[1].sounds[0].name, "hello");
        assert!((bundle.libraries[1].sounds[0].gain_db - 3.5).abs() < 1e-6);
    }

    #[test]
    fn a_corrupt_bundle_is_rejected() {
        assert!(matches!(parse("{ not json"), Err(BundleError::Parse(_))));
        // JSON, but not a bundle: no settings/libraries members.
        assert!(matches!(
            parse("{\"hello\": 1}"),
            Err(BundleError::Parse(_))
        ));

        // A JSON document from another program.
        let foreign = format!(
            "{{\"app\": \"something-else\", \"bundle\": {BUNDLE_VERSION}, \
              \"settings\": {{}}, \"libraries\": {{\"libraries\": []}}}}"
        );
        assert_eq!(parse(&foreign).err(), Some(BundleError::Foreign));

        // No stamp at all.
        let unstamped = "{\"settings\": {}, \"libraries\": {\"libraries\": []}}";
        assert_eq!(parse(unstamped).err(), Some(BundleError::Foreign));
    }

    #[test]
    fn a_newer_bundle_is_rejected_with_its_version() {
        let newer = format!(
            "{{\"app\": \"sounddeck\", \"bundle\": {}, \"settings\": {{}}, \
              \"libraries\": {{\"libraries\": []}}}}",
            BUNDLE_VERSION + 1
        );
        assert_eq!(
            parse(&newer).err(),
            Some(BundleError::Newer(BUNDLE_VERSION + 1))
        );
        assert!(BundleError::Newer(BUNDLE_VERSION + 1)
            .message()
            .contains(&(BUNDLE_VERSION + 1).to_string()));
    }

    #[test]
    fn an_imported_board_normalizes_its_tab_and_values() {
        // Library 99 does not exist and the gain is out of range; both are
        // corrected exactly like a config load.
        let text = format!(
            "{{\"app\": \"sounddeck\", \"bundle\": {BUNDLE_VERSION}, \
              \"settings\": {{\"active_library\": 99, \"master_gain_db\": 40.0}}, \
              \"libraries\": {{\"version\": 2, \"libraries\": [\
                {{\"id\": 4, \"name\": \"Only\", \"sounds\": []}}]}}}}"
        );
        let bundle = parse(&text).expect("valid bundle");
        assert_eq!(bundle.settings.active_library, 4);
        assert_eq!(bundle.settings.master_gain_db, 12.0);
        assert_eq!(bundle.libraries.len(), 1);
    }

    #[test]
    fn reading_a_missing_file_reports_the_error() {
        let path = crate::storage::test_dir("bundle-missing").join("nope.json");
        assert!(matches!(read(&path), Err(BundleError::Read(_))));
    }
}
