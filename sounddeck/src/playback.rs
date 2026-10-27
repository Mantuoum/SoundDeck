//! Shared playback registry.
//!
//! The UI and the global-hotkey thread both talk to this object, so hotkeys
//! keep firing samples even when the window is hidden in the tray.
//!
//! Sounds with a hotkey can be kept in RAM. Everything else is decoded on
//! trigger, played, and released again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::audio::decode::{decode_file, probe_bytes, DecodedAudio};
use crate::audio::engine::Engine;
use crate::memory;

struct Playable {
    path: PathBuf,
    volume: f32,
    samples: Option<Arc<Vec<f32>>>,
    loading: bool,
    pending_play: bool,
    error: Option<String>,
    generation: u64,
    retain: bool,
    /// Last time this buffer was played or admitted, used for LRU eviction.
    last_used: Instant,
}

impl Playable {
    fn new(path: PathBuf, volume: f32) -> Self {
        Self {
            path,
            volume,
            samples: None,
            loading: false,
            pending_play: false,
            error: None,
            generation: 0,
            retain: false,
            last_used: Instant::now(),
        }
    }
}

struct Job {
    id: u64,
    path: PathBuf,
    generation: u64,
    verbose: bool,
    retain: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SoundInfo {
    pub loading: bool,
    pub error: Option<String>,
    pub missing: bool,
}

#[derive(Clone, Debug)]
pub enum PlaybackEvent {
    Loaded {
        id: u64,
        duration: f32,
        bytes: usize,
        verbose: bool,
    },
    Failed {
        id: u64,
        message: String,
        missing: bool,
    },
    /// The sound works, but it was not kept in memory. One per sound per session.
    Refused { id: u64, message: String },
}

pub struct Playback {
    engine: Arc<Engine>,
    state: Arc<Mutex<HashMap<u64, Playable>>>,
    jobs: Sender<Job>,
    events_tx: Sender<PlaybackEvent>,
    events_rx: Receiver<PlaybackEvent>,
}

impl Playback {
    pub fn new(engine: Arc<Engine>) -> Self {
        let state = Arc::new(Mutex::new(HashMap::new()));
        let (jobs, jobs_rx) = unbounded::<Job>();
        let (events_tx, events_rx) = unbounded();

        {
            let state = state.clone();
            let engine = engine.clone();
            let events = events_tx.clone();
            let _ = thread::Builder::new()
                .name("sounddeck-loader".into())
                .spawn(move || loader_loop(jobs_rx, state, engine, events));
        }

        Self {
            engine,
            state,
            jobs,
            events_tx,
            events_rx,
        }
    }

    pub fn register(&self, id: u64, path: PathBuf, volume: f32) {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if let Some(entry) = state.get_mut(&id) {
            if entry.path != path {
                entry.path = path;
                entry.samples = None;
                entry.error = None;
                entry.loading = false;
                entry.pending_play = false;
                entry.generation += 1;
            }
            entry.volume = volume.clamp(0.0, 1.0);
        } else {
            state.insert(id, Playable::new(path, volume.clamp(0.0, 1.0)));
        }
    }

    pub fn remove(&self, id: u64) {
        self.engine.stop_sound(id);
        if let Ok(mut state) = self.state.lock() {
            state.remove(&id);
        }
    }

    pub fn set_path(&self, id: u64, path: PathBuf, play_when_ready: bool) {
        let mut job = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.get_mut(&id) else {
                return;
            };
            self.engine.stop_sound(id);
            entry.path = path;
            entry.samples = None;
            entry.error = None;
            entry.generation += 1;
            entry.loading = false;
            entry.pending_play = play_when_ready;
            if entry.retain || play_when_ready {
                job = self.enqueue(entry, id, entry.retain, false);
            }
        }
        self.send_job(job);
    }

