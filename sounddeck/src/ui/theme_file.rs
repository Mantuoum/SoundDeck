//! JSON theme files: `config/themes/<id>.json`.
//!
//! One pretty-printed object per theme, so a user can copy a file and edit
//! colours in a text editor:
//!
//! ```json
//! {
//!   "name": "Mocha",
//!   "light": false,
//!   "bg": "#1E1E2E",
//!   "panel": "#181825"
//! }
//! ```
//!
//! Missing or invalid tokens keep the Graphite fallback value, so a half-edited
//! theme never breaks the app; a file that is not JSON at all is skipped with a
//! warning and the other themes still load.
//!
//! Token names live in exactly one place: [`COLOUR_TOKENS`]. The parser reads
//! them, [`template`] writes the shipped files from them, and the tests prove
//! every `Palette` colour field is reachable — adding a field without a token
//! fails the build (struct literal) or the test (round-trip).
//!
//! [`parse_markdown`] is the pre-0.6 format. Nothing reads it as a live theme:
//! `theme::reload_from` calls it once per old file to convert `<id>.md` into
//! `<id>.json` (see `theme::migrate_markdown`).

use eframe::egui::Color32;
use serde::Deserialize;
use serde_json::Value;

use super::theme::Palette;
use super::theme_color::{blend, from_hex};
use super::theme_defaults::{
    ACCENT_SOFT_BLEND_DARK, ACCENT_SOFT_BLEND_GRAPHITE, ACCENT_SOFT_BLEND_LIGHT,
};

/// The one extension rule: a live theme is `<id>.json`.
pub(crate) const THEME_EXTENSION: &str = "json";

/// The pre-0.6 extension; `theme::migrate_markdown` converts these once.
pub(crate) const LEGACY_EXTENSION: &str = "md";

/// Suffix a converted Markdown file is renamed to — never deleted, and inert
/// because it is no longer a theme extension.
pub(crate) const MIGRATED_SUFFIX: &str = "md.migrated";

/// Boolean field (a JSON bool), not a colour like the rest.
const BOOL_TOKEN: &str = "light";

/// True when `path` ends in `extension`, case-insensitively (Windows names).
pub(crate) fn has_extension(path: &std::path::Path, extension: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(extension))
        .unwrap_or(false)
}

/// A `Palette` colour field a theme token can set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Colour {
    Bg,
    Panel,
    PanelDeep,
    Card,
    CardHover,
    Field,
    Border,
    BorderSoft,
    Text,
    Muted,
    Faint,
    Accent,
    AccentSoft,
    Purple,
    Amber,
    Red,
}

impl Colour {
    /// Mutable access to the backing field; the only place the mapping lives.
    fn slot(self, palette: &mut Palette) -> &mut Color32 {
        match self {
            Colour::Bg => &mut palette.bg,
            Colour::Panel => &mut palette.panel,
            Colour::PanelDeep => &mut palette.panel_deep,
            Colour::Card => &mut palette.card,
            Colour::CardHover => &mut palette.card_hover,
            Colour::Field => &mut palette.field,
            Colour::Border => &mut palette.border,
            Colour::BorderSoft => &mut palette.border_soft,
            Colour::Text => &mut palette.text,
            Colour::Muted => &mut palette.muted,
            Colour::Faint => &mut palette.faint,
            Colour::Accent => &mut palette.accent,
            Colour::AccentSoft => &mut palette.accent_soft,
            Colour::Purple => &mut palette.purple,
            Colour::Amber => &mut palette.amber,
            Colour::Red => &mut palette.red,
        }
    }

    fn get(self, palette: &Palette) -> Color32 {
        let mut copy = *palette;
        *self.slot(&mut copy)
    }

    fn set(self, palette: &mut Palette, color: Color32) {
        *self.slot(palette) = color;
    }
}

