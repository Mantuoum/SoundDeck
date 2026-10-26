//! One logarithmic volume curve and one slim volume control for the whole app.
//!
//! Stored values are linear amplitude (`0..1`); slider positions are mapped to
//! −60…0 dBFS so the track matches perceived loudness. Cards and Settings rows
//! share this bar so clamping, formatting, scrolling and tooltips stay identical.

use eframe::egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Ui, Vec2};

use super::theme;

pub const RANGE_DB: f32 = 60.0;

/// Linear amplitude to slider position (`0..1`).
pub fn to_position(volume: f32) -> f32 {
    if !volume.is_finite() {
        return 0.0;
    }
    if volume <= 0.001 {
        return 0.0;
    }
    let db = 20.0 * volume.clamp(0.0, 1.0).log10();
    ((db.clamp(-RANGE_DB, 0.0) + RANGE_DB) / RANGE_DB).clamp(0.0, 1.0)
}

/// Slider position (`0..1`) to linear amplitude.
pub fn from_position(position: f32) -> f32 {
    if !position.is_finite() {
        return 0.0;
    }
    let position = position.clamp(0.0, 1.0);
    if position <= 0.002 {
        return 0.0;
    }
    10f32.powf((position * RANGE_DB - RANGE_DB) / 20.0)
}

/// Fixed-width percentage for a slider position.
pub fn percent_text(position: f32) -> String {
    format!("{:>3.0}%", position.clamp(0.0, 1.0) * 100.0)
}

/// Slim volume bar used by sound cards, the loop card and Settings rows.
///
/// `compact` hides the speaker glyph and the percentage. Wheel over the bar
/// adjusts by 3 % of track per notch and consumes the scroll.
pub fn bar(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    volume: f32,
    mut on_change: impl FnMut(f32),
    compact: bool,
) {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let painter = ui.painter().clone();
    let position = to_position(volume);
    let label_width = if compact { 0.0 } else { 34.0 };
    let track_left = if compact {
        rect.left() + 2.0
    } else {
        let speaker_size = 14.0;
        let speaker_rect = Rect::from_center_size(
            Pos2::new(rect.left() + speaker_size * 0.5, rect.center().y),
            Vec2::splat(speaker_size),
        );
        theme::speaker_icon(
            &painter,
            speaker_rect,
            if response.hovered() {
                theme::muted()
            } else {
                theme::faint()
            },
        );
        speaker_rect.right() + 8.0
    };
    let track = Rect::from_min_max(
        Pos2::new(track_left, rect.center().y - 2.0),
        Pos2::new(rect.right() - label_width - 8.0, rect.center().y + 2.0),
    );
    painter.rect_filled(track, CornerRadius::same(2), theme::field());
    let filled = Rect::from_min_max(
        track.left_top(),
        Pos2::new(track.left() + track.width() * position, track.bottom()),
    );
    painter.rect_filled(filled, CornerRadius::same(2), theme::accent());
    let knob = Pos2::new(filled.right(), track.center().y);
    painter.circle_filled(
        knob,
        if response.hovered() { 5.0 } else { 4.0 },
        theme::text(),
    );
    if !compact {
        painter.text(
            Pos2::new(rect.right() - 2.0, rect.center().y),
            Align2::RIGHT_CENTER,
            percent_text(position),
            theme::ui_font(11.0),
            theme::muted(),
        );
    }

    if response.dragged() || response.clicked() {
        if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
            let target = ((pointer.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
            on_change(from_position(target));
        }
    } else if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.01 {
            let target = (position + 0.03 * scroll.signum()).clamp(0.0, 1.0);
            ui.input_mut(|i| {
                i.raw_scroll_delta = Vec2::ZERO;
                i.smooth_scroll_delta = Vec2::ZERO;
            });
            on_change(from_position(target));
        }
    }

    response.on_hover_text(format!(
        "Volume {} - scroll or drag to adjust",
        percent_text(position)
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_round_trips() {
        assert_eq!(to_position(0.0), 0.0);
        assert!((to_position(1.0) - 1.0).abs() < 1e-6);
        assert!((to_position(0.5) - 0.9).abs() < 1e-3);
        assert!((from_position(0.5) - 0.031_623).abs() < 1e-4);
        assert_eq!(from_position(0.0), 0.0);

        for volume in [0.01, 0.05, 0.1, 0.25, 0.5, 0.8, 1.0] {
            let back = from_position(to_position(volume));
            assert!((back - volume).abs() < 1e-3, "{volume} -> {back}");
        }
    }

    #[test]
    fn clamps_and_handles_nan() {
        assert_eq!(to_position(-1.0), 0.0);
        assert_eq!(to_position(2.0), 1.0);
        assert_eq!(to_position(f32::NAN), 0.0);
        assert_eq!(from_position(-1.0), 0.0);
        assert_eq!(from_position(2.0), 1.0);
        assert_eq!(from_position(f32::NAN), 0.0);
    }

    #[test]
    fn percentage_matches_position() {
        assert_eq!(percent_text(0.0), "  0%");
        assert_eq!(percent_text(0.8), " 80%");
        assert_eq!(percent_text(1.0), "100%");
    }
}
