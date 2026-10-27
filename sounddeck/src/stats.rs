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

static POP_CALLS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static POP_EMPTY: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

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
    let pop = [
        POP_CALLS[0].swap(0, Ordering::Relaxed),
        POP_CALLS[1].swap(0, Ordering::Relaxed),
    ];
    let empty = [
        POP_EMPTY[0].swap(0, Ordering::Relaxed),
        POP_EMPTY[1].swap(0, Ordering::Relaxed),
    ];
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
         mixer_iters={:.0} mixer_renders={:.0} mixer_sleeps={:.0} \
         pop0={:.0}/{:.0} pop1={:.0}/{:.0} voices={} loop={loop_label} stream_underruns={}",
        frame_max as f64 / 1_000_000.0,
        HIDDEN.load(Ordering::Relaxed) as u8,
        REPAINT_MS.load(Ordering::Relaxed),
        iters as f64 / elapsed,
        renders as f64 / elapsed,
        sleeps as f64 / elapsed,
        pop[0] as f64 / elapsed,
        empty[0] as f64 / elapsed,
        pop[1] as f64 / elapsed,
        empty[1] as f64 / elapsed,
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
