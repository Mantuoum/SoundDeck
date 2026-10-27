//! Memory budgets for decoded audio.
//!
//! Everything that keeps samples alive goes through here, so there is exactly
//! one place that decides whether a buffer is allowed to stay resident.
//!
//! The numbers are deliberately conservative: the base process is ~75 MB and the
//! product ceiling is 105 MB, so the audio cache can never be the reason the app
//! grows without bound.

/// Bytes per sample in the canonical format (48 kHz stereo f32).
pub const SAMPLE_BYTES: usize = std::mem::size_of::<f32>();
pub const CHANNELS: usize = 2;
pub const BYTES_PER_SECOND: usize = 48_000 * CHANNELS * SAMPLE_BYTES; // 384 000

/// Total decoded audio that may stay resident, across all sounds.
pub const RETAINED_BYTES_CAP: usize = 32 * 1024 * 1024;

/// Maximum number of resident sounds.
pub const RETAINED_SOUND_CAP: usize = 16;

/// Longest single sound that may be kept resident. Longer sounds always play
/// from disk; preloading them is refused before any decoding happens.
pub const RETAIN_SECONDS_CAP: f32 = 30.0;

/// Bytes a single sound may occupy to be eligible for preloading.
pub const PER_SOUND_RETAIN_BYTES: usize = (RETAIN_SECONDS_CAP as usize) * BYTES_PER_SECOND;

/// Hard ceiling for the whole process, measured as private commit charge.
///
/// Windows does not expose "private working set" cheaply, so the valve uses
/// commit (`PrivateUsage`), which runs ~12 MB above the working-set number the
/// product promise quotes. 125 MB of commit corresponds to roughly 105 MB in
/// Task Manager with the default audio cache.
pub const PROCESS_CEILING: usize = 125 * 1024 * 1024;
pub const PROCESS_HEADROOM: usize = 4 * 1024 * 1024;

/// What the cache currently holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: usize,
    pub sounds: usize,
}

/// Canonical decoded size for a duration, used for the pre-decode estimate.
pub fn bytes_for_seconds(seconds: f32) -> usize {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * BYTES_PER_SECOND as f32).ceil() as usize
}

pub fn seconds_for_bytes(bytes: usize) -> f32 {
    bytes as f32 / BYTES_PER_SECOND as f32
}

/// Whether a sound of `bytes` may be added to `usage` without breaking a cap.
///
/// `process_bytes` is the current private working set when known. `None` means
/// the platform refused to tell us, and the caller must fail closed.
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
pub fn private_bytes() -> Option<usize> {
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
    usize::try_from(counters.PrivateUsage).ok()
}

#[cfg(not(windows))]
pub fn private_bytes() -> Option<usize> {
    None
}

/// Sample count accepted by the mixer: non-empty and an even number of samples.
pub fn is_valid_buffer(samples: &[f32]) -> bool {
    !samples.is_empty() && samples.len() % CHANNELS == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_and_bytes_round_trip() {
        assert_eq!(bytes_for_seconds(1.0), 384_000);
        assert!((seconds_for_bytes(384_000) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn nonsense_durations_are_zero() {
        assert_eq!(bytes_for_seconds(f32::NAN), 0);
        assert_eq!(bytes_for_seconds(-5.0), 0);
        assert_eq!(bytes_for_seconds(0.0), 0);
    }

    #[test]
    fn a_long_sound_is_refused() {
        let bytes = bytes_for_seconds(31.0);
        assert!(!admit(Usage::default(), bytes, Some(60 * 1024 * 1024)));
    }

    #[test]
    fn a_short_sound_fits() {
        assert!(admit(Usage::default(), 2_000_000, Some(60 * 1024 * 1024)));
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
    fn the_byte_cap_binds() {
        let usage = Usage {
            bytes: RETAINED_BYTES_CAP - 1000,
            sounds: 1,
        };
        assert!(!admit(usage, 500_000, Some(60 * 1024 * 1024)));
    }

    #[test]
    fn an_unreadable_process_size_fails_closed() {
        assert!(!admit(Usage::default(), 1_000_000, None));
    }

    #[test]
    fn the_process_ceiling_binds() {
        let near = PROCESS_CEILING - PROCESS_HEADROOM - 1000;
        assert!(!admit(Usage::default(), 1_000_000, Some(near)));
    }

    #[test]
    fn buffers_must_be_even_and_non_empty() {
        assert!(is_valid_buffer(&[0.0, 0.0]));
        assert!(!is_valid_buffer(&[]));
        assert!(!is_valid_buffer(&[0.0]));
    }
}
