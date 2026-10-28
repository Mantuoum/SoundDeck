//! Every decision about RAM versus disk lives here.
//!
//! The registry, the loader and the UI all ask this module, so there is exactly
//! one definition of "eligible", "too long", "full" and "cannot read the
//! process size". No other file is allowed to guess.
//!
//! A decision and its explanation are the same value: [`admit`] returns
//! [`Refusal`] on failure, and [`Refusal::message`] is the only place the
//! user-visible wording lives. There is no second function that re-derives the
//! caps just to describe them.
//!
//! **Windows only.** Off Windows [`process_bytes`] returns `None`, which
//! [`admit`] treats as "do not grow" ([`Refusal::ProcessUnreadable`]): the cache
//! fails closed, so nothing is ever preloaded and every sound streams from disk.
//! SoundDeck is a Windows product (SPEC ENV-1); that is a documented limit of the
//! portability shims, not a silent failure.

use crate::audio::decode::{CHANNELS, SAMPLE_RATE};

/// Bytes per sample in the canonical format.
pub const SAMPLE_BYTES: usize = std::mem::size_of::<f32>();
pub const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * CHANNELS * SAMPLE_BYTES;

/// Bytes in a mebibyte: the unit every cap and every message is expressed in.
const MIB: usize = 1024 * 1024;

/// Total decoded audio that may stay resident, across all sounds.
pub const RETAINED_BYTES_CAP: usize = 32 * MIB;
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
pub const PROCESS_CEILING: usize = 125 * MIB;
pub const PROCESS_HEADROOM: usize = 4 * MIB;

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

/// Why a sound may not be kept resident — the decision *and* its explanation.
///
/// [`admit`] returns this as its error, [`route`] carries it in
/// [`Route::Stream`], and [`Refusal::message`] turns it into the one sentence the
/// activity log shows. Adding a cap means adding a variant, not a second
/// re-derivation of the checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The user switched preload off for this sound: a choice, not a refusal.
    PreloadOff,
    /// The container duration could not be read, so the size is unknown.
    UnknownDuration,
    /// Longer than [`RETAIN_SECONDS_CAP`].
    TooLong,
    /// Bigger than [`PER_SOUND_RETAIN_BYTES`] even though the duration passed.
    TooLarge,
    /// The decoder produced no audio.
    NoAudio,
    /// [`RETAINED_SOUND_CAP`] sounds are already resident.
    SoundCap,
    /// [`RETAINED_BYTES_CAP`] would be crossed.
    ByteCap,
    /// The process commit could not be queried, so growth fails closed.
    ProcessUnreadable,
    /// Admitting it would cross [`PROCESS_CEILING`].
    ProcessCeiling,
}

impl Refusal {
    /// The one human-readable reason for the activity log.
    pub fn message(self) -> String {
        match self {
            Refusal::PreloadOff => "preload is off for this sound".into(),
            Refusal::UnknownDuration => "its duration could not be read".into(),
            Refusal::TooLong => format!(
                "it is longer than the {:.0}s preload limit",
                RETAIN_SECONDS_CAP
            ),
            Refusal::TooLarge => format!(
                "it is larger than the {} MB a {:.0}s sound may hold",
                PER_SOUND_RETAIN_BYTES / MIB,
                RETAIN_SECONDS_CAP
            ),
            Refusal::NoAudio => "the decoder produced no audio".into(),
            Refusal::SoundCap => {
                format!("the {} sound preload limit is reached", RETAINED_SOUND_CAP)
            }
            Refusal::ByteCap => {
                format!("the {} MB preload budget is full", RETAINED_BYTES_CAP / MIB)
            }
            Refusal::ProcessUnreadable => "the process memory usage could not be read".into(),
            Refusal::ProcessCeiling => format!(
                "keeping it would exceed the {} MB process limit",
                PROCESS_CEILING / MIB
            ),
        }
    }
}

