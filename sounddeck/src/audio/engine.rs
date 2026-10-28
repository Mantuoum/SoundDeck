use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, Sender, TryRecvError};

use crate::stats;

use super::decode::{CHANNELS, SAMPLE_RATE};
use super::stream::{LoopState, LoopStream};
use super::wasapi;

const MIX_CHUNK_FRAMES: usize = 240; // 5 ms
const FADE_IN_FRAMES: f32 = 120.0; // 2.5 ms
const FADE_OUT_FRAMES: f32 = 240.0; // 5 ms
/// Attempts at starting the mixer thread before audio is declared dead. One
/// immediate retry covers a transient OS refusal; beyond that the environment is
/// broken and the user has to be told, not left with silent audio.
const MIXER_SPAWN_ATTEMPTS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Monitor = 0,
    Cable = 1,
}

impl Slot {
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn label(self) -> &'static str {
        match self {
            Slot::Monitor => "Monitor",
            Slot::Cable => "Cable",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceRoute {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub enum AudioEvent {
    DeviceStarted {
        slot: Slot,
    },
    DeviceError {
        slot: Slot,
        name: String,
        message: String,
    },
    Warning(String),
    Stopped {
        slot: Slot,
    },
}

/// A single producer / single consumer stereo sample ring.
pub struct Ring {
    queue: Mutex<VecDeque<f32>>,
    capacity_samples: usize,
    consumers: AtomicUsize,
    /// Peak of the samples handed to the device since the last UI read.
    peak: AtomicU32,
}

impl Ring {
    pub fn new(capacity_seconds: f32) -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(16_384)),
            capacity_samples: (SAMPLE_RATE as f32 * CHANNELS as f32 * capacity_seconds) as usize,
            consumers: AtomicUsize::new(0),
            peak: AtomicU32::new(0),
        }
    }

    pub fn add_consumer(&self) {
        self.consumers.fetch_add(1, Ordering::SeqCst);
    }

    pub fn remove_consumer(&self) {
        self.consumers.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn has_consumers(&self) -> bool {
        self.consumers.load(Ordering::Relaxed) > 0
    }

    pub fn push_scaled(&self, interleaved: &[f32], gain: f32) {
        let mut queue = match self.queue.lock() {
            Ok(q) => q,
            Err(_) => return,
        };
        let total = queue.len() + interleaved.len();
        if total > self.capacity_samples {
            let excess = total - self.capacity_samples;
            let drop = excess.min(queue.len());
            queue.drain(..drop);
        }
        if gain == 1.0 {
            queue.extend(interleaved.iter().copied());
        } else {
            queue.extend(interleaved.iter().map(|s| s * gain));
        }
    }

    pub fn pop_into(&self, out: &mut VecDeque<f32>, max_frames: usize) -> usize {
        let mut queue = self.queue.lock().unwrap_or_else(|err| err.into_inner());
        // Whole frames only. A queue length that is not a multiple of `CHANNELS`
        // can only come from a malformed push; the stray tail sample is dropped
        // rather than carried, because carrying it would shift every later frame
        // by one sample (L/R swap) for the life of the ring.
        let stray = queue.len() % CHANNELS;
        if stray != 0 {
            let keep = queue.len() - stray;
            queue.truncate(keep);
        }
        let frames = max_frames.saturating_mul(CHANNELS).min(queue.len()) / CHANNELS;
        let mut peak = 0.0f32;
        for _ in 0..frames * CHANNELS {
            if let Some(sample) = queue.pop_front() {
                peak = peak.max(sample.abs());
                out.push_back(sample);
            }
        }
        if peak > 0.0 {
            // Non-negative f32 bit patterns are ordered like the floats.
            self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
        frames
    }

    /// Peak since the previous call, as linear amplitude.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    pub fn clear(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.clear();
        }
        self.peak.store(0, Ordering::Relaxed);
    }

    pub fn buffered_frames(&self) -> usize {
        self.queue.lock().map(|q| q.len() / CHANNELS).unwrap_or(0)
    }
}

enum VoiceSource {
    /// Samples held in RAM.
    Buffer(Arc<Vec<f32>>),
    /// Samples pulled from a disk stream as the mixer consumes them.
    Stream {
        rx: Receiver<Vec<f32>>,
        chunk: Vec<f32>,
        pos: usize,
        ended: bool,
    },
}

struct Voice {
    sound_id: u64,
    source: VoiceSource,
    /// Frames rendered so far. Drives fades, stop points and progress.
    position: usize,
    gain: f32,
    stop_at: Option<usize>,
    finished: bool,
    total_frames: Option<usize>,
}

impl Voice {
    fn total_frames(&self) -> Option<usize> {
        match &self.source {
            VoiceSource::Buffer(samples) => Some(samples.len() / CHANNELS),
            VoiceSource::Stream { .. } => self.total_frames,
        }
    }

    fn next_frame(&mut self) -> Option<(f32, f32)> {
        match &mut self.source {
            VoiceSource::Buffer(samples) => {
                let index = self.position.checked_mul(CHANNELS)?;
                frame_at(samples, index)
            }
            VoiceSource::Stream {
                rx,
                chunk,
                pos,
                ended,
            } => loop {
                // Whole frames only: a chunk with an odd length (a truncated
                // final frame) yields its complete frames and then is discarded
                // rather than indexed past its end.
                if let Some(frame) = frame_at(chunk, *pos) {
                    *pos += CHANNELS;
                    return Some(frame);
                }
                chunk.clear();
                *pos = 0;
                if *ended {
                    return None;
                }
                match rx.try_recv() {
                    Ok(next) => *chunk = next,
                    Err(TryRecvError::Empty) => {
                        // The first render after a trigger happens before the
                        // decoder delivers anything; that initial silence is not
                        // an underrun. Only count starvation after audio started.
                        if self.position > 0 {
                            stats::stream_underrun();
                        }
                        return None;
                    }
                    Err(TryRecvError::Disconnected) => {
                        *ended = true;
                        return None;
                    }
                }
            },
        }
    }

