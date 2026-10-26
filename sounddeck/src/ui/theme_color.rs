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

pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_unmultiplied(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}
