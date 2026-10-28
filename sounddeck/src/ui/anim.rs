//! Small, paint-only animation helpers. State lives in egui's memory keyed by Id.
//!
//! Durations are intentionally short (60–90 ms) and every tween calls
//! `request_repaint` while it is settling, so the frame rate drops back to the
//! idle timer as soon as nothing is moving. When animations are off the helpers
//! return the target immediately and touch neither the animator nor the frame
//! loop — "off" must not cost repaints.

use std::hash::Hash;

use eframe::egui::{self, Id};

static USER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static OS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_user(enabled: bool) {
    USER.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn set_os(enabled: bool) {
    OS.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn enabled() -> bool {
    USER.load(std::sync::atomic::Ordering::Relaxed) && OS.load(std::sync::atomic::Ordering::Relaxed)
}

/// A value that eases toward `target`. Returns the current value.
pub fn tween(ui: &egui::Ui, id: impl Hash, target: f32, seconds: f32) -> f32 {
    if !enabled() {
        return target;
    }
    let value = ui
        .ctx()
        .animate_value_with_time(Id::new(id), target, seconds);
    if (value - target).abs() > 0.002 {
        ui.ctx().request_repaint();
    }
    value
}

/// Ease-out cubic, clamped to 0..1. Used for fades driven by a timestamp.
/// Non-finite input (`NaN` from a missing timestamp) eases from 0, not `NaN`.
pub fn ease_out(t: f32) -> f32 {
    if !t.is_finite() {
        return 0.0;
    }
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 0 when idle, 1 when hovered; eases over ~60 ms.
pub fn hover(ui: &egui::Ui, id: impl Hash, hovered: bool) -> f32 {
    let target = if hovered { 1.0 } else { 0.0 };
    if !enabled() {
        return target;
    }
    let value = ui.ctx().animate_value_with_time(Id::new(id), target, 0.06);
    if (value - target).abs() > 0.002 {
        ui.ctx().request_repaint();
    }
    value
}

/// 0 when released, 1 while pressed; eases over ~50 ms / 90 ms.
pub fn press(ui: &egui::Ui, id: impl Hash, pressed: bool) -> f32 {
    let target = if pressed { 1.0 } else { 0.0 };
    if !enabled() {
        return target;
    }
    let seconds = if pressed { 0.05 } else { 0.09 };
    let value = ui
        .ctx()
        .animate_value_with_time(Id::new(id), target, seconds);
    if (value - target).abs() > 0.002 {
        ui.ctx().request_repaint();
    }
    value
}

#[cfg(windows)]
pub fn os_animations_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let mut result: i32 = 1;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut result as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    result != 0
}

#[cfg(not(windows))]
pub fn os_animations_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_clamps_and_survives_nan() {
        assert_eq!(ease_out(0.0), 0.0);
        assert!((ease_out(1.0) - 1.0).abs() < 1e-6);
        assert!(ease_out(-3.0).abs() < 1e-6);
        assert!((ease_out(4.0) - 1.0).abs() < 1e-6);
        assert_eq!(ease_out(f32::NAN), 0.0);
        assert!(ease_out(f32::INFINITY).is_finite());
        assert_eq!(ease_out(f32::NEG_INFINITY), 0.0);
    }

    /// The easing helpers never divide by zero or overflow inside 0..1.
    #[test]
    fn ease_out_is_monotonic() {
        let mut previous = 0.0;
        for step in 0..=20 {
            let value = ease_out(step as f32 / 20.0);
            assert!(value >= previous, "{value} < {previous}");
            assert!((0.0..=1.0).contains(&value));
            previous = value;
        }
    }
}