    /// Nothing left to play, regardless of stop fades.
    fn exhausted(&self) -> bool {
        match &self.source {
            VoiceSource::Buffer(samples) => {
                frame_at(samples, self.position.saturating_mul(CHANNELS)).is_none()
            }
            VoiceSource::Stream {
                chunk, pos, ended, ..
            } => *ended && frame_at(chunk, *pos).is_none(),
        }
    }
}

/// The left/right pair starting at sample index `pos`, or `None` when fewer than
/// `CHANNELS` samples remain. Never indexes out of bounds, whatever the chunk
/// length (chunk parity is an invariant of the producer, not of this reader).
fn frame_at(samples: &[f32], pos: usize) -> Option<(f32, f32)> {
    match samples.get(pos..pos.checked_add(CHANNELS)?) {
        Some([left, right]) => Some((*left, *right)),
        _ => None,
    }
}

struct Mixer {
    voices: Vec<Voice>,
    monitor_gain: f32,
    cable_gain: f32,
    master_gain: f32,
    /// Streaming loop track. Kept out of `voices`: it never takes part in
    /// exclusive playback and is fed from disk instead of RAM.
    loop_rx: Option<Receiver<Vec<f32>>>,
    loop_chunk: Vec<f32>,
    loop_pos: usize,
    loop_gain: f32,
    loop_on_monitor: bool,
    loop_on_cable: bool,
}

impl Mixer {
    /// True when a render pass would produce something. Deliberately ignores
    /// the loop routing switches: the chunk must keep advancing while both
    /// outputs are muted so re-enabling is seamless.
    fn has_work(&self) -> bool {
        !self.voices.is_empty() || self.loop_rx.is_some()
    }

    fn render(&mut self, monitor: &mut [f32], cable: &mut [f32]) {
        debug_assert_eq!(monitor.len(), cable.len());
        if monitor.len() != cable.len() {
            return;
        }
        let frames = monitor.len() / CHANNELS;
        for voice in &mut self.voices {
            let mut produced = 0usize;
            while produced < frames {
                let Some((left, right)) = voice.next_frame() else {
                    break;
                };
                let frame = voice.position;
                let mut env = 1.0f32;
                if (frame as f32) < FADE_IN_FRAMES {
                    env *= frame as f32 / FADE_IN_FRAMES;
                }
                if let Some(stop_at) = voice.stop_at {
                    if frame >= stop_at {
                        let faded = (frame - stop_at) as f32 / FADE_OUT_FRAMES;
                        env *= (1.0 - faded).max(0.0);
                    }
                }
                let dst = produced * CHANNELS;
                let gain = voice.gain * env;
                monitor[dst] += left * gain;
                monitor[dst + 1] += right * gain;
                voice.position += 1;
                produced += 1;
            }

            let fade_done = voice
                .stop_at
                .map(|stop_at| voice.position >= stop_at + FADE_OUT_FRAMES as usize)
                .unwrap_or(false);
            // A starved stream during a stop fade would otherwise hold the voice
            // forever, because its position never advances.
            let stalled_stop = voice.stop_at.is_some() && produced == 0;
            if fade_done || stalled_stop || voice.exhausted() {
                voice.finished = true;
            }
        }

        // Sound effects are common to both outputs; the loop is routed
        // independently below.
        cable.copy_from_slice(monitor);

        // Loop track: consumed once, written to the outputs the user enabled.
        // An empty channel means silence for the rest of this render, not a stop.
        if let Some(rx) = self.loop_rx.as_ref() {
            let mut index = 0usize;
            while index < frames {
                if self.loop_pos >= self.loop_chunk.len() {
                    match rx.try_recv() {
                        Ok(chunk) => {
                            self.loop_chunk = chunk;
                            self.loop_pos = 0;
                        }
                        Err(TryRecvError::Empty) => break,
                        // The decoder is gone (error or stop): drop the receiver
                        // so `has_work` can park the mixer instead of polling a
                        // channel that will never produce again.
                        Err(TryRecvError::Disconnected) => {
                            self.loop_rx = None;
                            self.loop_chunk.clear();
                            self.loop_pos = 0;
                            break;
                        }
                    }
                }

                let available = (self.loop_chunk.len() - self.loop_pos) / CHANNELS;
                if available == 0 {
                    self.loop_chunk.clear();
                    self.loop_pos = 0;
                    break;
                }
                let take = (frames - index).min(available);
                let len = take * CHANNELS;
                let source = &self.loop_chunk[self.loop_pos..self.loop_pos + len];
                let start = index * CHANNELS;
                let gain = self.loop_gain;
                // One slice-wise write per destination instead of a per-sample
                // branch on the routing flags; at unity gain this is a plain add.
                if self.loop_on_monitor {
                    add_scaled(&mut monitor[start..start + len], source, gain);
                }
                if self.loop_on_cable {
                    add_scaled(&mut cable[start..start + len], source, gain);
                }
                self.loop_pos += len;
                index += take;
            }
        }

        apply_master_gain(monitor, self.master_gain);
        apply_master_gain(cable, self.master_gain);
        self.voices.retain(|v| !v.finished);
    }

