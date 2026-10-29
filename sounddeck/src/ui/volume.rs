//! One logarithmic volume curve and one slim volume control for the whole app.
//!
//! Stored values are linear amplitude (`0..1`); slider positions are mapped to
//! −60…0 dBFS so the track matches perceived loudness. Cards and Settings rows
//! share this bar so clamping, formatting, scrolling and tooltips stay identical.

use eframe::egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Ui, Vec2};

use super::theme;

pub const RANGE_DB: f32 = 60.0;

/// One wheel notch moves the slider by this fraction of the track.
const WHEEL_STEP: f32 = 0.03;

/// Wheel deltas at or below this are noise, not a notch.
const WHEEL_DEADZONE: f32 = 0.01;

/// Inset of the track from the bar's left edge. The bar draws no glyph, so the
/// track starts at this padding instead of after an icon.
const TRACK_INSET: f32 = 2.0;

/// Half-height of the 4 px track, measured from the bar's centre line.
const TRACK_HALF_HEIGHT: f32 = 2.0;

/// Space between the track's right end and the percentage label.
const LABEL_GAP: f32 = 8.0;

/// Width reserved for the fixed-width percentage text.
const LABEL_WIDTH: f32 = 34.0;

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

/// Fixed-width percentage for a slider position. A non-finite position renders
/// as "0 %" — never "NaN%" in the UI.
pub fn percent_text(position: f32) -> String {
    let position = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.0
    };
    format!("{:>3.0}%", position * 100.0)
}

/// Track rect inside a bar. Both modes start at the same left inset (there is
/// no glyph any more, so no dead space); only the label reserve differs.
fn track_rect(rect: Rect, compact: bool) -> Rect {
    let label_width = if compact { 0.0 } else { LABEL_WIDTH };
    Rect::from_min_max(
        Pos2::new(
            rect.left() + TRACK_INSET,
            rect.center().y - TRACK_HALF_HEIGHT,
        ),
        Pos2::new(
            rect.right() - label_width - LABEL_GAP,
            rect.center().y + TRACK_HALF_HEIGHT,
        ),
    )
}

/// Slider position after one wheel notch, or `None` when the scroll is noise.
/// Saturates at both ends of the track.
fn wheel_step(position: f32, scroll: f32) -> Option<f32> {
    if !scroll.is_finite() || scroll.abs() <= WHEEL_DEADZONE {
        return None;
    }
    Some((position + WHEEL_STEP * scroll.signum()).clamp(0.0, 1.0))
}

/// Slim volume bar used by sound cards, list rows, the loop card and Settings.
///
/// `compact` hides the percentage label. Wheel over the bar adjusts by 3 % of
/// the track per notch and consumes the scroll, so the surrounding `ScrollArea`
/// never moves with it.
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
    // One palette snapshot per bar: the colours come out of it, and reading
    // them off the copy keeps the paint path free of repeated lock lookups.
    let palette = theme::palette();
    let position = to_position(volume);
    let track = track_rect(rect, compact);
    painter.rect_filled(track, CornerRadius::same(2), palette.field);
    let filled = Rect::from_min_max(
        track.left_top(),
        Pos2::new(track.left() + track.width() * position, track.bottom()),
    );
    painter.rect_filled(filled, CornerRadius::same(2), palette.accent);
    let knob = Pos2::new(filled.right(), track.center().y);
    painter.circle_filled(
        knob,
        if response.hovered() { 5.0 } else { 4.0 },
        palette.text,
    );
    if !compact {
        painter.text(
            Pos2::new(rect.right() - 2.0, rect.center().y),
            Align2::RIGHT_CENTER,
            percent_text(position),
            theme::ui_font(11.0),
            palette.muted,
        );
    }

    if response.dragged() || response.clicked() {
        if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
            let target = ((pointer.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
            on_change(from_position(target));
        }
    } else if response.hovered() {
        // Consume the wheel on *every* frame the pointer is over the bar, not
        // only on the frame that carried the event. egui 0.31 feeds a notched
        // wheel into `unprocessed_scroll_delta` and re-emits a fraction of it as
        // `smooth_scroll_delta` over the following frames
        // (`input_state::begin_pass`); `ScrollArea::end` reads that field after
        // its children painted. Clearing only the event frame left the leftover
        // smoothing to scroll the page a few frames later.
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        ui.input_mut(|i| {
            i.raw_scroll_delta = Vec2::ZERO;
            i.smooth_scroll_delta = Vec2::ZERO;
        });
        if let Some(target) = wheel_step(position, scroll) {
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

    #[test]
    fn percentage_never_renders_nan() {
        assert_eq!(percent_text(f32::NAN), "  0%");
        assert_eq!(percent_text(f32::INFINITY), "  0%");
        assert_eq!(percent_text(f32::NEG_INFINITY), "  0%");
        assert_eq!(percent_text(-0.5), "  0%");
        assert_eq!(percent_text(1.5), "100%");
    }

    #[test]
    fn track_should_start_at_the_left_inset_in_both_modes() {
        let bar_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 16.0));
        let compact = track_rect(bar_rect, true);
        let full = track_rect(bar_rect, false);
        assert_eq!(compact.left(), bar_rect.left() + TRACK_INSET);
        assert_eq!(full.left(), compact.left(), "no glyph dead space");
    }

    #[test]
    fn track_should_keep_clear_of_the_percentage_label() {
        let bar_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 16.0));
        let compact = track_rect(bar_rect, true);
        let full = track_rect(bar_rect, false);
        assert_eq!(compact.right(), bar_rect.right() - LABEL_GAP);
        assert_eq!(full.right(), bar_rect.right() - LABEL_WIDTH - LABEL_GAP);
        assert!(full.width() > 0.0 && full.width() < compact.width());
    }

    #[test]
    fn wheel_step_should_move_three_percent_per_notch() {
        let up = wheel_step(0.5, 1.0).expect("a notch changes the position");
        let down = wheel_step(0.5, -1.0).expect("a notch changes the position");
        assert!((up - 0.53).abs() < 1e-6, "{up}");
        assert!((down - 0.47).abs() < 1e-6, "{down}");
    }

    #[test]
    fn wheel_step_should_clamp_at_the_track_ends() {
        assert_eq!(wheel_step(0.99, 1.0), Some(1.0));
        assert_eq!(wheel_step(0.01, -1.0), Some(0.0));
        assert_eq!(wheel_step(1.0, 1.0), Some(1.0));
        assert_eq!(wheel_step(0.0, -1.0), Some(0.0));
    }

    #[test]
    fn wheel_step_should_ignore_deltas_below_the_deadzone() {
        assert_eq!(wheel_step(0.5, 0.0), None);
        assert_eq!(wheel_step(0.5, WHEEL_DEADZONE * 0.5), None);
        assert_eq!(wheel_step(0.5, f32::NAN), None);
    }
}