/// What to do with a sound that is not already resident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Decode once, keep it, then play.
    Preload,
    /// Play straight from disk; memory stays flat. Carries the reason.
    Stream(Refusal),
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
        return Route::Stream(Refusal::PreloadOff);
    }
    let Some(duration) = request.duration else {
        return Route::Stream(Refusal::UnknownDuration);
    };
    if duration > RETAIN_SECONDS_CAP {
        return Route::Stream(Refusal::TooLong);
    }
    match admit(
        request.usage,
        bytes_for_seconds(duration),
        request.process_bytes,
    ) {
        Ok(()) => Route::Preload,
        Err(refusal) => Route::Stream(refusal),
    }
}

/// Admits or refuses a sound of `bytes` against `usage`.
///
/// `process_bytes` is the current private commit when known. `None` means the
/// platform refused to tell us, and admission fails closed.
pub fn admit(usage: Usage, bytes: usize, process_bytes: Option<usize>) -> Result<(), Refusal> {
    if bytes == 0 {
        return Err(Refusal::NoAudio);
    }
    if bytes > PER_SOUND_RETAIN_BYTES {
        return Err(Refusal::TooLarge);
    }
    if usage.sounds >= RETAINED_SOUND_CAP {
        return Err(Refusal::SoundCap);
    }
    if usage.bytes.saturating_add(bytes) > RETAINED_BYTES_CAP {
        return Err(Refusal::ByteCap);
    }
    match process_bytes {
        Some(current) => {
            if current.saturating_add(bytes) <= PROCESS_CEILING.saturating_sub(PROCESS_HEADROOM) {
                Ok(())
            } else {
                Err(Refusal::ProcessCeiling)
            }
        }
        None => Err(Refusal::ProcessUnreadable),
    }
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

    /// Comfortably below every cap, so only the field under test decides.
    const ROOMY_PROCESS: Option<usize> = Some(60 * MIB);

    fn request(duration: Option<f32>) -> Request {
        Request {
            retain: true,
            duration,
            usage: Usage::default(),
            process_bytes: ROOMY_PROCESS,
        }
    }

    /// One row of the admission table: inputs, the decision, and a fragment the
    /// explanation must contain (so the text stays wired to the cap it quotes).
    struct Case {
        name: &'static str,
        usage: Usage,
        bytes: usize,
        process: Option<usize>,
        expected: Refusal,
        message_contains: &'static str,
    }

    #[test]
    fn admission_and_its_explanation_agree_for_every_cap() {
        let cases = [
            Case {
                name: "no audio at all",
                usage: Usage::default(),
                bytes: 0,
                process: ROOMY_PROCESS,
                expected: Refusal::NoAudio,
                message_contains: "no audio",
            },
            Case {
                name: "per-sound bytes cap",
                usage: Usage::default(),
                bytes: PER_SOUND_RETAIN_BYTES + 1,
                process: ROOMY_PROCESS,
                expected: Refusal::TooLarge,
                message_contains: "larger than",
            },
            Case {
                name: "sound count cap",
                usage: Usage {
                    bytes: 1_000_000,
                    sounds: RETAINED_SOUND_CAP,
                },
                bytes: 100_000,
                process: ROOMY_PROCESS,
                expected: Refusal::SoundCap,
                message_contains: "sound preload limit",
            },
            Case {
                name: "total byte budget",
                usage: Usage {
                    bytes: RETAINED_BYTES_CAP - 1000,
                    sounds: 1,
                },
                bytes: 100_000,
                process: ROOMY_PROCESS,
                expected: Refusal::ByteCap,
                message_contains: "preload budget is full",
            },
            Case {
                name: "process ceiling",
                usage: Usage::default(),
                bytes: 1_000_000,
                process: Some(PROCESS_CEILING - PROCESS_HEADROOM - 1000),
                expected: Refusal::ProcessCeiling,
                message_contains: "process limit",
            },
            Case {
                name: "unreadable process size fails closed",
                usage: Usage::default(),
                bytes: 1_000_000,
                process: None,
                expected: Refusal::ProcessUnreadable,
                message_contains: "could not be read",
            },
        ];

        for case in cases {
            let decision = admit(case.usage, case.bytes, case.process);
            assert_eq!(
                decision,
                Err(case.expected),
                "wrong decision for {}",
                case.name
            );
            let refusal = decision.err().unwrap_or(Refusal::NoAudio);
            let message = refusal.message();
            assert!(
                message.contains(case.message_contains),
                "{}: message {message:?} should mention {:?}",
                case.name,
                case.message_contains
            );
        }
    }

    #[test]
    fn a_sound_within_every_cap_is_admitted() {
        assert_eq!(admit(Usage::default(), 1_000_000, ROOMY_PROCESS), Ok(()));
        let full_but_legal = Usage {
            bytes: RETAINED_BYTES_CAP - 1_000_000,
            sounds: RETAINED_SOUND_CAP - 1,
        };
        assert_eq!(admit(full_but_legal, 1_000_000, ROOMY_PROCESS), Ok(()));
    }

    #[test]
    fn route_reports_the_same_refusal_admit_decides() {
        // `route` hands `admit` the size it estimates from the duration, so both
        // must reach the same verdict for the same caps.
        let bytes = bytes_for_seconds(2.0);
        let cases = [
            (
                Usage {
                    bytes: 0,
                    sounds: RETAINED_SOUND_CAP,
                },
                ROOMY_PROCESS,
            ),
            (
                Usage {
                    bytes: RETAINED_BYTES_CAP - 1000,
                    sounds: 1,
                },
                ROOMY_PROCESS,
            ),
            (Usage::default(), None),
            (Usage::default(), Some(PROCESS_CEILING)),
        ];

        for (usage, process) in cases {
            let expected = admit(usage, bytes, process)
                .err()
                .unwrap_or(Refusal::NoAudio);
            let mut req = request(Some(2.0));
            req.usage = usage;
            req.process_bytes = process;
            assert_eq!(
                route(&req),
                Route::Stream(expected),
                "route and admit must not drift"
            );
        }
    }

    #[test]
    fn every_refusal_explains_itself_differently() {
        let all = [
            Refusal::PreloadOff,
            Refusal::UnknownDuration,
            Refusal::TooLong,
            Refusal::TooLarge,
            Refusal::NoAudio,
            Refusal::SoundCap,
            Refusal::ByteCap,
            Refusal::ProcessUnreadable,
            Refusal::ProcessCeiling,
        ];
        let mut messages: Vec<String> = all.iter().map(|r| r.message()).collect();
        assert!(messages.iter().all(|m| !m.trim().is_empty()));
        messages.sort();
        messages.dedup();
        assert_eq!(messages.len(), all.len(), "messages must not collide");
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
            Route::Stream(Refusal::TooLong)
        );
    }

    #[test]
    fn unknown_duration_streams() {
        assert_eq!(
            route(&request(None)),
            Route::Stream(Refusal::UnknownDuration)
        );
    }

    #[test]
    fn preload_off_streams() {
        let mut req = request(Some(2.0));
        req.retain = false;
        assert_eq!(route(&req), Route::Stream(Refusal::PreloadOff));
    }

    #[test]
    fn a_full_cache_streams() {
        let mut req = request(Some(2.0));
        req.usage = Usage {
            bytes: RETAINED_BYTES_CAP - 1000,
            sounds: 1,
        };
        assert_eq!(route(&req), Route::Stream(Refusal::ByteCap));
    }

    #[test]
    fn the_sound_count_cap_binds() {
        let usage = Usage {
            bytes: 1_000_000,
            sounds: RETAINED_SOUND_CAP,
        };
        assert_eq!(admit(usage, 100_000, ROOMY_PROCESS), Err(Refusal::SoundCap));
    }

    #[test]
    fn an_unreadable_process_size_fails_closed() {
        assert_eq!(
            admit(Usage::default(), 1_000_000, None),
            Err(Refusal::ProcessUnreadable)
        );
        let mut req = request(Some(2.0));
        req.process_bytes = None;
        assert_eq!(route(&req), Route::Stream(Refusal::ProcessUnreadable));
    }

    #[test]
    fn the_process_ceiling_binds() {
        let near = PROCESS_CEILING - PROCESS_HEADROOM - 1000;
        assert_eq!(
            admit(Usage::default(), 1_000_000, Some(near)),
            Err(Refusal::ProcessCeiling)
        );
    }
}