    fn active(&self) -> Vec<ActiveSound> {
        self.voices
            .iter()
            .filter(|v| !v.finished && v.stop_at.is_none())
            .map(|v| {
                let total = v.total_frames().unwrap_or(0).max(1) as f32;
                ActiveSound {
                    sound_id: v.sound_id,
                    progress: (v.position as f32 / total).clamp(0.0, 1.0),
                }
            })
            .collect()
    }
}

pub struct ActiveSound {
    pub sound_id: u64,
    pub progress: f32,
}

struct DeviceHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    id: String,
}

impl DeviceHandle {
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// A handle whose thread already exited must not be treated as a live
    /// device, otherwise a failed endpoint can never be reopened.
    fn alive(&self) -> bool {
        self.join
            .as_ref()
            .map(|join| !join.is_finished())
            .unwrap_or(false)
    }
}

pub struct Engine {
    mixer: Arc<Mutex<Mixer>>,
    rings: [Arc<Ring>; 2],
    devices: Mutex<[Option<DeviceHandle>; 2]>,
    events_tx: Sender<AudioEvent>,
    events_rx: Receiver<AudioEvent>,
    mixer_stop: Arc<AtomicBool>,
    /// Capacity-one wake token: a producer publishes work with `try_send`, the
    /// mixer parks on the receiving end when it has nothing to render. `None`
    /// means the mixer thread never started (see `mixer_failure`).
    mixer_wake: Option<Sender<()>>,
    mixer_thread: Mutex<Option<JoinHandle<()>>>,
    /// Reason the mixer thread could not be started. Set means nothing can
    /// render; every trigger reports it instead of failing silently.
    mixer_failure: Option<String>,
    loop_stream: Mutex<Option<LoopStream>>,
}

impl Engine {
    pub fn new(monitor_volume: f32, cable_volume: f32, master_gain_db: f32) -> Self {
        let mixer = Arc::new(Mutex::new(Mixer {
            voices: Vec::new(),
            monitor_gain: monitor_volume.clamp(0.0, 1.0),
            cable_gain: cable_volume.clamp(0.0, 1.0),
            master_gain: db_to_gain(master_gain_db),
            loop_rx: None,
            loop_chunk: Vec::new(),
            loop_pos: 0,
            loop_gain: 1.0,
            loop_on_monitor: true,
            loop_on_cable: true,
        }));
        let rings = [Arc::new(Ring::new(0.15)), Arc::new(Ring::new(0.15))];
        let (events_tx, events_rx) = unbounded();
        let mixer_stop = Arc::new(AtomicBool::new(false));

        // Each attempt gets its own wake pair: a failed spawn drops the closure
        // (and the receiver with it), so reusing one channel would leave the
        // sender permanently disconnected and the engine silently dead.
        let mut mixer_wake = None;
        let mut mixer_thread = None;
        let mut mixer_failure = None;
        for _ in 0..MIXER_SPAWN_ATTEMPTS {
            let (wake, wake_rx) = bounded::<()>(1);
            let mixer = mixer.clone();
            let rings = rings.clone();
            let stop = mixer_stop.clone();
            let spawned = thread::Builder::new()
                .name("sounddeck-mixer".into())
                .spawn(move || {
                    super::raise_thread_priority();
                    mixer_loop(mixer, rings, stop, wake_rx)
                });
            match spawned {
                Ok(handle) => {
                    mixer_wake = Some(wake);
                    mixer_thread = Some(handle);
                    mixer_failure = None;
                    break;
                }
                Err(err) => {
                    mixer_failure = Some(format!("the audio mixer thread could not start: {err}"))
                }
            }
        }

        if let Some(reason) = &mixer_failure {
            // Loud once at boot, then again on every trigger via
            // `report_mixer_failure`.
            let _ = events_tx.send(AudioEvent::Warning(format!(
                "No audio: {reason}. No sound will play until the app is restarted."
            )));
        }

        Self {
            mixer,
            rings,
            devices: Mutex::new([None, None]),
            events_tx,
            events_rx,
            mixer_stop,
            mixer_wake,
            mixer_thread: Mutex::new(mixer_thread),
            mixer_failure,
            loop_stream: Mutex::new(None),
        }
    }

    pub fn set_routes(&self, monitor: Option<DeviceRoute>, cable: Option<DeviceRoute>) {
        let mut desired = [monitor, cable];
        let same_id = match (&desired[0], &desired[1]) {
            (Some(a), Some(b)) => a.id == b.id,
            _ => false,
        };
        if same_id {
            let name = desired[0]
                .as_ref()
                .map(|r| r.name.clone())
                .unwrap_or_default();
            let _ = self.events_tx.send(AudioEvent::Warning(format!(
                "\"{name}\" is selected for both Monitor and Cable. Using it once."
            )));
            desired[1] = None;
        }

        let Ok(mut devices) = self.devices.lock() else {
            return;
        };

        for slot in [Slot::Monitor, Slot::Cable] {
            let index = slot.index();
            let wanted_id = desired[index].as_ref().map(|r| r.id.clone());
            let current_id = devices[index].as_ref().map(|d| d.id.clone());
            let current_alive = devices[index]
                .as_ref()
                .map(DeviceHandle::alive)
                .unwrap_or(false);
            if current_id == wanted_id && current_alive {
                continue;
            }
            if let Some(mut old) = devices[index].take() {
                old.shutdown();
            }
            self.rings[index].clear();
            if let Some(route) = desired[index].take() {
                let id = route.id.clone();
                let stop = Arc::new(AtomicBool::new(false));
                let join = wasapi::spawn_render(
                    route.id,
                    route.name,
                    slot,
                    self.rings[index].clone(),
                    self.events_tx.clone(),
                    stop.clone(),
                );
                devices[index] = Some(DeviceHandle { stop, join, id });
            }
        }
    }

