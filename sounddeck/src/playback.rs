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

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::audio::decode::{decode_file, DecodedAudio};
use crate::audio::engine::Engine;

struct Playable {
    path: PathBuf,
    volume: f32,
    samples: Option<Arc<Vec<f32>>>,
    loading: bool,
    pending_play: bool,
    error: Option<String>,
    generation: u64,
    retain: bool,
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

    pub fn loaded_count(&self) -> usize {
        let Ok(state) = self.state.lock() else {
            return 0;
        };
        state.values().filter(|e| e.samples.is_some()).count()
    }

    pub fn poll_events(&self) -> Vec<PlaybackEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events_rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn enqueue(
        &self,
        entry: &mut Playable,
        id: u64,
        retain: bool,
        verbose: bool,
    ) -> Option<Job> {
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

fn loader_loop(
    jobs: Receiver<Job>,
    state: Arc<Mutex<HashMap<u64, Playable>>>,
    engine: Arc<Engine>,
    events: Sender<PlaybackEvent>,
) {
    while let Ok(job) = jobs.recv() {
        let decoded: Result<DecodedAudio, String> =
            decode_file(&job.path).map_err(|e| format!("{e:#}"));

        let mut to_play: Option<(u64, Arc<Vec<f32>>, f32)> = None;
        let mut loaded_event = None;
        let mut failed_event = None;

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
            match decoded {
                Ok(audio) => {
                    if job.retain {
                        entry.samples = Some(audio.samples.clone());
                    } else {
                        entry.samples = None;
                    }
                    entry.error = None;
                    if entry.pending_play {
                        entry.pending_play = false;
                        to_play = Some((job.id, audio.samples.clone(), entry.volume));
                    }
                    if job.retain {
                        loaded_event = Some(PlaybackEvent::Loaded {
                            id: job.id,
                            duration: audio.duration_secs,
                            bytes: audio.samples.len() * 4,
                            verbose: job.verbose,
                        });
                    }
                }
                Err(message) => {
                    entry.error = Some(message.clone());
                    entry.pending_play = false;
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
    }
}
