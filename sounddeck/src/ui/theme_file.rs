//! Markdown theme files: `config/themes/<id>.md`.
//!
//! A file is a heading plus a two-column table. Missing or invalid tokens keep
//! the Graphite fallback value, so a half-edited theme never breaks the app.

use eframe::egui::Color32;

use super::theme::Palette;
use super::theme_color::{blend, from_hex};

pub struct ParsedTheme {
    pub name: String,
    pub palette: Palette,
    pub warnings: Vec<String>,
}

fn set_color(
    target: &mut Color32,
    fallback: Color32,
    value: &str,
    token: &str,
    warnings: &mut Vec<String>,
) {
    match from_hex(value) {
        Some(color) => *target = color,
        None => {
            *target = fallback;
            warnings.push(format!(
                "themes: \"{token}\" has an invalid colour \"{value}\"; using Graphite"
            ));
        }
    }
}

pub fn parse(id: &str, text: &str, fallback: &Palette) -> ParsedTheme {
    let mut palette = *fallback;
    let mut name = None;
    let mut warnings = Vec::new();
    let mut saw_token = false;
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
        saw_token = true;

        match key.as_str() {
            "light" => {
                palette.is_light =
                    matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "1");
            }
            "bg" => set_color(&mut palette.bg, fallback.bg, value, "bg", &mut warnings),
            "panel" => set_color(
                &mut palette.panel,
                fallback.panel,
                value,
                "panel",
                &mut warnings,
            ),
            "panel_deep" => set_color(
                &mut palette.panel_deep,
                fallback.panel_deep,
                value,
                "panel_deep",
                &mut warnings,
            ),
            "card" => set_color(
                &mut palette.card,
                fallback.card,
                value,
                "card",
                &mut warnings,
            ),
            "card_hover" => set_color(
                &mut palette.card_hover,
                fallback.card_hover,
                value,
                "card_hover",
                &mut warnings,
            ),
            "field" => set_color(
                &mut palette.field,
                fallback.field,
                value,
                "field",
                &mut warnings,
            ),
            "border" => set_color(
                &mut palette.border,
                fallback.border,
                value,
                "border",
                &mut warnings,
            ),
            "border_soft" => set_color(
                &mut palette.border_soft,
                fallback.border_soft,
                value,
                "border_soft",
                &mut warnings,
            ),
            "text" => set_color(
                &mut palette.text,
                fallback.text,
                value,
                "text",
                &mut warnings,
            ),
            "muted" => set_color(
                &mut palette.muted,
                fallback.muted,
                value,
                "muted",
                &mut warnings,
            ),
            "faint" => set_color(
                &mut palette.faint,
                fallback.faint,
                value,
                "faint",
                &mut warnings,
            ),
            "accent" => set_color(
                &mut palette.accent,
                fallback.accent,
                value,
                "accent",
                &mut warnings,
            ),
            "accent_soft" => {
                accent_soft_seen = true;
                set_color(
                    &mut palette.accent_soft,
                    fallback.accent_soft,
                    value,
                    "accent_soft",
                    &mut warnings,
                )
            }
            "purple" => set_color(
                &mut palette.purple,
                fallback.purple,
                value,
                "purple",
                &mut warnings,
            ),
            "amber" => set_color(
                &mut palette.amber,
                fallback.amber,
                value,
                "amber",
                &mut warnings,
            ),
            "red" => set_color(&mut palette.red, fallback.red, value, "red", &mut warnings),
            other => warnings.push(format!("themes: unknown token \"{other}\" ignored")),
        }
    }

    if !saw_token {
        warnings.push(format!("themes: \"{id}.md\" has no tokens; using Graphite"));
    }

    // Derive the tinted accent surface unless the file provides it explicitly,
    // so the exact shipped values round-trip and partial edits stay consistent.
    if !accent_soft_seen {
        let ratio = if id == "graphite" {
            0.25
        } else if palette.is_light {
            0.18
        } else {
            0.20
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
    out.push_str(&format!("| light | {} |\n", palette.is_light));
    for (key, color) in [
        ("bg", palette.bg),
        ("panel", palette.panel),
        ("panel_deep", palette.panel_deep),
        ("card", palette.card),
        ("card_hover", palette.card_hover),
        ("field", palette.field),
        ("border", palette.border),
        ("border_soft", palette.border_soft),
        ("text", palette.text),
        ("muted", palette.muted),
        ("faint", palette.faint),
        ("accent", palette.accent),
        ("accent_soft", palette.accent_soft),
        ("purple", palette.purple),
        ("amber", palette.amber),
        ("red", palette.red),
    ] {
        out.push_str(&format!("| {key} | {} |\n", hex(color)));
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
        let text = "# T\n\n| token | value |\n| --- | --- |\n| sparkle | #ffffff |\n";
        let parsed = parse("t", text, &fallback);
        assert!(parsed.warnings.iter().any(|w| w.contains("sparkle")));
        assert_eq!(parsed.palette.text, fallback.text);
    }
}