    pub fn play(&self, sound_id: u64, samples: Arc<Vec<f32>>, volume: f32) {
        if self.report_mixer_failure() {
            return;
        }
        if !is_valid_buffer(&samples) {
            let _ = self.events_tx.send(AudioEvent::Warning(format!(
                "Refused to play sound {sound_id}: invalid sample buffer"
            )));
            return;
        }
        {
            let Ok(mut mixer) = self.mixer.lock() else {
                return;
            };
            // Exclusive playback: fade out anything that is still playing (including
            // a previous instance of this same sound) so triggers never stack.
            for voice in &mut mixer.voices {
                if voice.stop_at.is_none() {
                    voice.stop_at = Some(voice.position);
                }
            }
            mixer.voices.push(Voice {
                sound_id,
                source: VoiceSource::Buffer(samples),
                position: 0,
                gain: volume.clamp(0.0, 1.0),
                stop_at: None,
                finished: false,
                total_frames: None,
            });
        }
        // Publish after the lock is released so the mixer cannot wake, block on
        // the lock and then find stale state.
        self.wake_mixer();
    }

    /// Plays a sound straight from a disk stream. Same exclusive-playback rules
    /// as [`Engine::play`]; memory is one bounded channel, whatever the length.
    pub fn play_stream(
        &self,
        sound_id: u64,
        rx: Receiver<Vec<f32>>,
        volume: f32,
        total_frames: Option<usize>,
    ) {
        if self.report_mixer_failure() {
            return;
        }
        {
            let Ok(mut mixer) = self.mixer.lock() else {
                return;
            };
            for voice in &mut mixer.voices {
                if voice.stop_at.is_none() {
                    voice.stop_at = Some(voice.position);
                }
            }
            mixer.voices.push(Voice {
                sound_id,
                source: VoiceSource::Stream {
                    rx,
                    chunk: Vec::new(),
                    pos: 0,
                    ended: false,
                },
                position: 0,
                gain: volume.clamp(0.0, 1.0),
                stop_at: None,
                finished: false,
                total_frames,
            });
        }
        self.wake_mixer();
    }

    /// Tells the user (again) that the mixer never started. Called from every
    /// trigger so a dead audio engine can never be mistaken for a quiet one.
    /// Returns `true` when audio is unavailable.
    fn report_mixer_failure(&self) -> bool {
        match self.mixer_failure.as_deref() {
            Some(reason) => {
                let _ = self.events_tx.send(AudioEvent::Warning(format!(
                    "No audio: {reason}. Nothing will play until the app is restarted."
                )));
                true
            }
            None => false,
        }
    }

    fn wake_mixer(&self) {
        if let Some(wake) = &self.mixer_wake {
            let _ = wake.try_send(());
        }
    }

    pub fn stop_sound(&self, sound_id: u64) {
        if let Ok(mut mixer) = self.mixer.lock() {
            for voice in &mut mixer.voices {
                if voice.sound_id == sound_id && voice.stop_at.is_none() {
                    voice.stop_at = Some(voice.position);
                }
            }
        }
    }

    pub fn stop_all(&self) {
        if let Ok(mut mixer) = self.mixer.lock() {
            for voice in &mut mixer.voices {
                if voice.stop_at.is_none() {
                    voice.stop_at = Some(voice.position);
                }
            }
        }
    }

    /// Starts the background loop. Replaces any loop already playing.
    ///
    /// The decoder is attached immediately; audio begins as soon as the first
    /// chunk arrives (a few milliseconds), so this never blocks the UI.
    pub fn start_loop(&self, path: PathBuf) -> Result<()> {
        if let Some(reason) = self.mixer_failure.as_deref() {
            bail!("No audio: {reason}");
        }
        if !path.is_file() {
            bail!("File not found: {}", path.display());
        }
        self.stop_loop();

        let status = Arc::new(Mutex::new(LoopState::Playing));
        let (stream, rx) = LoopStream::spawn(path, status)?;
        {
            let Ok(mut mixer) = self.mixer.lock() else {
                bail!("Audio mixer is unavailable");
            };
            mixer.loop_rx = Some(rx);
            mixer.loop_chunk.clear();
            mixer.loop_pos = 0;
        }
        if let Ok(mut slot) = self.loop_stream.lock() {
            *slot = Some(stream);
        }
        self.wake_mixer();
        Ok(())
    }

    /// Stops the loop immediately. Dropping the receiver unblocks the decoder
    /// even if it is parked in `send`, so the join is bounded.
    ///
    /// Poison-tolerant on purpose: a panicked mixer must not leave the receiver
    /// in place, because the decoder would then block in `send` forever and the
    /// join at shutdown would hang.
    pub fn stop_loop(&self) {
        let rx = {
            let mut mixer = self.mixer.lock().unwrap_or_else(|err| err.into_inner());
            mixer.loop_chunk.clear();
            mixer.loop_pos = 0;
            mixer.loop_rx.take()
        };
        drop(rx);

        let stream = self
            .loop_stream
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take();
        if let Some(stream) = stream {
            stream.stop();
        }
    }

