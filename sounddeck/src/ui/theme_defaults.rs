//! Built-in palettes. These seed `config/themes/` on first run and provide the
//! Graphite fallback when a theme file is missing tokens.

use eframe::egui::Color32;

use super::theme::Palette;
use super::theme_color::{blend, from_rgb_u32};

fn hex(value: u32) -> Color32 {
    from_rgb_u32(value)
}

pub fn graphite() -> Palette {
    let bg = hex(0x0e1013);
    let accent = hex(0x0097ff);
    Palette {
        is_light: false,
        bg,
        panel: hex(0x15171b),
        panel_deep: hex(0x111316),
        card: hex(0x1b1e23),
        card_hover: hex(0x20242a),
        field: hex(0x0c0e11),
        border: hex(0x30343c),
        border_soft: hex(0x252930),
        text: hex(0xe8ecef),
        muted: hex(0x8a919a),
        faint: hex(0x5c636c),
        accent,
        accent_soft: blend(bg, accent, 0.20),
        purple: hex(0x8b78e6),
        amber: hex(0xd6a656),
        red: hex(0xde6969),
    }
}

fn macchiato() -> Palette {
    let bg = hex(0x24273a);
    let accent = hex(0x0097ff);
    Palette {
        is_light: false,
        bg,
        panel: hex(0x1e2030),
        panel_deep: hex(0x181926),
        card: hex(0x363a4f),
        card_hover: hex(0x494d64),
        field: hex(0x181926),
        border: hex(0x5b6078),
        border_soft: hex(0x494d64),
        text: hex(0xcad3f5),
        muted: hex(0xa5adcb),
        faint: hex(0x8087a2),
        accent,
        accent_soft: blend(bg, accent, 0.20),
        purple: hex(0xc6a0f6),
        amber: hex(0xf5a97f),
        red: hex(0xed8796),
    }
}

fn mocha() -> Palette {
    let bg = hex(0x1e1e2e);
    let accent = hex(0x0097ff);
    Palette {
        is_light: false,
        bg,
        panel: hex(0x181825),
        panel_deep: hex(0x11111b),
        card: hex(0x313244),
        card_hover: hex(0x45475a),
        field: hex(0x11111b),
        border: hex(0x585b70),
        border_soft: hex(0x45475a),
        text: hex(0xcdd6f4),
        muted: hex(0xa6adc8),
        faint: hex(0x7f849c),
        accent,
        accent_soft: blend(bg, accent, 0.20),
        purple: hex(0xcba6f7),
        amber: hex(0xfab387),
        red: hex(0xf38ba8),
    }
}

fn frappe() -> Palette {
    let bg = hex(0x303446);
    let accent = hex(0x0097ff);
    Palette {
        is_light: false,
        bg,
        panel: hex(0x292c3c),
        panel_deep: hex(0x232634),
        card: hex(0x414559),
        card_hover: hex(0x51576d),
        field: hex(0x232634),
        border: hex(0x626880),
        border_soft: hex(0x51576d),
        text: hex(0xc6d0f5),
        muted: hex(0xa5adce),
        faint: hex(0x838ba7),
        accent,
        accent_soft: blend(bg, accent, 0.20),
        purple: hex(0xca9ee6),
        amber: hex(0xef9f76),
        red: hex(0xe78284),
    }
}

fn latte() -> Palette {
    let bg = hex(0xeff1f5);
    let accent = hex(0x0097ff);
    Palette {
        is_light: true,
        bg,
        panel: hex(0xe6e9ef),
        panel_deep: hex(0xdce0e8),
        card: hex(0xccd0da),
        card_hover: hex(0xbcc0cc),
        field: hex(0xe6e9ef),
        border: hex(0xacb0be),
        border_soft: hex(0xccd0da),
        text: hex(0x4c4f69),
        muted: hex(0x6c6f85),
        faint: hex(0x8c8fa1),
        accent,
        accent_soft: blend(bg, accent, 0.18),
        purple: hex(0x8839ef),
        amber: hex(0xdf8e1d),
        red: hex(0xd20f39),
    }
}

/// Graphite first: it is the default and the fallback.
pub fn all() -> Vec<(&'static str, &'static str, Palette)> {
    vec![
        ("graphite", "Graphite", graphite()),
        ("macchiato", "Catppuccin Macchiato", macchiato()),
        ("mocha", "Catppuccin Mocha", mocha()),
        ("frappe", "Catppuccin Frappe", frappe()),
        ("latte", "Catppuccin Latte", latte()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphite_is_dark_and_uses_the_accent() {
        let palette = graphite();
        assert!(!palette.is_light);
        assert_eq!(palette.accent, Color32::from_rgb(0x00, 0x97, 0xFF));
    }

    #[test]
    fn all_ids_are_unique() {
        let ids: std::collections::HashSet<_> = all().iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids.len(), all().len());
    }
}
