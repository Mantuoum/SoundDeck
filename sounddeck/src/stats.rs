//! Opt-in performance counters for `--stats`.
//!
//! Every counter is a relaxed atomic behind a single relaxed bool, so a normal
//! build pays one predictable branch per call site and nothing else.
//!
//! A release build has no console attached (`windows_subsystem = "windows"`),
//! so the reporter appends one line every five seconds to
//! `%TEMP%\sounddeck-stats.log` instead of printing to stderr.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);

static FRAMES: AtomicU64 = AtomicU64::new(0);
static FRAME_NS: AtomicU64 = AtomicU64::new(0);
static FRAME_NS_MAX: AtomicU64 = AtomicU64::new(0);
static HIDDEN: AtomicBool = AtomicBool::new(false);
static REPAINT_MS: AtomicU32 = AtomicU32::new(0);

static MIXER_ITERS: AtomicU64 = AtomicU64::new(0);
static MIXER_RENDERS: AtomicU64 = AtomicU64::new(0);
static MIXER_SLEEPS: AtomicU64 = AtomicU64::new(0);

/// One counter pair per audio output slot (`Slot::index()`). The arrays, the
/// bounds check in [`ring_pop`] and the label loop in [`report`] all read this,
/// so a third output slot cannot be half-added: it is one number to change.
const SLOT_PAIRS: usize = 2;
static POP_CALLS: [AtomicU64; SLOT_PAIRS] = [const { AtomicU64::new(0) }; SLOT_PAIRS];
static POP_EMPTY: [AtomicU64; SLOT_PAIRS] = [const { AtomicU64::new(0) }; SLOT_PAIRS];

static VOICES: AtomicU32 = AtomicU32::new(0);
/// Voice underruns: a stream voice asked the mixer for samples and the channel
/// was empty. Non-zero means the disk buffer is too shallow.
static STREAM_UNDERRUNS: AtomicU64 = AtomicU64::new(0);
/// 0 = idle, 1 = playing, 2 = error.
static LOOP: AtomicU32 = AtomicU32::new(0);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Starts the five-second reporter. Returns the log path so the caller can
/// tell the user where the numbers land.
pub fn start_reporter() -> std::path::PathBuf {
    ENABLED.store(true, Ordering::Relaxed);
    let path = std::env::temp_dir().join("sounddeck-stats.log");
    let written = path.clone();
    std::thread::Builder::new()
        .name("sounddeck-stats".into())
        .spawn(move || {
            let mut last = Instant::now();
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let elapsed = last.elapsed().as_secs_f64().max(0.001);
                last = Instant::now();
                let line = report(elapsed);
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&written)
                {
                    let _ = writeln!(file, "{line}");
                }
            }
        })
        .ok();
    path
}

fn report(elapsed: f64) -> String {
    let frames = FRAMES.swap(0, Ordering::Relaxed);
    let frame_ns = FRAME_NS.swap(0, Ordering::Relaxed);
    let frame_max = FRAME_NS_MAX.swap(0, Ordering::Relaxed);
    let iters = MIXER_ITERS.swap(0, Ordering::Relaxed);
    let renders = MIXER_RENDERS.swap(0, Ordering::Relaxed);
    let sleeps = MIXER_SLEEPS.swap(0, Ordering::Relaxed);
    // One segment per slot pair: the loop covers every counter the module has,
    // so adding a slot cannot leave its numbers unreported.
    let mut pops = String::new();
    for slot in 0..SLOT_PAIRS {
        let calls = POP_CALLS[slot].swap(0, Ordering::Relaxed) as f64 / elapsed;
        let empty = POP_EMPTY[slot].swap(0, Ordering::Relaxed) as f64 / elapsed;
        pops.push_str(&format!(" pop{slot}={calls:.0}/{empty:.0}"));
    }
    let stream_underruns = STREAM_UNDERRUNS.swap(0, Ordering::Relaxed);

    let fps = frames as f64 / elapsed;
    let avg_ms = if frames == 0 {
        0.0
    } else {
        frame_ns as f64 / frames as f64 / 1_000_000.0
    };
    let loop_label = match LOOP.load(Ordering::Relaxed) {
        1 => "playing",
        2 => "error",
        _ => "idle",
    };
    format!(
        "[stats] fps={fps:.1} frame_avg_ms={avg_ms:.2} frame_max_ms={:.2} hidden={} repaint={}ms \
         mixer_iters={:.0} mixer_renders={:.0} mixer_sleeps={:.0}{pops} \
         voices={} loop={loop_label} stream_underruns={}",
        frame_max as f64 / 1_000_000.0,
        HIDDEN.load(Ordering::Relaxed) as u8,
        REPAINT_MS.load(Ordering::Relaxed),
        iters as f64 / elapsed,
        renders as f64 / elapsed,
        sleeps as f64 / elapsed,
        VOICES.load(Ordering::Relaxed),
        stream_underruns,
    )
}

/// One stream voice found the channel empty when it wanted samples.
pub fn stream_underrun() {
    if enabled() {
        STREAM_UNDERRUNS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_frame(duration: Duration, hidden: bool, repaint_ms: u32) {
    if !enabled() {
        return;
    }
    let ns = duration.as_nanos().min(u64::MAX as u128) as u64;
    FRAMES.fetch_add(1, Ordering::Relaxed);
    FRAME_NS.fetch_add(ns, Ordering::Relaxed);
    FRAME_NS_MAX.fetch_max(ns, Ordering::Relaxed);
    HIDDEN.store(hidden, Ordering::Relaxed);
    REPAINT_MS.store(repaint_ms, Ordering::Relaxed);
}

pub fn mixer_iteration() {
    if enabled() {
        MIXER_ITERS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn mixer_render() {
    if enabled() {
        MIXER_RENDERS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn mixer_sleep() {
    if enabled() {
        MIXER_SLEEPS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Records one consumer-side ring read; `frames == 0` means the device had to
/// pad silence (an underrun).
pub fn ring_pop(slot: usize, frames: usize) {
    if !enabled() {
        return;
    }
    // The slot comes from a device route; an unexpected index must skip the
    // counter, not panic (there is one counter pair per output slot).
    if slot >= SLOT_PAIRS {
        return;
    }
    if frames == 0 {
        POP_EMPTY[slot].fetch_add(1, Ordering::Relaxed);
    } else {
        POP_CALLS[slot].fetch_add(1, Ordering::Relaxed);
    }
}

pub fn set_voices(count: usize) {
    if enabled() {
        VOICES.store(count.min(u32::MAX as usize) as u32, Ordering::Relaxed);
    }
}

pub fn set_loop(code: u32) {
    if enabled() {
        LOOP.store(code, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_pop_should_not_panic_when_slot_is_out_of_range() {
        ENABLED.store(true, Ordering::Relaxed);
        ring_pop(2, 480);
        ring_pop(usize::MAX, 0);
        ring_pop(0, 480);
        ENABLED.store(false, Ordering::Relaxed);
    }

    /// The label loop is what keeps a new output slot out of the "silently
    /// dropped" class: every counter pair must appear in the report line.
    #[test]
    fn report_should_name_every_slot_pair() {
        let line = report(1.0);
        for slot in 0..SLOT_PAIRS {
            assert!(line.contains(&format!("pop{slot}=")), "{line}");
        }
        assert!(line.contains("stream_underruns="), "{line}");
    }
}