    pub fn set_loop_volume(&self, volume: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.loop_gain = volume.clamp(0.0, 1.0);
        }
    }

    pub fn set_loop_outputs(&self, monitor: bool, cable: bool) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.loop_on_monitor = monitor;
            mixer.loop_on_cable = cable;
        }
    }

    pub fn loop_state(&self) -> LoopState {
        if let Ok(slot) = self.loop_stream.lock() {
            if let Some(stream) = slot.as_ref() {
                let state = stream.state();
                if state.error().is_some() {
                    return state;
                }
                if stream.finished() {
                    return LoopState::Error("Loop track stopped".into());
                }
                return state;
            }
        }
        LoopState::Idle
    }

    pub fn active(&self) -> Vec<ActiveSound> {
        self.mixer
            .lock()
            .map(|mixer| mixer.active())
            .unwrap_or_default()
    }

    /// Cheap "is anything sounding" check for the UI repaint policy. Unlike
    /// [`Engine::active`] this does not allocate.
    pub fn is_playing(&self) -> bool {
        self.mixer
            .lock()
            .map(|mixer| !mixer.voices.is_empty())
            .unwrap_or(false)
    }

    /// Whether this specific sound is currently sounding. Used by the cache to
    /// avoid evicting a buffer the mixer is still reading. Fails closed: if the
    /// mixer lock is poisoned the caller is told "yes, playing".
    pub fn is_playing_sound(&self, sound_id: u64) -> bool {
        self.mixer
            .lock()
            .map(|mixer| mixer.voices.iter().any(|voice| voice.sound_id == sound_id))
            .unwrap_or(true)
    }

    /// Peak level per output since the previous call, as linear amplitude.
    /// Measured where the device consumes samples, so it matches what is heard.
    pub fn take_levels(&self) -> [f32; 2] {
        [self.rings[0].take_peak(), self.rings[1].take_peak()]
    }

    pub fn set_master(&self, slot: Slot, volume: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            match slot {
                Slot::Monitor => mixer.monitor_gain = volume.clamp(0.0, 1.0),
                Slot::Cable => mixer.cable_gain = volume.clamp(0.0, 1.0),
            }
        }
    }

    pub fn set_master_gain_db(&self, db: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.master_gain = db_to_gain(db);
        }
    }

    pub fn set_sound_volume(&self, sound_id: u64, volume: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            for voice in &mut mixer.voices {
                if voice.sound_id == sound_id {
                    voice.gain = volume.clamp(0.0, 1.0);
                }
            }
        }
    }

    pub fn poll_events(&self) -> Vec<AudioEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events_rx.try_recv() {
            events.push(event);
        }
        events
    }

    pub fn buffered_frames(&self) -> (usize, usize) {
        (
            self.rings[0].buffered_frames(),
            self.rings[1].buffered_frames(),
        )
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop_loop();
        if let Ok(mut devices) = self.devices.lock() {
            for slot in 0..2 {
                if let Some(mut handle) = devices[slot].take() {
                    handle.shutdown();
                }
            }
        }
        self.mixer_stop.store(true, Ordering::SeqCst);
        self.wake_mixer();
        if let Ok(mut thread) = self.mixer_thread.lock() {
            if let Some(join) = thread.take() {
                let _ = join.join();
            }
        }
    }
}

/// Samples accepted by the mixer: non-empty, whole frames, and finite. A NaN or
/// infinite sample would poison the mix (and the device conversion) for as long
/// as the buffer is playing, so it is refused up front.
fn is_valid_buffer(samples: &[f32]) -> bool {
    !samples.is_empty() && samples.len() % CHANNELS == 0 && samples.iter().all(|s| s.is_finite())
}

/// `dst += src * gain` over a whole slice. `gain == 1.0` skips the multiply so
/// the unity path is a straight add (the same idea as `Ring::push_scaled`).
fn add_scaled(dst: &mut [f32], src: &[f32], gain: f32) {
    debug_assert_eq!(dst.len(), src.len());
    if gain == 1.0 {
        for (target, sample) in dst.iter_mut().zip(src) {
            *target += *sample;
        }
    } else {
        for (target, sample) in dst.iter_mut().zip(src) {
            *target += *sample * gain;
        }
    }
}

/// `buf = (buf * gain).clamp(-1, 1)`, the final stage before the rings. At unity
/// gain the multiply is skipped (`x * 1.0 == x`, so the output is unchanged).
fn apply_master_gain(buf: &mut [f32], gain: f32) {
    if gain == 1.0 {
        for sample in buf.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    } else {
        for sample in buf.iter_mut() {
            *sample = (*sample * gain).clamp(-1.0, 1.0);
        }
    }
}

fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db.clamp(-60.0, 12.0) / 20.0)
}