/// The theme token table: JSON key → palette field, in the order `template()`
/// writes them.
const COLOUR_TOKENS: &[(&str, Colour)] = &[
    ("bg", Colour::Bg),
    ("panel", Colour::Panel),
    ("panel_deep", Colour::PanelDeep),
    ("card", Colour::Card),
    ("card_hover", Colour::CardHover),
    ("field", Colour::Field),
    ("border", Colour::Border),
    ("border_soft", Colour::BorderSoft),
    ("text", Colour::Text),
    ("muted", Colour::Muted),
    ("faint", Colour::Faint),
    ("accent", Colour::Accent),
    ("accent_soft", Colour::AccentSoft),
    ("purple", Colour::Purple),
    ("amber", Colour::Amber),
    ("red", Colour::Red),
];

fn colour_token(key: &str) -> Option<Colour> {
    COLOUR_TOKENS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, colour)| *colour)
}

#[derive(Debug)]
pub struct ParsedTheme {
    pub name: String,
    pub palette: Palette,
    pub warnings: Vec<String>,
}

/// The on-disk object. The colour keys are deliberately **not** struct fields:
/// they are resolved through [`COLOUR_TOKENS`], so the token table stays the one
/// mapping between a file key and a `Palette` field.
#[derive(Deserialize)]
struct ThemeFile {
    /// Display name; a missing, empty or non-text value falls back to the id.
    #[serde(default)]
    name: Option<Value>,
    /// `true`/`yes`/`1` (or a JSON bool) for a light palette.
    #[serde(default)]
    light: Option<Value>,
    /// Every other key: the colour tokens, plus anything unknown, which is
    /// reported and ignored.
    #[serde(flatten)]
    rest: serde_json::Map<String, Value>,
}

/// `true`/`yes`/`1` — the pre-0.6 rule, kept so an old or hand-edited flag
/// means the same thing in either format.
fn truthy(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "1"
    )
}

fn light_flag(value: &Value, warnings: &mut Vec<String>) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::String(text) => truthy(text),
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        other => {
            warnings.push(format!(
                "themes: \"{BOOL_TOKEN}\" must be true or false (got {other}); using false"
            ));
            false
        }
    }
}

fn theme_name(value: Option<&Value>, id: &str, warnings: &mut Vec<String>) -> String {
    match value {
        None | Some(Value::Null) => id.to_string(),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                id.to_string()
            } else {
                trimmed.to_string()
            }
        }
        Some(other) => {
            warnings.push(format!(
                "themes: \"name\" must be text (got {other}); using the file name"
            ));
            id.to_string()
        }
    }
}

/// Parse one theme file. `Err` carries the warning line for a file that is not
/// JSON at all — the caller skips that file and keeps loading the rest.
pub fn parse(id: &str, text: &str, fallback: &Palette) -> Result<ParsedTheme, String> {
    let file: ThemeFile = serde_json::from_str(text).map_err(|err| {
        format!("themes: \"{id}.{THEME_EXTENSION}\" is not valid JSON ({err}); skipped")
    })?;

    let mut palette = *fallback;
    let mut warnings = Vec::new();
    let mut saw_valid_token = false;
    let mut accent_soft_seen = false;

    if let Some(value) = &file.light {
        saw_valid_token = true;
        palette.is_light = light_flag(value, &mut warnings);
    }

    for (key, value) in &file.rest {
        let token = key.to_ascii_lowercase();
        let Some(colour) = colour_token(&token) else {
            warnings.push(format!("themes: unknown token \"{key}\" ignored"));
            continue;
        };
        saw_valid_token = true;
        if colour == Colour::AccentSoft {
            accent_soft_seen = true;
        }
        let text = value.as_str().map(str::trim);
        match text.and_then(from_hex) {
            Some(color) => colour.set(&mut palette, color),
            None => {
                colour.set(&mut palette, colour.get(fallback));
                let shown = match text {
                    Some(text) => format!("\"{text}\""),
                    None => value.to_string(),
                };
                warnings.push(format!(
                    "themes: \"{token}\" has an invalid colour {shown}; using Graphite"
                ));
            }
        }
    }

    if !saw_valid_token {
        warnings.push(format!(
            "themes: \"{id}.{THEME_EXTENSION}\" has no tokens; using Graphite"
        ));
    }

    // Derive the tinted accent surface unless the file provides it explicitly,
    // so the exact shipped values round-trip and partial edits stay consistent.
    if !accent_soft_seen {
        let ratio = if id.eq_ignore_ascii_case("graphite") {
            ACCENT_SOFT_BLEND_GRAPHITE
        } else if palette.is_light {
            ACCENT_SOFT_BLEND_LIGHT
        } else {
            ACCENT_SOFT_BLEND_DARK
        };
        palette.accent_soft = blend(palette.bg, palette.accent, ratio);
    }

    Ok(ParsedTheme {
        name: theme_name(file.name.as_ref(), id, &mut warnings),
        palette,
        warnings,
    })
}

