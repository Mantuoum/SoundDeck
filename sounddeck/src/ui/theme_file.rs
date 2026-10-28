//! Markdown theme files: `config/themes/<id>.md`.
//!
//! A file is a heading plus a two-column table. Missing or invalid tokens keep
//! the Graphite fallback value, so a half-edited theme never breaks the app.
//!
//! Token names live in exactly one place: [`COLOUR_TOKENS`]. The parser reads
//! them, `template()` writes the shipped files from them, and the tests prove
//! every `Palette` colour field is reachable — adding a field without a token
//! fails the build (struct literal) or the test (round-trip).

use eframe::egui::Color32;

use super::theme::Palette;
use super::theme_color::{blend, from_hex};
use super::theme_defaults::{
    ACCENT_SOFT_BLEND_DARK, ACCENT_SOFT_BLEND_GRAPHITE, ACCENT_SOFT_BLEND_LIGHT,
};

/// Boolean row (rendered as `true`/`false`), not a colour like the rest.
const BOOL_TOKEN: &str = "light";

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

/// The theme token table: Markdown token name → palette field, in the order
/// `template()` writes them.
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

pub struct ParsedTheme {
    pub name: String,
    pub palette: Palette,
    pub warnings: Vec<String>,
}

pub fn parse(id: &str, text: &str, fallback: &Palette) -> ParsedTheme {
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
            palette.is_light = matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "1");
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
        warnings.push(format!("themes: \"{id}.md\" has no tokens; using Graphite"));
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

    ParsedTheme {
        name: name.unwrap_or_else(|| id.to_string()),
        palette,
        warnings,
    }
}

fn hex(color: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
}

pub fn template(name: &str, palette: &Palette) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {name}\n\n"));
    out.push_str("| token | value |\n");
    out.push_str("| --- | --- |\n");
    out.push_str(&format!("| {BOOL_TOKEN} | {} |\n", palette.is_light));
    for (token, colour) in COLOUR_TOKENS {
        out.push_str(&format!("| {token} | {} |\n", hex(colour.get(palette))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme_defaults;

    #[test]
    fn template_roundtrips_graphite() {
        let fallback = theme_defaults::graphite();
        let text = template("Graphite", &fallback);
        let parsed = parse("graphite", &text, &fallback);
        assert_eq!(parsed.name, "Graphite");
        assert_eq!(parsed.palette, fallback);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    /// One row per `Palette` colour field: adding a field breaks the sentinel
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
        let parsed = parse("sentinel", &text, &fallback);
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
                text.contains(&format!("| {token} |")),
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
            let text = format!("# X\n\n| token | value |\n| --- | --- |\n| {token} | #123456 |\n");
            let parsed = parse("x", &text, &fallback);
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
        let text = "# Custom\n\n| token | value |\n| --- | --- |\n| bg | #123456 |\n| accent | #ff0000 |\n";
        let parsed = parse("custom", text, &fallback);
        assert_eq!(parsed.name, "Custom");
        assert_eq!(parsed.palette.bg, Color32::from_rgb(0x12, 0x34, 0x56));
        assert_eq!(parsed.palette.accent, Color32::from_rgb(0xFF, 0x00, 0x00));
        assert_eq!(parsed.palette.text, fallback.text);
        assert_eq!(parsed.palette.panel, fallback.panel);
    }

    #[test]
    fn invalid_colour_keeps_the_fallback() {
        let fallback = theme_defaults::graphite();
        let text = "# Broken\n\n| token | value |\n| --- | --- |\n| bg | not-a-colour |\n| text | #123 |\n";
        let parsed = parse("broken", text, &fallback);
        assert_eq!(parsed.palette.bg, fallback.bg);
        assert_eq!(parsed.palette.text, Color32::from_rgb(0x11, 0x22, 0x33));
        assert!(parsed.warnings.iter().any(|w| w.contains("invalid colour")));
    }

    #[test]
    fn unknown_tokens_are_ignored() {
        let fallback = theme_defaults::graphite();
        let text =
            "# T\n\n| token | value |\n| --- | --- |\n| sparkle | #ffffff |\n| bg | #123456 |\n";
        let parsed = parse("t", text, &fallback);
        assert!(parsed.warnings.iter().any(|w| w.contains("sparkle")));
        assert_eq!(parsed.palette.bg, Color32::from_rgb(0x12, 0x34, 0x56));
        assert_eq!(parsed.palette.text, fallback.text);
    }

    /// Unknown tokens are not "tokens": a file with none of the known ones is
    /// still empty and must say so, on top of the per-token warnings.
    #[test]
    fn unknown_only_file_reports_no_valid_tokens() {
        let fallback = theme_defaults::graphite();
        let text = "# T\n\n| token | value |\n| --- | --- |\n| sparkle | #ffffff |\n| glitter | #000000 |\n";
        let parsed = parse("t", text, &fallback);
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

    /// `Graphite.md` (the seeded file name's heading case aside, the *id* may be
    /// capitalised by the user) must use graphite's ratio, not the dark default.
    #[test]
    fn graphite_accent_soft_ratio_is_case_insensitive() {
        let fallback = theme_defaults::graphite();
        let text =
            "# G\n\n| token | value |\n| --- | --- |\n| bg | #101010 |\n| accent | #0097ff |\n";
        let upper = parse("Graphite", text, &fallback);
        let lower = parse("graphite", text, &fallback);
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
        let text = "# L\n\n| token | value |\n| --- | --- |\n| light | true |\n| accent_soft | #abcdef |\n";
        let parsed = parse("l", text, &fallback);
        assert!(parsed.palette.is_light);
        assert_eq!(
            parsed.palette.accent_soft,
            Color32::from_rgb(0xAB, 0xCD, 0xEF)
        );
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }
}
