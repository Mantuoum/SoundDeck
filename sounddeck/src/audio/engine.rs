use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use super::decode::{CHANNELS, SAMPLE_RATE};
use super::wasapi;

const MIX_CHUNK_FRAMES: usize = 240; // 5 ms
const FADE_IN_FRAMES: f32 = 120.0; // 2.5 ms
const FADE_OUT_FRAMES: f32 = 240.0; // 5 ms
const MAX_VOICES: usize = 96;
const MAX_VOICES_PER_SOUND: usize = 8;

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
}

impl Ring {
    pub fn new(capacity_seconds: f32) -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(16_384)),
            capacity_samples: (SAMPLE_RATE as f32 * CHANNELS as f32 * capacity_seconds) as usize,
            consumers: AtomicUsize::new(0),
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
        for _ in 0..frames * CHANNELS {
            if let Some(sample) = queue.pop_front() {
                out.push_back(sample);
            }
        }
        frames
    }

    pub fn clear(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.clear();
        }
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
}

impl Engine {
    pub fn new(monitor_volume: f32, cable_volume: f32, master_gain_db: f32) -> Self {
        let mixer = Arc::new(Mutex::new(Mixer {
            voices: Vec::new(),
            monitor_gain: monitor_volume.clamp(0.0, 1.0),
            cable_gain: cable_volume.clamp(0.0, 1.0),
            master_gain: db_to_gain(master_gain_db),
        }));
        let rings = [Arc::new(Ring::new(2.0)), Arc::new(Ring::new(2.0))];
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
        let same_sound = mixer
            .voices
            .iter()
            .filter(|v| v.sound_id == sound_id)
            .count();
        if same_sound >= MAX_VOICES_PER_SOUND {
            if let Some(pos) = mixer.voices.iter().position(|v| v.sound_id == sound_id) {
                mixer.voices.remove(pos);
            }
        }
        if mixer.voices.len() >= MAX_VOICES {
            mixer.voices.remove(0);
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

    pub fn active(&self) -> Vec<ActiveSound> {
        self.mixer
            .lock()
            .map(|mixer| mixer.active())
            .unwrap_or_default()
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
