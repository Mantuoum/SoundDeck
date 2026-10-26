use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};

use super::decode::{CHANNELS, SAMPLE_RATE};
use super::stream::{LoopState, LoopStream};
use super::wasapi;

const MIX_CHUNK_FRAMES: usize = 240; // 5 ms
const FADE_IN_FRAMES: f32 = 120.0; // 2.5 ms
const FADE_OUT_FRAMES: f32 = 240.0; // 5 ms

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

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum AudioEvent {
    DeviceStarted {
        slot: Slot,
        name: String,
        detail: String,
    },
    DeviceError {
        slot: Slot,
        name: String,
        message: String,
    },
    Warning(String),
    Stopped {
        slot: Slot,
        name: String,
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
        let mut queue = match self.queue.lock() {
            Ok(q) => q,
            Err(_) => return 0,
        };
        let frames = (max_frames * CHANNELS).min(queue.len()) / CHANNELS;
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
        self.queue
            .lock()
            .map(|q| q.len() / CHANNELS)
            .unwrap_or(0)
    }
}

struct Voice {
    sound_id: u64,
    samples: Arc<Vec<f32>>,
    position: usize,
    gain: f32,
    stop_at: Option<usize>,
    finished: bool,
}

impl Voice {
    fn frames(&self) -> usize {
        self.samples.len() / CHANNELS
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
}

impl Mixer {
    fn render(&mut self, out: &mut [f32]) {
        let frames = out.len() / CHANNELS;
        for voice in &mut self.voices {
            let total = voice.frames();
            if voice.position >= total {
                voice.finished = true;
                continue;
            }
            let start = voice.position;
            let count = (total - start).min(frames);
            for i in 0..count {
                let frame = start + i;
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
                let src = frame * CHANNELS;
                let dst = i * CHANNELS;
                let g = voice.gain * env;
                out[dst] += voice.samples[src] * g;
                out[dst + 1] += voice.samples[src + 1] * g;
                if let Some(stop_at) = voice.stop_at {
                    if frame >= stop_at + FADE_OUT_FRAMES as usize {
                        voice.finished = true;
                    }
                }
            }
            voice.position = start + count;
            if voice.position >= total {
                voice.finished = true;
            }
        }

        // Loop track: mixed alongside voices, before master gain. An empty
        // channel means silence for the rest of this render, not a stop.
        if let Some(rx) = self.loop_rx.as_ref() {
            let frames = out.len() / CHANNELS;
            let mut index = 0usize;
            while index < frames {
                if self.loop_pos >= self.loop_chunk.len() {
                    match rx.try_recv() {
                        Ok(chunk) => {
                            self.loop_chunk = chunk;
                            self.loop_pos = 0;
                        }
                        Err(_) => break,
                    }
                }

                let available = (self.loop_chunk.len() - self.loop_pos) / CHANNELS;
                if available == 0 {
                    self.loop_chunk.clear();
                    self.loop_pos = 0;
                    break;
                }
                let take = (frames - index).min(available);
                let source = &self.loop_chunk[self.loop_pos..self.loop_pos + take * CHANNELS];
                let start = index * CHANNELS;
                let gain = self.loop_gain;
                for (dst, src) in out[start..start + take * CHANNELS]
                    .iter_mut()
                    .zip(source.iter())
                {
                    *dst += *src * gain;
                }
                self.loop_pos += take * CHANNELS;
                index += take;
            }
        }

        for sample in out.iter_mut() {
            *sample = (*sample * self.master_gain).clamp(-1.0, 1.0);
        }
        self.voices.retain(|v| !v.finished);
    }

    fn active(&self) -> Vec<ActiveSound> {
        self.voices
            .iter()
            .filter(|v| !v.finished && v.stop_at.is_none())
            .map(|v| {
                let total = v.frames().max(1) as f32;
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
}

pub struct Engine {
    mixer: Arc<Mutex<Mixer>>,
    rings: [Arc<Ring>; 2],
    devices: Mutex<[Option<DeviceHandle>; 2]>,
    events_tx: Sender<AudioEvent>,
    events_rx: Receiver<AudioEvent>,
    mixer_stop: Arc<AtomicBool>,
    mixer_thread: Mutex<Option<JoinHandle<()>>>,
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
        }));
        let rings = [Arc::new(Ring::new(0.15)), Arc::new(Ring::new(0.15))];
        let (events_tx, events_rx) = unbounded();
        let mixer_stop = Arc::new(AtomicBool::new(false));

