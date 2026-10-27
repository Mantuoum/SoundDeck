//! Shared playback registry.
//!
//! The UI and the global-hotkey thread both talk to this object, so hotkeys keep
//! firing when the window is hidden in the tray.
//!
//! Every RAM/disk decision is delegated to [`policy`]. This module only executes
//! the answer: keep a buffer, or stream the file from disk. There is no third
//! path where a whole file is decoded just to be played once.

pub mod policy;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::audio::decode::decode_file;
use crate::audio::engine::Engine;
use crate::audio::reader::{frames_for_seconds, probe_seconds};
use crate::audio::stream;

use policy::{Route, StreamReason, Usage};

struct Playable {
    path: PathBuf,
    volume: f32,
    samples: Option<Arc<Vec<f32>>>,
    loading: bool,
    pending_play: bool,
    error: Option<String>,
    missing: bool,
    /// Set when the cache refused this sound; it streams instead of retrying.
    refused: Option<StreamReason>,
    /// Container duration, once probed. Used for stream progress.
    duration: Option<f32>,
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
            missing: false,
            refused: None,
            duration: None,
            generation: 0,
            retain: false,
            last_used: Instant::now(),
        }
    }

    fn bytes(&self) -> usize {
        self.samples
            .as_ref()
            .map(|samples| samples.len() * policy::SAMPLE_BYTES)
            .unwrap_or(0)
    }
}

struct Job {
    id: u64,
    generation: u64,
    verbose: bool,
    retain: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SoundInfo {
    pub loading: bool,
    pub error: Option<String>,
    pub missing: bool,
    /// Samples are resident right now. This is the truth the UI must show.
    pub loaded: bool,
    /// Preload was refused; the sound streams from disk.
    pub refused: bool,
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
    /// The sound works, but it is not kept in memory. One per sound per session.
    Refused { id: u64, message: String },
    /// Background worker problem that is not tied to one sound.
    Warning(String),
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
        let (events_tx, events_rx) = unbounded::<PlaybackEvent>();

        {
            let state = state.clone();
            let engine = engine.clone();
            let events = events_tx.clone();
            let spawned = thread::Builder::new()
                .name("sounddeck-loader".into())
                .spawn(move || loader_loop(jobs_rx, state, engine, events));
            if let Err(err) = spawned {
                let _ = events_tx.send(PlaybackEvent::Warning(format!(
                    "Could not start the preload worker: {err}. Sounds will stream from disk."
                )));
            }
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
            Ok(state) => state,
            Err(_) => return,
        };
        if let Some(entry) = state.get_mut(&id) {
            if entry.path != path {
                entry.path = path;
                entry.samples = None;
                entry.error = None;
                entry.loading = false;
                entry.pending_play = false;
                entry.refused = None;
                entry.duration = None;
                entry.generation += 1;
            }
            entry.volume = volume;
            entry.missing = !entry.path.is_file();
            return;
        }
        let mut entry = Playable::new(path, volume);
        entry.missing = !entry.path.is_file();
        state.insert(id, entry);
    }

