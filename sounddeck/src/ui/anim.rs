//! Small, paint-only animation helpers. State lives in egui's memory keyed by Id.
//!
//! Durations are intentionally short (60–90 ms) and every tween calls
//! `request_repaint` while it is settling, so the frame rate drops back to the
//! idle timer as soon as nothing is moving.

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
    let value = ui
        .ctx()
        .animate_value_with_time(Id::new(id), target, seconds);
    if (value - target).abs() > 0.002 {
        ui.ctx().request_repaint();
    }
    if !enabled() {
        return target;
    }
    value
}

/// Ease-out cubic, clamped to 0..1. Used for fades driven by a timestamp.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 0 when idle, 1 when hovered; eases over ~60 ms.
pub fn hover(ui: &egui::Ui, id: impl Hash, hovered: bool) -> f32 {
    let target = if hovered { 1.0 } else { 0.0 };
    let value = ui.ctx().animate_value_with_time(Id::new(id), target, 0.06);
    if (value - target).abs() > 0.002 {
        ui.ctx().request_repaint();
    }
    if !enabled() {
        return target;
    }
    value
}

/// 0 when released, 1 while pressed; eases over ~50 ms / 90 ms.
pub fn press(ui: &egui::Ui, id: impl Hash, pressed: bool) -> f32 {
    let target = if pressed { 1.0 } else { 0.0 };
    let seconds = if pressed { 0.05 } else { 0.09 };
    let value = ui
        .ctx()
        .animate_value_with_time(Id::new(id), target, seconds);
    if (value - target).abs() > 0.002 {
        ui.ctx().request_repaint();
    }
    if !enabled() {
        return target;
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