        let mixer_thread = {
            let mixer = mixer.clone();
            let rings = rings.clone();
            let stop = mixer_stop.clone();
            thread::Builder::new()
                .name("sounddeck-mixer".into())
                .spawn(move || mixer_loop(mixer, rings, stop))
                .ok()
        };

        Self {
            mixer,
            rings,
            devices: Mutex::new([None, None]),
            events_tx,
            events_rx,
            mixer_stop,
            mixer_thread: Mutex::new(mixer_thread),
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
            let name = desired[0].as_ref().map(|r| r.name.clone()).unwrap_or_default();
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
            if current_id == wanted_id {
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
            samples,
            position: 0,
            gain: volume.clamp(0.0, 2.0),
            stop_at: None,
            finished: false,
        });
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
        Ok(())
    }

    /// Stops the loop immediately. Dropping the receiver unblocks the decoder
    /// even if it is parked in `send`, so the join is bounded.
    pub fn stop_loop(&self) {
        let rx = match self.mixer.lock() {
            Ok(mut mixer) => {
                mixer.loop_chunk.clear();
                mixer.loop_pos = 0;
                mixer.loop_rx.take()
            }
            Err(_) => None,
        };
        drop(rx);

        let stream = self.loop_stream.lock().ok().and_then(|mut slot| slot.take());
        if let Some(stream) = stream {
            stream.stop();
        }
    }

    pub fn set_loop_volume(&self, volume: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.loop_gain = volume.clamp(0.0, 1.0);
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
                    voice.gain = volume.clamp(0.0, 2.0);
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
        if let Ok(mut thread) = self.mixer_thread.lock() {
            if let Some(join) = thread.take() {
                let _ = join.join();
            }
        }
    }
}

fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db.clamp(-60.0, 12.0) / 20.0)
}

/// Move the shared loop status from `from` to `to` if it currently matches.
/// Used for mixer-side transitions (first chunk, underrun) without stomping on
/// a decoder error.
fn mixer_loop(
    mixer: Arc<Mutex<Mixer>>,
    rings: [Arc<Ring>; 2],
    stop: Arc<AtomicBool>,
) {
    let mut buffer = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
    let mut next = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now < next {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        let behind = now.saturating_duration_since(next);
        let chunks = ((behind.as_micros() / 5_000) as usize).clamp(1, 4);
        for _ in 0..chunks {
            buffer.fill(0.0);
            let (monitor_gain, cable_gain) = {
                match mixer.lock() {
                    Ok(mut mixer) => {
                        mixer.render(&mut buffer);
                        (mixer.monitor_gain, mixer.cable_gain)
                    }
                    Err(_) => break,
                }
            };
            if rings[0].has_consumers() {
                rings[0].push_scaled(&buffer, monitor_gain);
            }
            if rings[1].has_consumers() {
                rings[1].push_scaled(&buffer, cable_gain);
            }
            next += Duration::from_millis(5);
        }
        thread::sleep(Duration::from_millis(1));
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
        };
        tx.send(vec![0.8f32; MIX_CHUNK_FRAMES * CHANNELS])
            .unwrap();

        let mut out = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        mixer.render(&mut out);
        assert!(out
            .iter()
            .all(|sample| (*sample - 0.2).abs() < 1e-6));
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
        };
        tx.send(vec![0.25f32; MIX_CHUNK_FRAMES * CHANNELS])
            .unwrap();

        let mut out = vec![0.0f32; MIX_CHUNK_FRAMES * CHANNELS];
        mixer.render(&mut out);
        assert!(out
            .iter()
            .all(|sample| (*sample - 0.125).abs() < 1e-6));

        // Channel drained: silence, not a stop.
        out.fill(0.0);
        mixer.render(&mut out);
        assert!(out.iter().all(|sample| *sample == 0.0));
        assert!(mixer.loop_rx.is_some());
    }
}
