//! Small colour helpers shared by the theme defaults and the Markdown parser.

use eframe::egui::Color32;

pub fn from_rgb_u32(value: u32) -> Color32 {
    Color32::from_rgb(
        ((value >> 16) & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (value & 0xFF) as u8,
    )
}

/// Accepts `#RGB` and `#RRGGBB` (with or without the leading `#`).
pub fn from_hex(text: &str) -> Option<Color32> {
    let hex = text.trim().trim_start_matches('#');
    match hex.len() {
        3 => {
            let mut value = 0u32;
            for ch in hex.chars() {
                let digit = ch.to_digit(16)?;
                value = (value << 4) | digit;
                value = (value << 4) | digit;
            }
            Some(from_rgb_u32(value))
        }
        6 => u32::from_str_radix(hex, 16).ok().map(from_rgb_u32),
        _ => None,
    }
}

/// Linear blend `a → b`. `t` is clamped to 0..1; non-finite `t` counts as 0 so
/// a `NaN` hover factor degrades to "no tint" instead of a transparent `NaN`
/// colour.
pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_unmultiplied(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: Color32 = Color32::from_rgb(0, 0, 0);
    const WHITE: Color32 = Color32::from_rgb(255, 255, 255);

    #[test]
    fn hex_accepts_three_and_six_digits() {
        assert_eq!(from_hex("#123"), Some(Color32::from_rgb(0x11, 0x22, 0x33)));
        assert_eq!(
            from_hex("123456"),
            Some(Color32::from_rgb(0x12, 0x34, 0x56))
        );
        assert_eq!(
            from_hex(" #abcdef "),
            Some(Color32::from_rgb(0xAB, 0xCD, 0xEF))
        );
    }

    #[test]
    fn hex_rejects_junk() {
        for bad in ["", "#", "#12", "#12345", "#gggggg", "not-a-colour"] {
            assert_eq!(from_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn blend_endpoints_and_midpoint() {
        assert_eq!(blend(BLACK, WHITE, 0.0), BLACK);
        assert_eq!(blend(BLACK, WHITE, 1.0), WHITE);
        assert_eq!(blend(BLACK, WHITE, 0.5), Color32::from_rgb(127, 127, 127));
        assert_eq!(blend(BLACK, WHITE, -2.0), BLACK);
        assert_eq!(blend(BLACK, WHITE, 3.0), WHITE);
    }

    #[test]
    fn blend_nan_factor_is_treated_as_zero() {
        assert_eq!(blend(BLACK, WHITE, f32::NAN), BLACK);
        assert_eq!(blend(WHITE, BLACK, f32::NAN), WHITE);
        assert!(blend(BLACK, WHITE, f32::INFINITY).is_opaque());
    }

    #[test]
    fn blend_keeps_alpha() {
        let a = Color32::from_rgba_unmultiplied(10, 20, 30, 40);
        let b = Color32::from_rgba_unmultiplied(200, 210, 220, 240);
        assert_eq!(blend(a, b, 0.0).a(), 40);
        assert_eq!(blend(a, b, 1.0).a(), 240);
    }
}