    pub fn remove(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.remove(&id);
        }
        self.engine.stop_sound(id);
    }

    pub fn set_path(&self, id: u64, path: PathBuf, reload: bool) {
        let mut job = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.get_mut(&id) else {
                return;
            };
            entry.path = path;
            entry.samples = None;
            entry.error = None;
            entry.loading = false;
            entry.pending_play = false;
            entry.refused = None;
            entry.duration = None;
            entry.generation += 1;
            entry.missing = !entry.path.is_file();
            if reload && entry.retain {
                entry.loading = true;
                job = Some(Job {
                    id,
                    generation: entry.generation,
                    verbose: true,
                    retain: true,
                });
            }
        }
        self.send_job(job);
    }

    pub fn set_volume(&self, id: u64, volume: f32) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.get_mut(&id) {
                entry.volume = volume;
            }
        }
        self.engine.set_sound_volume(id, volume);
    }

    /// Enable or disable keeping this sound in RAM.
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
                entry.refused = None;
                if entry.samples.is_none() && !entry.loading {
                    entry.loading = true;
                    job = Some(Job {
                        id,
                        generation: entry.generation,
                        verbose: false,
                        retain: true,
                    });
                }
            } else {
                entry.samples = None;
                entry.loading = false;
                entry.refused = None;
            }
        }
        self.send_job(job);
    }

    /// Load a retained sound if it is not already resident.
    pub fn ensure_loaded(&self, id: u64) {
        let job = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.get_mut(&id) else {
                return;
            };
            if !entry.retain || entry.samples.is_some() || entry.loading || entry.refused.is_some()
            {
                return;
            }
            entry.loading = true;
            Job {
                id,
                generation: entry.generation,
                verbose: false,
                retain: true,
            }
        };
        self.send_job(Some(job));
    }

    /// Force a decode, used by "Reload" in the context menu.
    pub fn reload(&self, id: u64, verbose: bool) {
        let job = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.get_mut(&id) else {
                return;
            };
            entry.samples = None;
            entry.error = None;
            entry.refused = None;
            entry.duration = None;
            entry.generation += 1;
            entry.loading = true;
            entry.missing = !entry.path.is_file();
            Job {
                id,
                generation: entry.generation,
                verbose,
                retain: entry.retain,
            }
        };
        self.send_job(Some(job));
    }

    /// Trigger a sound. Resident buffers play instantly; everything else streams
    /// from disk, with no full decode in the playback path.
    pub fn play(&self, id: u64) {
        let mut job = None;
        let mut stream_request = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.get_mut(&id) else {
                return;
            };
            entry.last_used = Instant::now();
            if let Some(samples) = entry.samples.clone() {
                self.engine.play(id, samples, entry.volume);
                return;
            }
            entry.missing = !entry.path.is_file();
            if entry.missing {
                entry.error = Some(format!("File not found: {}", entry.path.display()));
                let _ = self.events_tx.send(PlaybackEvent::Failed {
                    id,
                    message: entry.error.clone().unwrap_or_default(),
                    missing: true,
                });
                return;
            }
            if entry.loading {
                entry.pending_play = true;
                return;
            }
            if entry.retain && entry.refused.is_none() {
                entry.loading = true;
                entry.pending_play = true;
                job = Some(Job {
                    id,
                    generation: entry.generation,
                    verbose: false,
                    retain: true,
                });
            } else {
                let duration = entry.duration.or_else(|| probe_seconds(&entry.path));
                entry.duration = duration;
                stream_request = Some((entry.path.clone(), entry.volume, duration));
            }
        }
        if let Some(job) = job {
            let _ = self.jobs.send(job);
        }
        if let Some((path, volume, duration)) = stream_request {
            self.start_stream(id, &path, volume, duration);
        }
    }

    pub fn stop_sound(&self, id: u64) {
        self.engine.stop_sound(id);
    }

    pub fn stop_all(&self) {
        self.engine.stop_all();
    }

    pub fn info(&self, id: u64) -> SoundInfo {
        let Ok(mut state) = self.state.lock() else {
            return SoundInfo::default();
        };
        let Some(entry) = state.get_mut(&id) else {
            return SoundInfo::default();
        };
        SoundInfo {
            loading: entry.loading,
            error: entry.error.clone(),
            missing: entry.missing,
            loaded: entry.samples.is_some(),
            refused: entry.refused.is_some(),
        }
    }

    /// One lock for a whole frame's worth of cards.
    pub fn info_map(&self, ids: &[u64]) -> HashMap<u64, SoundInfo> {
        let mut out = HashMap::with_capacity(ids.len());
        let Ok(mut state) = self.state.lock() else {
            return out;
        };
        for id in ids {
            let Some(entry) = state.get_mut(id) else {
                continue;
            };
            out.insert(
                *id,
                SoundInfo {
                    loading: entry.loading,
                    error: entry.error.clone(),
                    missing: entry.missing,
                    loaded: entry.samples.is_some(),
                    refused: entry.refused.is_some(),
                },
            );
        }
        out
    }

    pub fn mark_missing(&self, id: u64, missing: bool, message: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.get_mut(&id) {
                entry.missing = missing;
                if missing {
                    entry.error = message;
                } else if entry.error.is_some() {
                    entry.error = None;
                }
            }
        }
    }

    /// Current cache usage, for diagnostics and the settings screen.
    pub fn cache_usage(&self) -> Usage {
        match self.state.lock() {
            Ok(state) => usage_of(&state, u64::MAX),
            Err(_) => Usage::default(),
        }
    }

    pub fn poll_events(&self) -> Vec<PlaybackEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events_rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn send_job(&self, job: Option<Job>) {
        if let Some(job) = job {
            let _ = self.jobs.send(job);
        }
    }

    fn start_stream(&self, id: u64, path: &Path, volume: f32, duration: Option<f32>) {
        match stream::spawn_once(path) {
            Ok(rx) => {
                let total = duration.map(frames_for_seconds);
                self.engine.play_stream(id, rx, volume, total);
            }
            Err(err) => {
                let message = format!("{err:#}");
                let missing = !path.exists();
                if let Ok(mut state) = self.state.lock() {
                    if let Some(entry) = state.get_mut(&id) {
                        entry.error = Some(message.clone());
                        entry.loading = false;
                        entry.pending_play = false;
                    }
                }
                let _ = self.events_tx.send(PlaybackEvent::Failed {
                    id,
                    message,
                    missing,
                });
            }
        }
    }
}