    pub fn set_volume(&self, id: u64, volume: f32) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.get_mut(&id) {
                entry.volume = volume.clamp(0.0, 1.0);
                self.engine.set_sound_volume(id, entry.volume);
            }
        }
    }

    /// Enable or disable keeping this sound in RAM.
    /// Enabling loads it; disabling releases its cached samples.
    pub fn set_retain(&self, id: u64, retain: bool) {
        let mut job = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.get_mut(&id) else {
                return;
            };
            entry.retain = retain;
            if retain {
                if entry.samples.is_none() {
                    entry.generation += 1;
                    entry.loading = false;
                    entry.pending_play = false;
                    job = self.enqueue(entry, id, true, false);
                }
            } else {
                entry.samples = None;
                entry.loading = false;
                entry.pending_play = false;
                entry.generation += 1;
            }
        }
        self.send_job(job);
    }

    /// Decode a retained sound without playing it (startup preload).
    pub fn ensure_loaded(&self, id: u64) {
        let mut job = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if let Some(entry) = state.get_mut(&id) {
                if entry.retain && entry.samples.is_none() && !entry.loading {
                    job = self.enqueue(entry, id, true, false);
                }
            }
        }
        self.send_job(job);
    }

    pub fn reload(&self, id: u64, verbose: bool) {
        let mut job = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if let Some(entry) = state.get_mut(&id) {
                self.engine.stop_sound(id);
                entry.samples = None;
                entry.error = None;
                entry.generation += 1;
                entry.loading = false;
                entry.pending_play = false;
                if entry.retain {
                    job = self.enqueue(entry, id, true, verbose);
                }
            }
        }
        self.send_job(job);
    }

    /// Play a sound. If it is retained and cached it starts immediately.
    /// Otherwise it is decoded on the loader thread, played, and released.
    /// Safe to call from the hotkey thread.
    pub fn play(&self, id: u64) -> bool {
        let mut job = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return false;
            };
            let Some(entry) = state.get_mut(&id) else {
                return false;
            };
            entry.last_used = Instant::now();
            if entry.retain {
                if let Some(samples) = entry.samples.clone() {
                    self.engine.play(id, samples, entry.volume);
                    return true;
                }
            }
            if entry.path.exists() {
                entry.pending_play = true;
                entry.error = None;
                if !entry.loading {
                    job = self.enqueue(entry, id, entry.retain, false);
                }
            } else {
                entry.error = Some(format!("File not found: {}", entry.path.display()));
                entry.pending_play = false;
                let _ = self.events_tx.send(PlaybackEvent::Failed {
                    id,
                    message: entry.error.clone().unwrap_or_default(),
                    missing: true,
                });
            }
        }
        self.send_job(job);
        false
    }

    pub fn stop_sound(&self, id: u64) {
        self.engine.stop_sound(id);
    }

    pub fn stop_all(&self) {
        self.engine.stop_all();
    }

    pub fn mark_missing(&self, id: u64, missing: bool, message: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.get_mut(&id) {
                if entry.samples.is_none() && missing {
                    entry.error = message;
                }
                if !missing && entry.error.is_some() {
                    entry.error = None;
                }
            }
        }
    }

    pub fn info(&self, id: u64) -> SoundInfo {
        let Ok(state) = self.state.lock() else {
            return SoundInfo::default();
        };
        let Some(entry) = state.get(&id) else {
            return SoundInfo::default();
        };
        SoundInfo {
            loading: entry.loading,
            error: entry.error.clone(),
            missing: entry.samples.is_none() && !entry.path.exists(),
        }
    }

    /// Current cache usage, for diagnostics and the settings screen.
    pub fn cache_usage(&self) -> memory::Usage {
        match self.state.lock() {
            Ok(state) => usage_of(&state, u64::MAX),
            Err(_) => memory::Usage::default(),
        }
    }

    pub fn poll_events(&self) -> Vec<PlaybackEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events_rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn enqueue(&self, entry: &mut Playable, id: u64, retain: bool, verbose: bool) -> Option<Job> {
        entry.loading = true;
        Some(Job {
            id,
            path: entry.path.clone(),
            generation: entry.generation,
            verbose,
            retain,
        })
    }

    fn send_job(&self, job: Option<Job>) {
        if let Some(job) = job {
            let _ = self.jobs.send(job);
        }
    }
}

