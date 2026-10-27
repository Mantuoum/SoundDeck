//! Every decision about RAM versus disk lives here.
//!
//! The registry, the loader and the UI all ask this module, so there is exactly
//! one definition of "eligible", "too long", "full" and "cannot read the
//! process size". No other file is allowed to guess.

use crate::audio::decode::{CHANNELS, SAMPLE_RATE};

/// Bytes per sample in the canonical format.
pub const SAMPLE_BYTES: usize = std::mem::size_of::<f32>();
pub const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * CHANNELS * SAMPLE_BYTES;

/// Total decoded audio that may stay resident, across all sounds.
pub const RETAINED_BYTES_CAP: usize = 32 * 1024 * 1024;
/// Maximum number of resident sounds.
pub const RETAINED_SOUND_CAP: usize = 16;
/// Longest single sound that may be kept resident. Longer sounds always stream.
pub const RETAIN_SECONDS_CAP: f32 = 30.0;
/// Bytes a single sound may occupy to be eligible for preloading.
pub const PER_SOUND_RETAIN_BYTES: usize = (RETAIN_SECONDS_CAP as usize) * BYTES_PER_SECOND;

/// Hard ceiling for the whole process, measured as private commit charge.
///
/// Windows does not expose "private working set" cheaply, so the valve uses
/// commit (`PrivateUsage`), which runs ~12 MB above the working-set number the
/// product promise quotes.
pub const PROCESS_CEILING: usize = 125 * 1024 * 1024;
pub const PROCESS_HEADROOM: usize = 4 * 1024 * 1024;

/// What the cache currently holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: usize,
    pub sounds: usize,
}

/// Canonical decoded size for a duration, used for pre-decode estimates.
pub fn bytes_for_seconds(seconds: f32) -> usize {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * BYTES_PER_SECOND as f32).ceil() as usize
}

/// Why a sound is going to the disk path instead of the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamReason {
    PreloadOff,
    TooLong,
    UnknownDuration,
    BudgetFull,
}

/// What to do with a sound that is not already resident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Decode once, keep it, then play.
    Preload,
    /// Play straight from disk; memory stays flat.
    Stream(StreamReason),
}

/// Inputs to [`route`]. Deliberately plain data so the rules are unit-testable.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub retain: bool,
    pub duration: Option<f32>,
    pub usage: Usage,
    pub process_bytes: Option<usize>,
}

/// The single decision function. `None` means "already resident" and never
/// reaches here.
pub fn route(request: &Request) -> Route {
    if !request.retain {
        return Route::Stream(StreamReason::PreloadOff);
    }
    let Some(duration) = request.duration else {
        return Route::Stream(StreamReason::UnknownDuration);
    };
    if duration > RETAIN_SECONDS_CAP {
        return Route::Stream(StreamReason::TooLong);
    }
    if !admit(
        request.usage,
        bytes_for_seconds(duration),
        request.process_bytes,
    ) {
        return Route::Stream(StreamReason::BudgetFull);
    }
    Route::Preload
}

/// Whether a sound of `bytes` may be added to `usage` without breaking a cap.
///
/// `process_bytes` is the current private commit when known. `None` means the
/// platform refused to tell us, and the caller must fail closed.
pub fn admit(usage: Usage, bytes: usize, process_bytes: Option<usize>) -> bool {
    if bytes == 0 || bytes > PER_SOUND_RETAIN_BYTES {
        return false;
    }
    if usage.sounds >= RETAINED_SOUND_CAP {
        return false;
    }
    if usage.bytes.saturating_add(bytes) > RETAINED_BYTES_CAP {
        return false;
    }
    match process_bytes {
        Some(current) => {
            current.saturating_add(bytes) <= PROCESS_CEILING.saturating_sub(PROCESS_HEADROOM)
        }
        None => false,
    }
}

/// Human-readable refusal reason for the activity log.
pub fn refusal_reason(usage: Usage, bytes: usize, process_bytes: Option<usize>) -> String {
    if bytes == 0 {
        return "the decoder produced no audio".into();
    }
    if bytes > PER_SOUND_RETAIN_BYTES {
        return format!(
            "it is longer than {:.0}s ({:.1} MB decoded)",
            RETAIN_SECONDS_CAP,
            bytes as f32 / (1024.0 * 1024.0)
        );
    }
    if usage.sounds >= RETAINED_SOUND_CAP {
        return format!("the {} sound preload limit is reached", RETAINED_SOUND_CAP);
    }
    if usage.bytes.saturating_add(bytes) > RETAINED_BYTES_CAP {
        return format!(
            "the {} MB preload budget is full",
            RETAINED_BYTES_CAP / (1024 * 1024)
        );
    }
    if process_bytes.is_none() {
        return "the process memory usage could not be read".into();
    }
    format!(
        "keeping it would exceed the {} MB process limit",
        PROCESS_CEILING / (1024 * 1024)
    )
}

/// Private commit charge of this process, in bytes.
///
/// `None` when the query fails; callers treat that as "do not grow".
#[cfg(windows)]
pub fn process_bytes() -> Option<usize> {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    }
    .ok()?;
    Some(counters.PrivateUsage)
}

#[cfg(not(windows))]
pub fn process_bytes() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(duration: Option<f32>) -> Request {
        Request {
            retain: true,
            duration,
            usage: Usage::default(),
            process_bytes: Some(60 * 1024 * 1024),
        }
    }

    #[test]
    fn nonsense_durations_are_zero() {
        assert_eq!(bytes_for_seconds(f32::NAN), 0);
        assert_eq!(bytes_for_seconds(-5.0), 0);
        assert_eq!(bytes_for_seconds(0.0), 0);
    }

    #[test]
    fn a_short_hotkey_sound_preloads() {
        assert_eq!(route(&request(Some(2.0))), Route::Preload);
    }

    #[test]
    fn the_thirty_second_boundary_is_inclusive() {
        assert_eq!(route(&request(Some(30.0))), Route::Preload);
        assert_eq!(
            route(&request(Some(30.01))),
            Route::Stream(StreamReason::TooLong)
        );
    }

    #[test]
    fn unknown_duration_streams() {
        assert_eq!(
            route(&request(None)),
            Route::Stream(StreamReason::UnknownDuration)
        );
    }

    #[test]
    fn preload_off_streams() {
        let mut req = request(Some(2.0));
        req.retain = false;
        assert_eq!(route(&req), Route::Stream(StreamReason::PreloadOff));
    }

    #[test]
    fn a_full_cache_streams() {
        let mut req = request(Some(2.0));
        req.usage = Usage {
            bytes: RETAINED_BYTES_CAP - 1000,
            sounds: 1,
        };
        assert_eq!(route(&req), Route::Stream(StreamReason::BudgetFull));
    }

    #[test]
    fn the_sound_count_cap_binds() {
        let usage = Usage {
            bytes: 1_000_000,
            sounds: RETAINED_SOUND_CAP,
        };
        assert!(!admit(usage, 100_000, Some(60 * 1024 * 1024)));
    }

    #[test]
    fn an_unreadable_process_size_fails_closed() {
        assert!(!admit(Usage::default(), 1_000_000, None));
        let mut req = request(Some(2.0));
        req.process_bytes = None;
        assert_eq!(route(&req), Route::Stream(StreamReason::BudgetFull));
    }

    #[test]
    fn the_process_ceiling_binds() {
        let near = PROCESS_CEILING - PROCESS_HEADROOM - 1000;
        assert!(!admit(Usage::default(), 1_000_000, Some(near)));
    }
}