fn mixer_loop(
    mixer: Arc<Mutex<Mixer>>,
    rings: [Arc<Ring>; 2],
    stop: Arc<AtomicBool>,
    wake: Receiver<()>,
) {
    /// How long the mixer parks when nothing is playing. A producer wake normally
    /// releases it sooner; this only bounds the damage from a missed wake.
    const IDLE_PARK: Duration = Duration::from_millis(50);

    let mut monitor_buffer = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
    let mut cable_buffer = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
    let mut next = Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        stats::mixer_iteration();

        let has_work = match mixer.lock() {
            Ok(mixer) => mixer.has_work(),
            Err(_) => break,
        };
        if !has_work {
            stats::mixer_sleep();
            match wake.recv_timeout(IDLE_PARK) {
                Ok(()) => {
                    // Work was published while we were asleep; start a fresh
                    // schedule instead of bursting to catch up on idle time.
                    next = Instant::now();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            continue;
        }

        let due = next.saturating_duration_since(Instant::now());
        if !due.is_zero() {
            match wake.recv_timeout(due) {
                Ok(()) => next = Instant::now(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        let behind = Instant::now().saturating_duration_since(next);
        let chunks = ((behind.as_micros() / 5_000) as usize).clamp(1, 4);
        for _ in 0..chunks {
            monitor_buffer.fill(0.0);
            cable_buffer.fill(0.0);
            let (monitor_gain, cable_gain, voices) = {
                match mixer.lock() {
                    Ok(mut mixer) => {
                        mixer.render(&mut monitor_buffer, &mut cable_buffer);
                        (mixer.monitor_gain, mixer.cable_gain, mixer.voices.len())
                    }
                    Err(_) => break,
                }
            };
            stats::mixer_render();
            stats::set_voices(voices);
            if rings[0].has_consumers() {
                rings[0].push_scaled(&monitor_buffer, monitor_gain);
            }
            if rings[1].has_consumers() {
                rings[1].push_scaled(&cable_buffer, cable_gain);
            }
            next += Duration::from_millis(5);
        }
    }
}

#[cfg(test)]
mod loop_tests {
    use super::*;
    use std::fs;
    use std::time::Instant;

    fn write_wav(path: &std::path::Path, seconds: f32) {
        let rate = 48_000u32;
        let frames = (rate as f32 * seconds) as u32;
        let mut data = Vec::new();
        for i in 0..frames {
            let sample = ((i as f32 / rate as f32) * 220.0 * std::f32::consts::TAU).sin() * 0.3;
            let value = (sample * 32_767.0) as i16;
            data.extend_from_slice(&value.to_le_bytes());
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        fs::write(path, wav).unwrap();
    }

    #[test]
    fn start_reaches_playing_and_stop_is_idle() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sounddeck-engine-loop-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("loop.wav");
        write_wav(&path, 0.2);

        let engine = Engine::new(1.0, 1.0, 0.0);
        engine.start_loop(path).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && engine.loop_state() != LoopState::Playing {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(engine.loop_state(), LoopState::Playing);

        engine.stop_loop();
        assert_eq!(engine.loop_state(), LoopState::Idle);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_rejected_without_a_stream() {
        let engine = Engine::new(1.0, 1.0, 0.0);
        assert!(engine
            .start_loop(PathBuf::from("Z:\\no-such-loop-file.wav"))
            .is_err());
        assert_eq!(engine.loop_state(), LoopState::Idle);
    }

    #[test]
    fn ring_peak_tracks_consumed_samples() {
        let ring = Ring::new(0.1);
        ring.push_scaled(&[0.5f32, -0.25, 0.1, -0.1], 1.0);
        let mut out = VecDeque::new();
        assert_eq!(ring.pop_into(&mut out, 2), 2);
        assert!((ring.take_peak() - 0.5).abs() < 1e-6);
        assert_eq!(ring.take_peak(), 0.0);
    }

    #[test]
    fn ring_peak_applies_slot_gain() {
        let ring = Ring::new(0.1);
        ring.push_scaled(&[0.8f32, -0.8], 0.5);
        let mut out = VecDeque::new();
        ring.pop_into(&mut out, 1);
        assert!((ring.take_peak() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn mixer_loop_gain_scales_the_loop() {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let mut mixer = Mixer {
            voices: Vec::new(),
            monitor_gain: 1.0,
            cable_gain: 1.0,
            master_gain: 1.0,
            loop_rx: Some(rx),
            loop_chunk: Vec::new(),
            loop_pos: 0,
            loop_gain: 0.25,
            loop_on_monitor: true,
            loop_on_cable: true,
        };
        tx.send(vec![0.8f32; MIX_CHUNK_FRAMES * CHANNELS]).unwrap();

        let mut monitor = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        let mut cable = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        mixer.render(&mut monitor, &mut cable);
        for out in [&monitor, &cable] {
            assert!(out.iter().all(|sample| (*sample - 0.2).abs() < 1e-6));
        }
    }

    #[test]
    fn mixer_adds_the_loop_before_master_gain() {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let mut mixer = Mixer {
            voices: Vec::new(),
            monitor_gain: 1.0,
            cable_gain: 1.0,
            master_gain: 0.5,
            loop_rx: Some(rx),
            loop_chunk: Vec::new(),
            loop_pos: 0,
            loop_gain: 1.0,
            loop_on_monitor: true,
            loop_on_cable: true,
        };
        tx.send(vec![0.25f32; MIX_CHUNK_FRAMES * CHANNELS]).unwrap();

        let mut monitor = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        let mut cable = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        mixer.render(&mut monitor, &mut cable);
        for out in [&monitor, &cable] {
            assert!(out.iter().all(|sample| (*sample - 0.125).abs() < 1e-6));
        }

        // Channel drained: silence, not a stop.
        monitor.fill(0.0);
        cable.fill(0.0);
        mixer.render(&mut monitor, &mut cable);
        assert!(monitor.iter().all(|sample| *sample == 0.0));
        assert!(cable.iter().all(|sample| *sample == 0.0));
        assert!(mixer.loop_rx.is_some());
    }

    #[test]
    fn loop_routes_to_selected_outputs() {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let mut mixer = Mixer {
            voices: Vec::new(),
            monitor_gain: 1.0,
            cable_gain: 1.0,
            master_gain: 1.0,
            loop_rx: Some(rx),
            loop_chunk: Vec::new(),
            loop_pos: 0,
            loop_gain: 1.0,
            loop_on_monitor: false,
            loop_on_cable: true,
        };
        tx.send(vec![0.4f32; MIX_CHUNK_FRAMES * CHANNELS]).unwrap();

        let mut monitor = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        let mut cable = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        mixer.render(&mut monitor, &mut cable);
        assert!(monitor.iter().all(|sample| *sample == 0.0));
        assert!(cable.iter().all(|sample| (*sample - 0.4).abs() < 1e-6));

        // Both outputs off: the loop is still consumed, but silent.
        mixer.loop_on_cable = false;
        tx.send(vec![0.4f32; MIX_CHUNK_FRAMES * CHANNELS]).unwrap();
        monitor.fill(0.0);
        cable.fill(0.0);
        mixer.render(&mut monitor, &mut cable);
        assert!(monitor.iter().all(|sample| *sample == 0.0));
        assert!(cable.iter().all(|sample| *sample == 0.0));
    }

    fn idle_mixer(loop_rx: Option<Receiver<Vec<f32>>>) -> Mixer {
        Mixer {
            voices: Vec::new(),
            monitor_gain: 1.0,
            cable_gain: 1.0,
            master_gain: 1.0,
            loop_rx,
            loop_chunk: Vec::new(),
            loop_pos: 0,
            loop_gain: 1.0,
            loop_on_monitor: true,
            loop_on_cable: true,
        }
    }

    #[test]
    fn idle_mixer_has_no_work() {
        assert!(!idle_mixer(None).has_work());
    }

    #[test]
    fn a_playing_voice_keeps_the_mixer_awake() {
        let mut mixer = idle_mixer(None);
        mixer.voices.push(Voice {
            sound_id: 1,
            source: VoiceSource::Buffer(Arc::new(vec![0.1f32; MIX_CHUNK_FRAMES * CHANNELS])),
            position: 0,
            gain: 1.0,
            stop_at: None,
            finished: false,
            total_frames: None,
        });
        assert!(mixer.has_work());
    }

    #[test]
    fn a_stream_voice_mixes_and_ends() {
        let mut mixer = idle_mixer(None);
        let (tx, rx) = crossbeam_channel::bounded(4);
        mixer.voices.push(Voice {
            sound_id: 7,
            source: VoiceSource::Stream {
                rx,
                chunk: Vec::new(),
                pos: 0,
                ended: false,
            },
            position: 0,
            gain: 1.0,
            stop_at: None,
            finished: false,
            total_frames: Some(MIX_CHUNK_FRAMES),
        });
        tx.send(vec![0.25f32; MIX_CHUNK_FRAMES * CHANNELS]).unwrap();

        let mut monitor = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        let mut cable = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        mixer.render(&mut monitor, &mut cable);
        assert!(monitor.iter().any(|sample| sample.abs() > 0.001));
        assert!(cable.iter().any(|sample| sample.abs() > 0.001));

        drop(tx);
        for _ in 0..4 {
            mixer.render(&mut monitor, &mut cable);
            if mixer.voices.is_empty() {
                break;
            }
        }
        assert!(mixer.voices.is_empty(), "stream voice never finished");
    }

    #[test]
    fn a_connected_loop_counts_as_work() {
        let (_tx, rx) = crossbeam_channel::unbounded();
        assert!(idle_mixer(Some(rx)).has_work());
    }

    #[test]
    fn a_dead_loop_stops_counting_as_work() {
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<f32>>();
        let mut mixer = idle_mixer(Some(rx));
        assert!(mixer.has_work());

        drop(tx);
        let mut monitor = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        let mut cable = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        mixer.render(&mut monitor, &mut cable);

        assert!(mixer.loop_rx.is_none(), "receiver should be dropped");
        assert!(
            !mixer.has_work(),
            "a dead loop must let the mixer park instead of polling"
        );
    }

    #[test]
    fn ring_pop_drops_an_odd_tail_sample() {
        let ring = Ring::new(0.1);
        // Two whole frames plus a stray sample. The mixer never produces this,
        // but the ring must not strand it (or keep frame alignment broken).
        ring.push_scaled(&[0.1, 0.2, 0.3, 0.4, 0.5], 1.0);

        let mut out = VecDeque::new();
        assert_eq!(ring.pop_into(&mut out, 4), 2);
        let popped: Vec<f32> = out.iter().copied().collect();
        assert_eq!(popped, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(ring.buffered_frames(), 0, "the stray sample must not stay");

        // Frame alignment survives: the next frame pops as (L, R).
        ring.push_scaled(&[0.6, 0.7], 1.0);
        let mut next = VecDeque::new();
        assert_eq!(ring.pop_into(&mut next, 4), 1);
        assert_eq!(next.iter().copied().collect::<Vec<f32>>(), vec![0.6, 0.7]);
    }

    #[test]
    fn buffers_with_non_finite_samples_are_rejected() {
        assert!(is_valid_buffer(&[0.0, 1.0]));
        assert!(!is_valid_buffer(&[]));
        assert!(!is_valid_buffer(&[0.0, 1.0, 2.0]), "partial frame");
        assert!(!is_valid_buffer(&[f32::NAN, 0.0]));
        assert!(!is_valid_buffer(&[f32::INFINITY, 0.0]));
        assert!(!is_valid_buffer(&[0.0, f32::NEG_INFINITY]));
    }

    #[test]
    fn an_odd_stream_chunk_yields_whole_frames_only() {
        let (_tx, rx) = crossbeam_channel::bounded(4);
        let mut voice = Voice {
            sound_id: 1,
            source: VoiceSource::Stream {
                rx,
                chunk: vec![0.25, -0.25, 0.5],
                pos: 0,
                ended: true,
            },
            position: 0,
            gain: 1.0,
            stop_at: None,
            finished: false,
            total_frames: None,
        };
        assert_eq!(voice.next_frame(), Some((0.25, -0.25)));
        // The stranded third sample cannot form a frame; indexing it would panic.
        assert_eq!(voice.next_frame(), None);
        assert!(voice.exhausted());
    }

    #[test]
    fn a_truncated_stream_chunk_cannot_wedge_the_mixer() {
        let (tx, rx) = crossbeam_channel::bounded(4);
        tx.send(vec![0.1, 0.2, 0.3]).unwrap();
        drop(tx);

        let mut mixer = idle_mixer(None);
        mixer.voices.push(Voice {
            sound_id: 9,
            source: VoiceSource::Stream {
                rx,
                chunk: Vec::new(),
                pos: 0,
                ended: false,
            },
            position: 0,
            gain: 1.0,
            stop_at: None,
            finished: false,
            total_frames: Some(2),
        });

        let mut monitor = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        let mut cable = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        for _ in 0..4 {
            mixer.render(&mut monitor, &mut cable);
            if mixer.voices.is_empty() {
                break;
            }
        }
        assert!(mixer.voices.is_empty(), "odd chunk wedged the voice");
    }

    #[test]
    fn scaled_add_matches_a_per_sample_multiply() {
        let src = [0.5f32, -0.25, 1.0, -1.0];
        for gain in [1.0f32, 0.5, 0.0, 1.5] {
            let mut dst = [0.125f32; 4];
            add_scaled(&mut dst, &src, gain);
            for (index, sample) in src.iter().enumerate() {
                assert_eq!(dst[index], 0.125 + sample * gain);
            }
        }
    }

    #[test]
    fn unity_master_gain_only_clamps() {
        let mut unity = [0.5f32, -1.5, 1.5, 0.0];
        apply_master_gain(&mut unity, 1.0);
        assert_eq!(unity, [0.5, -1.0, 1.0, 0.0]);

        let mut halved = [0.5f32, -1.5, 1.5, 0.0];
        apply_master_gain(&mut halved, 0.5);
        assert_eq!(halved, [0.25, -0.75, 0.75, 0.0]);
    }

    #[test]
    fn a_short_loop_chunk_spans_render_passes() {
        // Two chunks of 100 frames each: the loop write crosses chunk boundaries
        // inside one 240-frame render pass.
        let (tx, rx) = crossbeam_channel::bounded(4);
        let mut mixer = idle_mixer(Some(rx));
        mixer.loop_gain = 0.5;
        tx.send(vec![0.5f32; 100 * CHANNELS]).unwrap();
        tx.send(vec![0.25f32; 100 * CHANNELS]).unwrap();

        let mut monitor = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        let mut cable = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        mixer.render(&mut monitor, &mut cable);

        let first = 100 * CHANNELS;
        let second = 200 * CHANNELS;
        assert!(monitor[..first].iter().all(|s| (*s - 0.25).abs() < 1e-6));
        assert!(monitor[first..second]
            .iter()
            .all(|s| (*s - 0.125).abs() < 1e-6));
        assert!(monitor[second..].iter().all(|s| *s == 0.0));
        // Identical on both outputs: the loop is routed to both.
        assert_eq!(monitor, cable);
        // Both chunks were drained (the third read found the channel empty).
        assert_eq!(mixer.loop_pos, 100 * CHANNELS);
    }

    #[test]
    fn a_started_engine_has_a_live_wake_path() {
        let engine = Engine::new(1.0, 1.0, 0.0);
        assert!(
            engine.mixer_failure.is_none(),
            "no failure on a normal boot"
        );
        assert!(engine.mixer_wake.is_some(), "wake sender must exist");
        let running = engine
            .mixer_thread
            .lock()
            .map(|thread| thread.is_some())
            .unwrap_or(false);
        assert!(running, "mixer thread handle must exist");
    }

    #[test]
    fn stop_loop_completes_when_the_mixer_lock_is_poisoned() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("sounddeck-engine-poison-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("loop.wav");
        write_wav(&path, 0.3);

        let engine = Engine::new(1.0, 1.0, 0.0);
        engine.start_loop(path).unwrap();

        // Poison the mixer mutex exactly like a panicking mixer would.
        let mixer = engine.mixer.clone();
        let panicked = thread::spawn(move || {
            let _guard = mixer.lock().unwrap();
            panic!("simulated mixer panic");
        })
        .join();
        assert!(panicked.is_err());
        assert!(engine.mixer.lock().is_err(), "lock should be poisoned");

        // Must not hang: the receiver is taken (dropping it unblocks the parked
        // decoder) and the stream is stopped and joined.
        engine.stop_loop();

        let mixer = engine.mixer.lock().unwrap_or_else(|err| err.into_inner());
        assert!(mixer.loop_rx.is_none(), "receiver must be released");
        assert!(mixer.loop_chunk.is_empty());
        drop(mixer);
        assert_eq!(engine.loop_state(), LoopState::Idle);
        let _ = fs::remove_dir_all(&dir);
    }
}