/// Total resident audio, excluding `skip` (the sound being admitted).
fn usage_of(state: &HashMap<u64, Playable>, skip: u64) -> memory::Usage {
    let mut usage = memory::Usage::default();
    for (id, entry) in state {
        if *id == skip {
            continue;
        }
        if let Some(samples) = &entry.samples {
            usage.bytes = usage
                .bytes
                .saturating_add(samples.len() * memory::SAMPLE_BYTES);
            usage.sounds += 1;
        }
    }
    usage
}

/// Drops the least recently used, not-currently-playing buffers until `need`
/// more bytes fit. Never evicts the sound being admitted.
fn evict_to_fit(state: &mut HashMap<u64, Playable>, engine: &Engine, need: usize, skip: u64) {
    loop {
        let usage = usage_of(state, skip);
        if memory::admit(usage, need, memory::private_bytes()) {
            return;
        }
        let victim = state
            .iter()
            .filter(|(id, entry)| {
                **id != skip && entry.samples.is_some() && !engine.is_playing_sound(**id)
            })
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(id, _)| *id);
        let Some(id) = victim else {
            return;
        };
        if let Some(entry) = state.get_mut(&id) {
            entry.samples = None;
            entry.retain = false;
        }
    }
}

fn loader_loop(
    jobs: Receiver<Job>,
    state: Arc<Mutex<HashMap<u64, Playable>>>,
    engine: Arc<Engine>,
    events: Sender<PlaybackEvent>,
) {
    while let Ok(job) = jobs.recv() {
        let mut retain = job.retain;
        let mut refusal: Option<String> = None;

        // Precheck from container metadata: a long file must never be decoded
        // just to be refused afterwards.
        if retain {
            match probe_bytes(&job.path) {
                Some(bytes) if bytes > memory::PER_SOUND_RETAIN_BYTES => {
                    retain = false;
                    refusal = Some(format!(
                        "not preloaded because it is {:.0}s long (limit {}s)",
                        memory::seconds_for_bytes(bytes),
                        memory::RETAIN_SECONDS_CAP as u32
                    ));
                }
                Some(_) => {}
                None => {
                    // Fail closed: without a duration estimate we cannot promise
                    // the decode stays inside the budget.
                    retain = false;
                    refusal = Some("not preloaded because its duration could not be read".into());
                }
            }
        }

        // A pure preload request that cannot be satisfied is dropped without
        // touching the disk.
        if job.retain && !retain {
            let pending = match state.lock() {
                Ok(state) => state
                    .get(&job.id)
                    .map(|entry| entry.pending_play)
                    .unwrap_or(false),
                Err(_) => false,
            };
            if !pending {
                if let Ok(mut state) = state.lock() {
                    if let Some(entry) = state.get_mut(&job.id) {
                        if entry.generation == job.generation {
                            entry.loading = false;
                            entry.retain = false;
                        }
                    }
                }
                if let Some(message) = refusal {
                    let _ = events.send(PlaybackEvent::Refused {
                        id: job.id,
                        message,
                    });
                }
                continue;
            }
        }

        let decoded: Result<DecodedAudio, String> =
            decode_file(&job.path).map_err(|e| format!("{e:#}"));

        let mut to_play: Option<(u64, Arc<Vec<f32>>, f32)> = None;
        let mut loaded_event = None;
        let mut failed_event = None;
        let mut refused_event = None;

        {
            let Ok(mut state) = state.lock() else {
                continue;
            };
            let Some(current) = state.get(&job.id) else {
                continue;
            };
            if current.generation != job.generation {
                continue;
            }
            let volume = current.volume;
            let pending_play = current.pending_play;
            if let Some(entry) = state.get_mut(&job.id) {
                entry.loading = false;
            }

            match decoded {
                Ok(audio) => {
                    let bytes = audio.samples.len() * memory::SAMPLE_BYTES;
                    let mut keep = false;
                    if retain {
                        evict_to_fit(&mut state, &engine, bytes, job.id);
                        let usage = usage_of(&state, job.id);
                        let process = memory::private_bytes();
                        keep = memory::admit(usage, bytes, process);
                        if !keep {
                            refusal = Some(memory::refusal_reason(usage, bytes, process));
                        }
                    }

                    if let Some(entry) = state.get_mut(&job.id) {
                        entry.error = None;
                        entry.samples = if keep {
                            Some(audio.samples.clone())
                        } else {
                            None
                        };
                        if job.retain && !keep {
                            entry.retain = false;
                        }
                        if pending_play {
                            entry.pending_play = false;
                            entry.last_used = Instant::now();
                        }
                        if !keep && entry.retain {
                            // Defensive: retain and no samples must never coexist.
                            entry.retain = false;
                        }
                    }

                    if keep {
                        loaded_event = Some(PlaybackEvent::Loaded {
                            id: job.id,
                            duration: audio.duration_secs,
                            bytes,
                            verbose: job.verbose,
                        });
                    }
                    if pending_play {
                        to_play = Some((job.id, audio.samples.clone(), volume));
                    }
                    if !keep {
                        if let Some(message) = refusal.take() {
                            if job.retain {
                                refused_event = Some(PlaybackEvent::Refused {
                                    id: job.id,
                                    message,
                                });
                            }
                        }
                    }
                }
                Err(message) => {
                    if let Some(entry) = state.get_mut(&job.id) {
                        entry.error = Some(message.clone());
                        entry.pending_play = false;
                        if job.retain {
                            entry.retain = false;
                        }
                    }
                    failed_event = Some(PlaybackEvent::Failed {
                        id: job.id,
                        message,
                        missing: !job.path.exists(),
                    });
                }
            }
        }

        if let Some((id, samples, volume)) = to_play {
            engine.play(id, samples, volume);
        }
        if let Some(event) = loaded_event {
            let _ = events.send(event);
        }
        if let Some(event) = failed_event {
            let _ = events.send(event);
        }
        if let Some(event) = refused_event {
            let _ = events.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn write_wav(path: &std::path::Path, seconds: u32) {
        let rate = 48_000u32;
        let frames = rate * seconds;
        let mut data = Vec::with_capacity(44 + frames as usize * 2);
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&(rate * 2).to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&(frames * 2).to_le_bytes());
        for _ in 0..frames {
            data.extend_from_slice(&0i16.to_le_bytes());
        }
        fs::write(path, data).unwrap();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sounddeck-playback-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait_for_events(playback: &Playback, timeout: Duration) -> Vec<PlaybackEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        while start.elapsed() < timeout {
            events.extend(playback.poll_events());
            if !events.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        events
    }

    #[test]
    fn a_long_sound_is_never_retained() {
        let dir = temp_dir("long");
        let path = dir.join("long.wav");
        write_wav(&path, 40);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(1, path, 1.0);
        playback.set_retain(1, true);

        let events = wait_for_events(&playback, Duration::from_secs(5));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, PlaybackEvent::Refused { .. })),
            "expected a refusal, got {events:?}"
        );
        assert_eq!(playback.cache_usage().sounds, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_short_sound_is_retained() {
        let dir = temp_dir("short");
        let path = dir.join("short.wav");
        write_wav(&path, 1);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(1, path, 1.0);
        playback.set_retain(1, true);

        let events = wait_for_events(&playback, Duration::from_secs(5));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, PlaybackEvent::Loaded { .. })),
            "expected a load, got {events:?}"
        );
        assert_eq!(playback.cache_usage().sounds, 1);
        assert!(playback.cache_usage().bytes > 0);
        let _ = fs::remove_dir_all(dir);
    }
}