/// Total resident audio, excluding `skip` (the sound being admitted).
fn usage_of(state: &HashMap<u64, Playable>, skip: u64) -> Usage {
    let mut usage = Usage::default();
    for (id, entry) in state {
        if *id == skip || entry.samples.is_none() {
            continue;
        }
        usage.bytes = usage.bytes.saturating_add(entry.bytes());
        usage.sounds += 1;
    }
    usage
}

/// Drops the least recently used, not-currently-playing buffers until `need`
/// more bytes fit. Never evicts the sound being admitted.
fn evict_to_fit(state: &mut HashMap<u64, Playable>, engine: &Engine, need: usize, skip: u64) {
    loop {
        let usage = usage_of(state, skip);
        if policy::admit(usage, need, policy::process_bytes()) {
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
        }
    }
}

fn reason_message(reason: StreamReason, usage: Usage, bytes: usize) -> String {
    match reason {
        StreamReason::TooLong => format!(
            "it is longer than the {:.0} s preload limit",
            policy::RETAIN_SECONDS_CAP
        ),
        StreamReason::UnknownDuration => "its duration could not be read".into(),
        StreamReason::BudgetFull => policy::refusal_reason(usage, bytes, policy::process_bytes()),
        StreamReason::PreloadOff => "preload is off for this sound".into(),
    }
}