fn hex(color: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
}

/// A JSON string literal. serde owns the escaping, so a theme name holding a
/// quote or a backslash cannot produce a file that will not parse back.
fn json_string(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// The shipped file for one palette: `name` and `light` first, then the colour
/// tokens in table order, two-space indented for hand-editing.
pub fn template(name: &str, palette: &Palette) -> String {
    let mut rows: Vec<String> = Vec::with_capacity(COLOUR_TOKENS.len() + 2);
    rows.push(format!("  \"name\": {}", json_string(name)));
    rows.push(format!("  \"{BOOL_TOKEN}\": {}", palette.is_light));
    for (token, colour) in COLOUR_TOKENS {
        rows.push(format!(
            "  \"{token}\": {}",
            json_string(&hex(colour.get(palette)))
        ));
    }
    format!("{{\n{}\n}}\n", rows.join(",\n"))
}

/// The pre-0.6 Markdown reader: a `# Heading` plus a `| token | value |` table.
///
/// Only `theme::migrate_markdown` calls this — the format is otherwise gone, and
/// no Markdown file is ever a live theme. It keeps the old semantics exactly,
/// because a migration has to reproduce what 0.5.0 would have shown.
pub fn parse_markdown(id: &str, text: &str, fallback: &Palette) -> ParsedTheme {
    let mut palette = *fallback;
    let mut name = None;
    let mut warnings = Vec::new();
    let mut saw_valid_token = false;
    let mut accent_soft_seen = false;

    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("# ") {
            if name.is_none() {
                let heading = rest.trim();
                if !heading.is_empty() {
                    name = Some(heading.to_string());
                }
            }
            continue;
        }
        if !line.starts_with('|') {
            continue;
        }

        let cells: Vec<&str> = line
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim())
            .collect();
        if cells.len() < 2 {
            continue;
        }
        let key = cells[0].trim_matches('`').trim().to_ascii_lowercase();
        let value = cells[1].trim_matches('`').trim();
        if key.is_empty() || key == "token" || key.chars().all(|ch| ch == '-' || ch == ':') {
            continue;
        }

        if key == BOOL_TOKEN {
            palette.is_light = truthy(value);
            saw_valid_token = true;
            continue;
        }

        let Some(colour) = colour_token(&key) else {
            warnings.push(format!("themes: unknown token \"{key}\" ignored"));
            continue;
        };
        saw_valid_token = true;
        if colour == Colour::AccentSoft {
            accent_soft_seen = true;
        }
        match from_hex(value) {
            Some(color) => colour.set(&mut palette, color),
            None => {
                colour.set(&mut palette, colour.get(fallback));
                warnings.push(format!(
                    "themes: \"{key}\" has an invalid colour \"{value}\"; using Graphite"
                ));
            }
        }
    }

    if !saw_valid_token {
        warnings.push(format!(
            "themes: \"{id}.{LEGACY_EXTENSION}\" has no tokens; using Graphite"
        ));
    }

    if !accent_soft_seen {
        let ratio = if id.eq_ignore_ascii_case("graphite") {
            ACCENT_SOFT_BLEND_GRAPHITE
        } else if palette.is_light {
            ACCENT_SOFT_BLEND_LIGHT
        } else {
            ACCENT_SOFT_BLEND_DARK
        };
        palette.accent_soft = blend(palette.bg, palette.accent, ratio);
    }

    ParsedTheme {
        name: name.unwrap_or_else(|| id.to_string()),
        palette,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme_defaults;

    /// One object literal with `bg` only — the shape every token test edits.
    fn theme_json(rows: &str) -> String {
        format!("{{\n{rows}\n}}\n")
    }

    #[test]
    fn template_roundtrips_graphite() {
        let fallback = theme_defaults::graphite();
        let text = template("Graphite", &fallback);
        let parsed = parse("graphite", &text, &fallback).expect("template parses");
        assert_eq!(parsed.name, "Graphite");
        assert_eq!(parsed.palette, fallback);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    /// One key per `Palette` colour field: adding a field breaks the sentinel
    /// literal (compile error), and the round-trip below then fails until a
    /// token row is added for it.
    #[test]
    fn every_palette_colour_field_has_a_token() {
        let sentinel = Palette {
            is_light: false,
            bg: Color32::from_rgb(0x01, 0x01, 0x01),
            panel: Color32::from_rgb(0x02, 0x01, 0x01),
            panel_deep: Color32::from_rgb(0x03, 0x01, 0x01),
            card: Color32::from_rgb(0x04, 0x01, 0x01),
            card_hover: Color32::from_rgb(0x05, 0x01, 0x01),
            field: Color32::from_rgb(0x06, 0x01, 0x01),
            border: Color32::from_rgb(0x07, 0x01, 0x01),
            border_soft: Color32::from_rgb(0x08, 0x01, 0x01),
            text: Color32::from_rgb(0x09, 0x01, 0x01),
            muted: Color32::from_rgb(0x0A, 0x01, 0x01),
            faint: Color32::from_rgb(0x0B, 0x01, 0x01),
            accent: Color32::from_rgb(0x0C, 0x01, 0x01),
            accent_soft: Color32::from_rgb(0x0D, 0x01, 0x01),
            purple: Color32::from_rgb(0x0E, 0x01, 0x01),
            amber: Color32::from_rgb(0x0F, 0x01, 0x01),
            red: Color32::from_rgb(0x10, 0x01, 0x01),
        };
        let fallback = theme_defaults::graphite();
        let text = template("Sentinel", &sentinel);
        let parsed = parse("sentinel", &text, &fallback).expect("sentinel template parses");
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.palette, sentinel);
    }

    #[test]
    fn token_table_is_unique_and_matches_the_template() {
        let palette = theme_defaults::graphite();
        let text = template("T", &palette);
        let mut seen = std::collections::HashSet::new();
        let mut colour_count = 0;
        for (token, _) in COLOUR_TOKENS {
            assert!(seen.insert(*token), "duplicate token {token}");
            assert!(
                text.contains(&format!("\"{token}\":")),
                "{token} missing from template"
            );
            colour_count += 1;
        }
        assert_eq!(colour_count, COLOUR_TOKENS.len());
        assert_eq!(colour_count, 16, "one token per Palette colour field");
    }

    /// Every colour token writes the field it claims to write (the template →
    /// parse round-trip above proves the set is complete; this proves each row
    /// points at its own field).
    #[test]
    fn each_colour_token_sets_its_own_field() {
        let fallback = theme_defaults::graphite();
        for (token, colour) in COLOUR_TOKENS {
            let text = theme_json(&format!("\"name\": \"X\",\n\"{token}\": \"#123456\""));
            let parsed = parse("x", &text, &fallback).expect("token object parses");
            assert_eq!(
                colour.get(&parsed.palette),
                Color32::from_rgb(0x12, 0x34, 0x56),
                "{token} did not set its field"
            );
        }
    }

    #[test]
    fn partial_theme_falls_back_per_token() {
        let fallback = theme_defaults::graphite();
        let text =
            theme_json("\"name\": \"Custom\",\n\"bg\": \"#123456\",\n\"accent\": \"#ff0000\"");
        let parsed = parse("custom", &text, &fallback).expect("partial theme parses");
        assert_eq!(parsed.name, "Custom");
        assert_eq!(parsed.palette.bg, Color32::from_rgb(0x12, 0x34, 0x56));
        assert_eq!(parsed.palette.accent, Color32::from_rgb(0xFF, 0x00, 0x00));
        assert_eq!(parsed.palette.text, fallback.text);
        assert_eq!(parsed.palette.panel, fallback.panel);
    }

    #[test]
    fn invalid_colour_keeps_the_fallback() {
        let fallback = theme_defaults::graphite();
        let text = theme_json("\"bg\": \"not-a-colour\",\n\"text\": \"#123\"");
        let parsed = parse("broken", &text, &fallback).expect("file still parses");
        assert_eq!(parsed.palette.bg, fallback.bg);
        assert_eq!(parsed.palette.text, Color32::from_rgb(0x11, 0x22, 0x33));
        assert!(parsed.warnings.iter().any(|w| w.contains("invalid colour")));
    }

    /// A colour that is not even a string is the same failure as bad hex: keep
    /// the Graphite value and say so, rather than dropping the whole theme.
    #[test]
    fn non_string_colour_keeps_the_fallback() {
        let fallback = theme_defaults::graphite();
        let text = theme_json("\"bg\": 123456,\n\"text\": \"#123\"");
        let parsed = parse("broken", &text, &fallback).expect("file still parses");
        assert_eq!(parsed.palette.bg, fallback.bg);
        assert!(
            parsed
                .warnings
                .iter()
                .any(|w| w.contains("invalid colour") && w.contains("123456")),
            "{:?}",
            parsed.warnings
        );
    }

    /// A hex value without the `#` is accepted, exactly like the old parser.
    #[test]
    fn hex_without_hash_is_accepted() {
        let fallback = theme_defaults::graphite();
        let text = theme_json("\"bg\": \"123456\"");
        let parsed = parse("x", &text, &fallback).expect("file parses");
        assert_eq!(parsed.palette.bg, Color32::from_rgb(0x12, 0x34, 0x56));
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    #[test]
    fn unknown_tokens_are_ignored() {
        let fallback = theme_defaults::graphite();
        let text = theme_json("\"sparkle\": \"#ffffff\",\n\"bg\": \"#123456\"");
        let parsed = parse("t", &text, &fallback).expect("file parses");
        assert!(parsed.warnings.iter().any(|w| w.contains("sparkle")));
        assert_eq!(parsed.palette.bg, Color32::from_rgb(0x12, 0x34, 0x56));
        assert_eq!(parsed.palette.text, fallback.text);
    }

    /// Unknown tokens are not "tokens": a file with none of the known ones is
    /// still empty and must say so, on top of the per-token warnings.
    #[test]
    fn unknown_only_file_reports_no_valid_tokens() {
        let fallback = theme_defaults::graphite();
        let text = theme_json("\"sparkle\": \"#ffffff\",\n\"glitter\": \"#000000\"");
        let parsed = parse("t", &text, &fallback).expect("file parses");
        let warnings = &parsed.warnings;
        assert!(
            warnings.iter().any(|w| w.contains("sparkle")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("glitter")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("has no tokens")),
            "{warnings:?}"
        );
        assert_eq!(parsed.palette.bg, fallback.bg);
        assert_eq!(parsed.palette.text, fallback.text);
    }

    /// A file that is not JSON at all is a skip, not a half-parsed theme: the
    /// caller drops it and keeps the others.
    #[test]
    fn corrupt_json_is_rejected_with_a_warning() {
        let fallback = theme_defaults::graphite();
        let error = parse("broken", "{ \"bg\": ", &fallback).expect_err("must not parse");
        assert!(error.contains("broken.json"), "{error}");
        assert!(error.contains("not valid JSON"), "{error}");
        assert!(
            parse("list", "[]", &fallback).is_err(),
            "array is not an object"
        );
        assert!(parse("scalar", "\"#ffffff\"", &fallback).is_err());
    }

    /// Missing or empty name → the id, the same rule the heading had.
    #[test]
    fn missing_or_empty_name_falls_back_to_the_id() {
        let fallback = theme_defaults::graphite();
        let unnamed =
            parse("mine", &theme_json("\"bg\": \"#101010\""), &fallback).expect("file parses");
        assert_eq!(unnamed.name, "mine");
        let empty = parse("mine", &theme_json("\"name\": \"  \""), &fallback).expect("file parses");
        assert_eq!(empty.name, "mine");
        let named =
            parse("mine", &theme_json("\"name\": \" Mine \""), &fallback).expect("file parses");
        assert_eq!(named.name, "Mine");
    }

    /// `Graphite.md` (the seeded file name's heading case aside, the *id* may be
    /// capitalised by the user) must use graphite's ratio, not the dark default.
    #[test]
    fn graphite_accent_soft_ratio_is_case_insensitive() {
        let fallback = theme_defaults::graphite();
        let text = theme_json("\"bg\": \"#101010\",\n\"accent\": \"#0097ff\"");
        let upper = parse("Graphite", &text, &fallback).expect("file parses");
        let lower = parse("graphite", &text, &fallback).expect("file parses");
        assert_eq!(upper.palette.accent_soft, lower.palette.accent_soft);
        assert_eq!(
            upper.palette.accent_soft,
            blend(
                upper.palette.bg,
                upper.palette.accent,
                ACCENT_SOFT_BLEND_GRAPHITE
            )
        );
        assert_ne!(
            upper.palette.accent_soft,
            blend(
                upper.palette.bg,
                upper.palette.accent,
                ACCENT_SOFT_BLEND_DARK,
            )
        );
    }

    #[test]
    fn light_flag_and_explicit_accent_soft_are_honoured() {
        let fallback = theme_defaults::graphite();
        let text = theme_json("\"light\": true,\n\"accent_soft\": \"#abcdef\"");
        let parsed = parse("l", &text, &fallback).expect("file parses");
        assert!(parsed.palette.is_light);
        assert_eq!(
            parsed.palette.accent_soft,
            Color32::from_rgb(0xAB, 0xCD, 0xEF)
        );
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    /// The flag keeps the old lenient spelling, and a nonsense value warns
    /// instead of dropping the file.
    #[test]
    fn light_flag_accepts_strings_and_warns_on_junk() {
        let fallback = theme_defaults::graphite();
        let spelled = parse("l", &theme_json("\"light\": \"yes\""), &fallback).expect("parses");
        assert!(spelled.palette.is_light);
        let junk = parse("l", &theme_json("\"light\": [1]"), &fallback).expect("parses");
        assert!(!junk.palette.is_light);
        assert!(
            junk.warnings.iter().any(|w| w.contains("\"light\"")),
            "{:?}",
            junk.warnings
        );
    }

    /// The pre-0.6 reader still has to work, because it is what a migration
    /// converts: this is a real 0.5.0 file, verbatim.
    #[test]
    fn markdown_sample_still_parses_for_migration() {
        let fallback = theme_defaults::graphite();
        let text = "# Mocha (old)\n\n| token | value |\n| --- | --- |\n| light | false |\n| bg | #1e1e2e |\n| accent | #f5a97f |\n";
        let parsed = parse_markdown("mocha", text, &fallback);
        assert_eq!(parsed.name, "Mocha (old)");
        assert_eq!(parsed.palette.bg, Color32::from_rgb(0x1E, 0x1E, 0x2E));
        assert_eq!(parsed.palette.accent, Color32::from_rgb(0xF5, 0xA9, 0x7F));
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        // ...and the conversion target reproduces it exactly.
        let converted = template(&parsed.name, &parsed.palette);
        let round = parse("mocha", &converted, &fallback).expect("converted file parses");
        assert_eq!(round.name, parsed.name);
        assert_eq!(round.palette, parsed.palette);
    }
}