fn loader_loop(
    jobs: Receiver<Job>,
    state: Arc<Mutex<HashMap<u64, Playable>>>,
    engine: Arc<Engine>,
    events: Sender<PlaybackEvent>,
) {
    while let Ok(job) = jobs.recv() {
        // Snapshot under the lock, then do file I/O unlocked.
        let snapshot = {
            let Ok(state) = state.lock() else {
                continue;
            };
            state.get(&job.id).and_then(|entry| {
                if entry.generation != job.generation {
                    return None;
                }
                Some((entry.path.clone(), entry.volume, entry.retain))
            })
        };
        let Some((path, volume, retain)) = snapshot else {
            continue;
        };

        let duration = probe_seconds(&path);
        let usage = {
            let Ok(state) = state.lock() else {
                continue;
            };
            usage_of(&state, job.id)
        };
        let route = policy::route(&policy::Request {
            retain: retain && job.retain,
            duration,
            usage,
            process_bytes: policy::process_bytes(),
        });

        match route {
            Route::Stream(reason) => {
                let message = reason_message(reason, usage, 0);
                // Preload being switched off is a choice, not a refusal; it must
                // not clear the per-sound toggle or spam the log.
                let intentional = reason == StreamReason::PreloadOff;
                let pending;
                {
                    let Ok(mut state) = state.lock() else {
                        continue;
                    };
                    let Some(entry) = state.get_mut(&job.id) else {
                        continue;
                    };
                    if entry.generation != job.generation {
                        continue;
                    }
                    entry.loading = false;
                    if !intentional {
                        entry.refused = Some(reason);
                    }
                    entry.duration = duration;
                    pending = std::mem::take(&mut entry.pending_play);
                }
                if !intentional {
                    let _ = events.send(PlaybackEvent::Refused {
                        id: job.id,
                        message,
                    });
                }
                if pending {
                    start_stream_from(&engine, &events, &state, job.id, &path, volume, duration);
                }
            }
            Route::Preload => {
                let decoded = decode_file(&path);
                let pending;
                let mut completed = None;
                match decoded {
                    Ok(audio) => {
                        let bytes = audio.samples.len() * policy::SAMPLE_BYTES;
                        let keep = {
                            let Ok(mut state) = state.lock() else {
                                continue;
                            };
                            evict_to_fit(&mut state, &engine, bytes, job.id);
                            let usage = usage_of(&state, job.id);
                            policy::admit(usage, bytes, policy::process_bytes())
                        };
                        {
                            let Ok(mut state) = state.lock() else {
                                continue;
                            };
                            let Some(entry) = state.get_mut(&job.id) else {
                                continue;
                            };
                            if entry.generation != job.generation {
                                continue;
                            }
                            entry.loading = false;
                            entry.error = None;
                            entry.duration = Some(audio.duration_secs);
                            pending = std::mem::take(&mut entry.pending_play);
                            if keep {
                                entry.samples = Some(audio.samples.clone());
                                entry.refused = None;
                                completed = Some((audio.duration_secs, bytes));
                            } else {
                                entry.refused = Some(StreamReason::BudgetFull);
                            }
                        }
                        if let Some((duration, bytes)) = completed {
                            let _ = events.send(PlaybackEvent::Loaded {
                                id: job.id,
                                duration,
                                bytes,
                                verbose: job.verbose,
                            });
                        } else {
                            let _ = events.send(PlaybackEvent::Refused {
                                id: job.id,
                                message: "the preload budget is full".into(),
                            });
                        }
                        if pending {
                            let samples = state.lock().ok().and_then(|state| {
                                state.get(&job.id).and_then(|e| e.samples.clone())
                            });
                            match samples {
                                Some(samples) => engine.play(job.id, samples, volume),
                                None => start_stream_from(
                                    &engine, &events, &state, job.id, &path, volume, duration,
                                ),
                            }
                        }
                    }
                    Err(message) => {
                        let message = format!("{message:#}");
                        {
                            let Ok(mut state) = state.lock() else {
                                continue;
                            };
                            let Some(entry) = state.get_mut(&job.id) else {
                                continue;
                            };
                            if entry.generation != job.generation {
                                continue;
                            }
                            entry.loading = false;
                            entry.pending_play = false;
                            entry.error = Some(message.clone());
                        }
                        let _ = events.send(PlaybackEvent::Failed {
                            id: job.id,
                            message,
                            missing: !path.exists(),
                        });
                    }
                }
            }
        }
    }
}

fn start_stream_from(
    engine: &Engine,
    events: &Sender<PlaybackEvent>,
    state: &Arc<Mutex<HashMap<u64, Playable>>>,
    id: u64,
    path: &Path,
    volume: f32,
    duration: Option<f32>,
) {
    match stream::spawn_once(path) {
        Ok(rx) => engine.play_stream(id, rx, volume, duration.map(frames_for_seconds)),
        Err(err) => {
            let message = format!("{err:#}");
            if let Ok(mut state) = state.lock() {
                if let Some(entry) = state.get_mut(&id) {
                    entry.error = Some(message.clone());
                }
            }
            let _ = events.send(PlaybackEvent::Failed {
                id,
                message,
                missing: !path.exists(),
            });
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
        let mut data = Vec::with_capacity(44 + frames as usize * 4);
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + frames * 4).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&(rate * 4).to_le_bytes());
        data.extend_from_slice(&4u16.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&(frames * 4).to_le_bytes());
        for _ in 0..frames * 2 {
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
    fn a_long_sound_is_refused() {
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
        assert!(!playback.info(1).loaded);
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
        assert!(playback.info(1).loaded);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_long_sound_still_plays_from_disk() {
        let dir = temp_dir("stream");
        let path = dir.join("long.wav");
        write_wav(&path, 40);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(2, path, 1.0);
        playback.set_retain(2, false);
        playback.play(2);
        assert!(playback.cache_usage().sounds == 0);
        assert!(!playback.info(2).loaded);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_reports_failure() {
        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(3, PathBuf::from("Z:\\nope.wav"), 1.0);
        playback.play(3);
        let events = wait_for_events(&playback, Duration::from_secs(2));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, PlaybackEvent::Failed { missing: true, .. })),
            "got {events:?}"
        );
    }
}
