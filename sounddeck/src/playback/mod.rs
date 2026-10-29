//! Shared playback registry.
//!
//! The UI and the global-hotkey thread both talk to this object, so hotkeys keep
//! firing when the window is hidden in the tray.
//!
//! Every RAM/disk decision is delegated to [`policy`]. This module only executes
//! the answer: keep a buffer, or stream the file from disk. There is no third
//! path where a whole file is decoded just to be played once.
//!
//! # Locking
//!
//! `state` is the only lock here, and the order is UI → `state` → the engine's
//! mixer. So no engine call ever happens while a `state` guard is alive: each
//! method snapshots what it needs under the lock, drops the guard, and only then
//! talks to the engine.
//!
//! # The trigger path
//!
//! [`Playback::play`] runs on the UI or the hotkey thread, so it does no container
//! probing. It starts a stream with the duration it already knows and lets a
//! one-shot `sounddeck-probe` worker fill that in for the next trigger; progress
//! simply stays unknown until then (the engine clamps it safely). The one disk
//! call left on that path is the `stat` that says the file is gone, because the
//! user has to be told.
//!
//! # Channels
//!
//! `jobs` and `events` are deliberately **unbounded**. Their producers are the
//! threads that must never stall — the UI/hotkey thread queues jobs, the loader
//! reports events — so a bounded `send` would block one of them whenever the
//! consumer was a frame behind, and the loader would stop serving hotkey
//! triggers. The volume is bounded in practice (one job per user action, one
//! event per load or failure) and `poll_events` drains the event queue every
//! frame, including while the window is hidden.

mod lock;
pub mod policy;

use lock::FileLock;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::audio::decode::decode_file;
use crate::audio::engine::{effective_gain, Engine};
use crate::audio::reader::{frames_for_seconds, probe_seconds};
use crate::audio::stream;

use policy::{Refusal, Route, Usage};

/// `skip` value meaning "no sound is being admitted": excludes nothing.
const NO_SKIP: u64 = u64::MAX;

struct Playable {
    path: PathBuf,
    /// Held for as long as the sound is on the board; denies delete and rename.
    lock: Option<FileLock>,
    volume: f32,
    /// Per-sound trim in dB, applied on top of `volume` (FR-5.3.17).
    gain_db: f32,
    samples: Option<Arc<Vec<f32>>>,
    loading: bool,
    pending_play: bool,
    error: Option<String>,
    missing: bool,
    /// Set when the cache refused this sound; it streams instead of retrying.
    refused: Option<Refusal>,
    /// Container duration, once probed. Used for stream progress.
    duration: Option<f32>,
    /// A `sounddeck-probe` worker is reading the duration right now.
    probe_pending: bool,
    generation: u64,
    retain: bool,
    /// Last time this buffer was played or admitted, used for LRU eviction.
    last_used: Instant,
}

impl Playable {
    fn new(path: PathBuf, volume: f32, gain_db: f32) -> Self {
        Self {
            path,
            lock: None,
            volume,
            gain_db,
            samples: None,
            loading: false,
            pending_play: false,
            error: None,
            missing: false,
            refused: None,
            duration: None,
            probe_pending: false,
            generation: 0,
            retain: false,
            last_used: Instant::now(),
        }
    }

    /// The linear gain a voice for this sound carries: the stored volume with
    /// the dB trim folded in. The engine is never handed the two separately.
    fn voice_gain(&self) -> f32 {
        effective_gain(self.volume, self.gain_db)
    }

    fn bytes(&self) -> usize {
        self.samples
            .as_ref()
            .map(|samples| samples.len() * policy::SAMPLE_BYTES)
            .unwrap_or(0)
    }

    /// Keeps the read lock current. A stale handle (unplugged volume) is replaced
    /// on the next attempt; failure is not fatal, playback decides what to do.
    fn ensure_lock(&mut self) {
        let valid = self.lock.as_ref().map(FileLock::is_valid).unwrap_or(false);
        if !valid {
            self.lock = FileLock::acquire(&self.path).ok();
        }
    }

    /// Drops everything that belonged to the old file or the old attempt.
    fn reset_audio(&mut self) {
        self.samples = None;
        self.error = None;
        self.loading = false;
        self.pending_play = false;
        self.refused = None;
        self.duration = None;
        self.probe_pending = false;
        self.generation += 1;
    }

    /// Snapshot for the UI. `missing` means "cannot be played from disk"; a
    /// resident buffer still plays, so it must not look broken.
    fn sound_info(&self) -> SoundInfo {
        SoundInfo {
            loading: self.loading,
            error: self.error.clone(),
            missing: self.missing && self.samples.is_none(),
            loaded: self.samples.is_some(),
            refused: self.refused.is_some(),
        }
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
    /// The sound works, but it is not kept in memory. One per sound per attempt:
    /// the registry carries the decision, the UI owns the sound name and asks
    /// [`Refusal::message`] for the wording.
    Refused { id: u64, reason: Refusal },
    /// Background worker problem that is not tied to one sound.
    Warning(String),
}

/// Outcome of one eviction pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    /// The sound fits now (possibly after evicting).
    Admitted,
    /// It does not fit, and this is the reason to report.
    Refused(Refusal),
    /// The registry is unusable, so nothing can be decided.
    Unavailable,
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
        // Unbounded on purpose; see the module header.
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

    pub fn register(&self, id: u64, path: PathBuf, volume: f32, gain_db: f32) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        if let Some(entry) = state.get_mut(&id) {
            if entry.path != path {
                entry.path = path;
                entry.lock = None;
                entry.reset_audio();
            }
            entry.volume = volume;
            entry.gain_db = gain_db;
            entry.ensure_lock();
            entry.missing = !entry.path.is_file();
            return;
        }
        let mut entry = Playable::new(path, volume, gain_db);
        entry.ensure_lock();
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
            entry.lock = None;
            entry.reset_audio();
            entry.ensure_lock();
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

    /// Live volume change. The trim is untouched, so the applied gain is
    /// volume × trim for whatever is sounding right now.
    pub fn set_volume(&self, id: u64, volume: f32) {
        let mut gain = None;
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.get_mut(&id) {
                entry.volume = volume;
                gain = Some(entry.voice_gain());
            }
        }
        if let Some(gain) = gain {
            self.engine.set_sound_volume(id, gain);
        }
    }

    /// Live trim change, the same path as [`Playback::set_volume`]: the volume is
    /// untouched and the new gain applies to a voice that is already sounding.
    pub fn set_gain(&self, id: u64, gain_db: f32) {
        let mut gain = None;
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.get_mut(&id) {
                entry.gain_db = gain_db;
                gain = Some(entry.voice_gain());
            }
        }
        if let Some(gain) = gain {
            self.engine.set_sound_volume(id, gain);
        }
    }

    /// Retry the read lock for a configured sound. Used after startup failures
    /// (file briefly held by another app) and after a volume reconnects.
    pub fn ensure_lock(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.get_mut(&id) {
                entry.ensure_lock();
            }
        }
    }

    /// Enable or disable keeping this sound in RAM.
    ///
    /// Re-enabling is a fresh attempt, so a previous refusal is dropped. A call
    /// that does not change the flag (the tab-switch sync re-asserts it on every
    /// switch) is a no-op: clearing the refusal there would decode the same
    /// unfittable file again and log the same line again on every switch.
    pub fn set_retain(&self, id: u64, retain: bool) {
        let mut job = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.get_mut(&id) else {
                return;
            };
            let was_retained = entry.retain;
            entry.retain = retain;
            if retain {
                if !was_retained {
                    entry.refused = None;
                }
                if entry.samples.is_none() && !entry.loading && entry.refused.is_none() {
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
            entry.probe_pending = false;
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
    ///
    /// Runs on the UI or hotkey thread, so it never probes the container and
    /// never holds the state lock across an engine call (see the module header).
    /// The only disk touches left are a `stat` (the missing-file contract needs
    /// it) and a retry of a stale read lock; neither opens or parses the audio.
    pub fn play(&self, id: u64) {
        let mut job = None;
        let mut stream_request = None;
        let mut probe = None;
        let mut resident = None;
        let mut failure = None;
        {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(entry) = state.get_mut(&id) else {
                return;
            };
            entry.last_used = Instant::now();
            if let Some(samples) = entry.samples.clone() {
                // Resident: take the gain along with the samples and leave.
                resident = Some((samples, entry.voice_gain()));
            } else {
                entry.ensure_lock();
                entry.missing = !entry.path.is_file();
                if entry.missing {
                    let message = missing_message(&entry.path);
                    entry.error = Some(message.clone());
                    failure = Some(message);
                } else if entry.loading {
                    entry.pending_play = true;
                } else if entry.retain && entry.refused.is_none() {
                    entry.loading = true;
                    entry.pending_play = true;
                    job = Some(Job {
                        id,
                        generation: entry.generation,
                        verbose: false,
                        retain: true,
                    });
                } else {
                    // Streams start immediately. The duration is only what we
                    // already know; a worker fills it in for the next trigger.
                    if entry.duration.is_none() && !entry.probe_pending {
                        entry.probe_pending = true;
                        probe = Some((entry.path.clone(), entry.generation));
                    }
                    stream_request = Some((entry.path.clone(), entry.voice_gain(), entry.duration));
                }
            }
        }
        if let Some((samples, gain)) = resident {
            self.engine.play(id, samples, gain);
            return;
        }
        if let Some(message) = failure {
            let _ = self.events_tx.send(PlaybackEvent::Failed {
                id,
                message,
                missing: true,
            });
            return;
        }
        if let Some(job) = job {
            let _ = self.jobs.send(job);
        }
        if let Some((path, generation)) = probe {
            self.spawn_probe(id, path, generation);
        }
        if let Some((path, gain, duration)) = stream_request {
            self.start_stream(id, &path, gain, duration);
        }
    }

    pub fn stop_sound(&self, id: u64) {
        self.engine.stop_sound(id);
    }

    pub fn stop_all(&self) {
        self.engine.stop_all();
    }

    pub fn info(&self, id: u64) -> SoundInfo {
        let Ok(state) = self.state.lock() else {
            return SoundInfo::default();
        };
        state.get(&id).map(Playable::sound_info).unwrap_or_default()
    }

    /// One lock for a whole frame's worth of cards.
    pub fn info_map(&self, ids: &[u64]) -> HashMap<u64, SoundInfo> {
        let mut out = HashMap::with_capacity(ids.len());
        let Ok(state) = self.state.lock() else {
            return out;
        };
        for id in ids {
            if let Some(entry) = state.get(id) {
                out.insert(*id, entry.sound_info());
            }
        }
        out
    }

    /// Records whether the file behind a sound is reachable.
    ///
    /// The message only stays while the sound cannot play at all: a resident
    /// buffer plays fine even when the volume it came from is gone.
    pub fn mark_missing(&self, id: u64, missing: bool, message: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.get_mut(&id) {
                entry.missing = missing;
                let unplayable = missing && entry.samples.is_none();
                entry.error = if unplayable {
                    Some(message.unwrap_or_else(|| missing_message(&entry.path)))
                } else {
                    None
                };
            }
        }
    }

    /// Current cache usage, for diagnostics and the settings screen.
    pub fn cache_usage(&self) -> Usage {
        match self.state.lock() {
            Ok(state) => usage_of(&state, NO_SKIP),
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

    /// Reads a stream's duration off the trigger path and caches it.
    ///
    /// One detached worker at a time per sound: `probe_pending` deduplicates, and
    /// it is cleared here (before the generation check) so a stale probe never
    /// wedges the flag.
    fn spawn_probe(&self, id: u64, path: PathBuf, generation: u64) {
        let state = self.state.clone();
        let events = self.events_tx.clone();
        let spawned = thread::Builder::new()
            .name("sounddeck-probe".into())
            .spawn(move || {
                let duration = probe_seconds(&path);
                let Ok(mut state) = state.lock() else {
                    return;
                };
                let Some(entry) = state.get_mut(&id) else {
                    return;
                };
                entry.probe_pending = false;
                if entry.generation == generation {
                    if let Some(duration) = duration {
                        entry.duration = Some(duration);
                    }
                }
            });
        if spawned.is_err() {
            if let Ok(mut state) = self.state.lock() {
                if let Some(entry) = state.get_mut(&id) {
                    entry.probe_pending = false;
                }
            }
            let _ = events.send(PlaybackEvent::Warning(
                "Could not start the duration probe; stream progress stays hidden.".into(),
            ));
        }
    }

    fn start_stream(&self, id: u64, path: &Path, gain: f32, duration: Option<f32>) {
        match stream::spawn_once(path) {
            Ok(rx) => {
                let total = duration.map(frames_for_seconds);
                self.engine.play_stream(id, rx, gain, total);
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

/// The one wording for "the file is not there", shared by [`Playback::play`] and
/// [`Playback::mark_missing`] so the two paths cannot drift.
fn missing_message(path: &Path) -> String {
    format!("File not found: {}", path.display())
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

/// Gain as it is *now*, for a play that was deferred while the file was
/// decoding. The volume and the trim are read here rather than carried from the
/// job, so a slider move or a gain step during the decode is still honoured, and
/// the two always travel together through one conversion.
fn playback_snapshot(
    state: &Mutex<HashMap<u64, Playable>>,
    id: u64,
) -> Option<(Option<Arc<Vec<f32>>>, f32)> {
    let state = state.lock().ok()?;
    let entry = state.get(&id)?;
    Some((entry.samples.clone(), entry.voice_gain()))
}

/// Picks the buffers to drop so `need` more bytes fit: LRU order (`candidates`
/// is oldest first), never a playing sound.
///
/// Eviction only happens when the cache is what is in the way
/// ([`Refusal::evictable`]). A sound that is too big on its own, or a process
/// already at its ceiling, is refused with **no** victims, so one unfittable
/// file in a preload batch cannot dump everything the batch has already loaded
/// (B1).
///
/// Running totals only, so the pass is O(candidates) instead of re-summing the
/// whole cache per victim. Returns every non-playing candidate when even that is
/// not enough; the caller then refuses on the fresh numbers.
fn plan_evictions(
    usage: Usage,
    candidates: &[(Instant, u64, usize)],
    need: usize,
    process: Option<usize>,
    is_playing: impl Fn(u64) -> bool,
) -> Vec<u64> {
    match policy::admit(usage, need, process) {
        Ok(()) => return Vec::new(),
        Err(reason) if !reason.evictable() => return Vec::new(),
        Err(_) => {}
    }
    let mut bytes = usage.bytes;
    let mut sounds = usage.sounds;
    let mut victims = Vec::new();
    for &(_, id, size) in candidates {
        if is_playing(id) {
            continue;
        }
        victims.push(id);
        bytes = bytes.saturating_sub(size);
        sounds = sounds.saturating_sub(1);
        if policy::admit(Usage { bytes, sounds }, need, process).is_ok() {
            break;
        }
    }
    victims
}

/// Decides, in one place, how an admission result maps to a pass outcome.
fn verdict(result: Result<(), Refusal>) -> Admission {
    match result {
        Ok(()) => Admission::Admitted,
        Err(reason) => Admission::Refused(reason),
    }
}

/// Frees cached buffers until `need` more bytes fit, then reports whether they
/// now do.
///
/// How many buffers to drop — including "none, this cannot be fixed" — is
/// [`plan_evictions`]'s decision; this function only collects the candidates,
/// applies the victims and re-checks on fresh numbers.
///
/// The engine is asked **outside** the state lock (§4.2 lock order): collect the
/// candidates, ask which of them are playing, then apply the eviction under a
/// fresh lock. Evicting only drops the cache reference — a playing voice owns its
/// own `Arc` — so the worst case is a sound that streams again next time.
fn evict_to_fit(
    state: &Mutex<HashMap<u64, Playable>>,
    engine: &Engine,
    need: usize,
    skip: u64,
) -> Admission {
    let process = policy::process_bytes();
    let (usage, candidates) = {
        let Ok(state) = state.lock() else {
            return Admission::Unavailable;
        };
        let usage = usage_of(&state, skip);
        if policy::admit(usage, need, process).is_ok() {
            return Admission::Admitted;
        }
        let mut candidates: Vec<(Instant, u64, usize)> = state
            .iter()
            .filter(|(id, entry)| **id != skip && entry.samples.is_some())
            .map(|(id, entry)| (entry.last_used, *id, entry.bytes()))
            .collect();
        candidates.sort_by_key(|(last_used, _, _)| *last_used);
        (usage, candidates)
    };

    let victims = plan_evictions(usage, &candidates, need, process, |id| {
        engine.is_playing_sound(id)
    });
    if victims.is_empty() {
        return verdict(policy::admit(usage, need, process));
    }

    // The world may have moved while the engine was queried, so drop only what is
    // still resident and decide on the fresh numbers.
    let Ok(mut state) = state.lock() else {
        return Admission::Unavailable;
    };
    for id in &victims {
        if let Some(entry) = state.get_mut(id) {
            entry.samples = None;
        }
    }
    verdict(policy::admit(usage_of(&state, skip), need, process))
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
                Some((entry.path.clone(), entry.retain))
            })
        };
        let Some((path, retain)) = snapshot else {
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
                // Preload being switched off is a choice, not a refusal; it must
                // not clear the per-sound toggle or spam the log.
                let intentional = reason == Refusal::PreloadOff;
                let pending;
                let report;
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
                    // Skip and continue: this sound streams from disk and the
                    // loader takes the next job. The line is reported once — a
                    // repeated attempt with the same verdict stays quiet.
                    report = !intentional && entry.refused != Some(reason);
                    if !intentional {
                        entry.refused = Some(reason);
                    }
                    if duration.is_some() {
                        entry.duration = duration;
                    }
                    pending = std::mem::take(&mut entry.pending_play);
                }
                if report {
                    let _ = events.send(PlaybackEvent::Refused { id: job.id, reason });
                }
                if pending {
                    let Some((_, gain)) = playback_snapshot(&state, job.id) else {
                        continue;
                    };
                    start_stream_from(&engine, &events, &state, job.id, &path, gain, duration);
                }
            }
            Route::Preload => {
                let decoded = decode_file(&path);
                match decoded {
                    Ok(audio) => {
                        let bytes = audio.samples.len() * policy::SAMPLE_BYTES;
                        let (keep, refusal) = match evict_to_fit(&state, &engine, bytes, job.id) {
                            Admission::Admitted => (true, None),
                            Admission::Refused(reason) => (false, Some(reason)),
                            Admission::Unavailable => continue,
                        };
                        let pending;
                        let mut completed = None;
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
                            entry.missing = false;
                            entry.duration = Some(audio.duration_secs);
                            pending = std::mem::take(&mut entry.pending_play);
                            if keep {
                                entry.samples = Some(audio.samples.clone());
                                entry.refused = None;
                                completed = Some((audio.duration_secs, bytes));
                            } else {
                                entry.refused = refusal;
                            }
                        }
                        if let Some((duration, bytes)) = completed {
                            let _ = events.send(PlaybackEvent::Loaded {
                                id: job.id,
                                duration,
                                bytes,
                                verbose: job.verbose,
                            });
                        } else if let Some(refusal) = refusal {
                            let _ = events.send(PlaybackEvent::Refused {
                                id: job.id,
                                reason: refusal,
                            });
                        }
                        if pending {
                            // Same lock that fetches the samples supplies the
                            // gain, so a level change during the decode counts.
                            let Some((samples, gain)) = playback_snapshot(&state, job.id) else {
                                continue;
                            };
                            match samples {
                                Some(samples) => engine.play(job.id, samples, gain),
                                None => start_stream_from(
                                    &engine, &events, &state, job.id, &path, gain, duration,
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
    gain: f32,
    duration: Option<f32>,
) {
    match stream::spawn_once(path) {
        Ok(rx) => engine.play_stream(id, rx, gain, duration.map(frames_for_seconds)),
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

    /// LRU candidate list, oldest first, as [`evict_to_fit`] builds it.
    fn lru(sizes: &[(u64, usize)]) -> Vec<(Instant, u64, usize)> {
        let base = Instant::now();
        sizes
            .iter()
            .enumerate()
            .map(|(index, (id, size))| (base - Duration::from_secs(100 - index as u64), *id, *size))
            .collect()
    }

    const MIB: usize = 1024 * 1024;

    #[test]
    fn a_long_sound_is_refused() {
        let dir = temp_dir("long");
        let path = dir.join("long.wav");
        write_wav(&path, 40);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(1, path, 1.0, 0.0);
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
        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_short_sound_is_retained() {
        let dir = temp_dir("short");
        let path = dir.join("short.wav");
        write_wav(&path, 1);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(1, path, 1.0, 0.0);
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
        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_long_sound_still_plays_from_disk() {
        let dir = temp_dir("stream");
        let path = dir.join("long.wav");
        write_wav(&path, 40);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(2, path, 1.0, 0.0);
        playback.set_retain(2, false);
        playback.play(2);
        assert!(playback.cache_usage().sounds == 0);
        assert!(!playback.info(2).loaded);
        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_reports_failure() {
        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(3, PathBuf::from("Z:\\nope.wav"), 1.0, 0.0);
        playback.play(3);
        let events = wait_for_events(&playback, Duration::from_secs(2));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, PlaybackEvent::Failed { missing: true, .. })),
            "got {events:?}"
        );
    }

    #[test]
    fn removing_a_sound_releases_its_file() {
        let dir = temp_dir("unlock");
        let path = dir.join("tone.wav");
        write_wav(&path, 1);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(9, path.clone(), 1.0, 0.0);

        assert!(
            fs::remove_file(&path).is_err(),
            "a configured sound must be locked against deletion"
        );
        playback.remove(9);
        assert!(
            fs::remove_file(&path).is_ok(),
            "removing the card must release the lock"
        );
        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_resident_sound_ignores_a_missing_file() {
        let dir = temp_dir("resident-missing");
        let path = dir.join("tone.wav");
        write_wav(&path, 1);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine.clone());
        playback.register(4, path, 1.0, 0.0);
        playback.set_retain(4, true);
        let events = wait_for_events(&playback, Duration::from_secs(5));
        assert!(events
            .iter()
            .any(|event| matches!(event, PlaybackEvent::Loaded { .. })));

        // The volume disappears: the path is unreachable, the samples are not.
        playback.mark_missing(4, true, Some("volume disconnected".into()));
        playback.play(4);

        assert!(engine.is_playing(), "resident audio must still play");
        assert!(
            !playback
                .poll_events()
                .iter()
                .any(|event| matches!(event, PlaybackEvent::Failed { .. })),
            "no failure for a resident sound"
        );
        assert!(
            !playback.info(4).missing,
            "a resident sound must not look missing"
        );
        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_resident_sound_keeps_no_error_when_the_file_disappears() {
        let dir = temp_dir("resident-error");
        let path = dir.join("tone.wav");
        write_wav(&path, 1);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(12, path, 1.0, 0.0);
        playback.set_retain(12, true);
        let events = wait_for_events(&playback, Duration::from_secs(5));
        assert!(events
            .iter()
            .any(|event| matches!(event, PlaybackEvent::Loaded { .. })));

        playback.mark_missing(12, true, Some("volume disconnected".into()));

        let info = playback.info(12);
        assert!(info.loaded, "the buffer is still there");
        assert!(
            info.error.is_none(),
            "a resident sound plays fine, so it must not carry an error: {:?}",
            info.error
        );
        assert!(!info.missing);
        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_sound_that_cannot_play_reports_the_message() {
        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(13, PathBuf::from("Z:\\nope.wav"), 1.0, 0.0);

        playback.mark_missing(13, true, Some("volume disconnected".into()));

        let info = playback.info(13);
        assert!(info.missing);
        assert_eq!(info.error.as_deref(), Some("volume disconnected"));

        // No message supplied still yields the shared wording, not silence.
        playback.mark_missing(13, true, None);
        assert_eq!(
            playback.info(13).error.as_deref(),
            Some(missing_message(Path::new("Z:\\nope.wav")).as_str())
        );

        // The file is back: the error clears.
        playback.mark_missing(13, false, None);
        assert!(!playback.info(13).missing);
        assert!(playback.info(13).error.is_none());
    }

    #[test]
    fn a_deferred_play_reads_the_level_at_playback_time() {
        let state = Mutex::new(HashMap::new());
        {
            let mut entries = state.lock().unwrap();
            let mut entry = Playable::new(PathBuf::from("tone.wav"), 0.9, 0.0);
            entry.samples = Some(Arc::new(vec![0.0; 480]));
            entries.insert(5, entry);
        }
        // The job was queued at 0.9, then the slider moved while it decoded.
        if let Ok(mut entries) = state.lock() {
            if let Some(entry) = entries.get_mut(&5) {
                entry.volume = 0.2;
            }
        }

        let (samples, gain) = playback_snapshot(&state, 5).unwrap();
        assert!(samples.is_some());
        assert_eq!(gain, 0.2, "the deferred play must use the latest volume");
        assert!(playback_snapshot(&state, 404).is_none());

        // A trim set during the decode is part of the same snapshot: the voice
        // gets volume x trim, never one without the other.
        if let Ok(mut entries) = state.lock() {
            if let Some(entry) = entries.get_mut(&5) {
                entry.gain_db = 6.0;
            }
        }
        let (_, gain) = playback_snapshot(&state, 5).unwrap();
        assert!((gain - 0.2 * 1.995_262).abs() < 1e-4, "got {gain}");
    }

    #[test]
    fn registering_a_sound_keeps_its_trim_with_its_volume() {
        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(5, PathBuf::from("Z:\\nope.wav"), 0.5, -3.0);

        let gain = playback
            .state
            .lock()
            .ok()
            .and_then(|state| state.get(&5).map(Playable::voice_gain))
            .unwrap_or_default();
        // 10^(-3/20) = 0.70795; half volume on top of that.
        assert!((gain - 0.353_973).abs() < 1e-4, "got {gain}");

        // Re-registering (a relink, a config import) replaces both values.
        playback.register(5, PathBuf::from("Z:\\nope.wav"), 1.0, 0.0);
        let gain = playback
            .state
            .lock()
            .ok()
            .and_then(|state| state.get(&5).map(Playable::voice_gain))
            .unwrap_or_default();
        assert!((gain - 1.0).abs() < 1e-6, "got {gain}");
    }

    #[test]
    fn a_level_change_reaches_the_voice_that_is_already_playing() {
        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine.clone());
        playback.register(31, PathBuf::from("Z:\\nope.wav"), 0.5, 0.0);
        // A stand-in voice: no decode, just something for the live path to reach.
        // The buffer is deliberately long (5 s): a real mixer thread consumes the
        // voice as soon as it plays, and a 5 ms stand-in can vanish before the
        // assertions below run, which made this test flaky.
        engine.play(31, Arc::new(vec![0.0; 48_000 * 5 * 2]), 0.5);

        // A gain step keeps the volume and re-applies the product.
        playback.set_gain(31, 6.0);
        let gain = engine.voice_gain(31).expect("the voice is still sounding");
        assert!((gain - 0.5 * 1.995_262).abs() < 1e-4, "got {gain}");

        // A slider move keeps the trim.
        playback.set_volume(31, 1.0);
        let gain = engine.voice_gain(31).expect("the voice is still sounding");
        assert!((gain - 1.995_262).abs() < 1e-4, "got {gain}");
    }

    #[test]
    fn a_deferred_play_starts_when_the_decode_lands() {
        let dir = temp_dir("deferred");
        let path = dir.join("tone.wav");
        write_wav(&path, 1);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine.clone());
        playback.register(21, path, 1.0, 0.0);
        playback.set_retain(21, true);
        playback.play(21);
        playback.set_volume(21, 0.25);

        let events = wait_for_events(&playback, Duration::from_secs(5));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, PlaybackEvent::Loaded { .. })),
            "got {events:?}"
        );
        assert!(engine.is_playing(), "the deferred play must start on load");
        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_streamed_sound_gets_its_duration_from_the_probe_worker() {
        let dir = temp_dir("probe");
        let path = dir.join("long.wav");
        write_wav(&path, 40);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(11, path, 1.0, 0.0);
        playback.play(11);

        let start = Instant::now();
        let duration = loop {
            let duration = playback
                .state
                .lock()
                .ok()
                .and_then(|state| state.get(&11).and_then(|entry| entry.duration));
            if duration.is_some() {
                break duration;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the probe worker never reported a duration"
            );
            thread::sleep(Duration::from_millis(20));
        };
        assert!((duration.unwrap_or_default() - 40.0).abs() < 0.5);
        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn eviction_takes_the_oldest_buffers_until_the_new_one_fits() {
        let resident = [(1u64, 8 * MIB), (2, 8 * MIB), (3, 8 * MIB)];
        let usage = Usage {
            bytes: 24 * MIB,
            sounds: resident.len(),
        };
        let need = 10 * MIB; // 24 + 10 > 32, 16 + 10 fits.

        let victims = plan_evictions(usage, &lru(&resident), need, Some(0), |_| false);
        assert_eq!(victims, vec![1], "only the oldest buffer has to go");

        // The same pass with the oldest busy playing takes the next one.
        let victims = plan_evictions(usage, &lru(&resident), need, Some(0), |id| id == 1);
        assert_eq!(victims, vec![2]);

        // Nothing to free when the sound already fits.
        let victims = plan_evictions(usage, &lru(&resident), MIB, Some(0), |_| false);
        assert!(victims.is_empty());
    }

    #[test]
    fn eviction_counts_the_sound_cap_as_well_as_the_bytes() {
        let resident = [(1u64, MIB), (2, MIB), (3, MIB)];
        let usage = Usage {
            bytes: 16 * MIB,
            sounds: policy::RETAINED_SOUND_CAP,
        };
        assert_eq!(
            policy::admit(usage, MIB, Some(0)),
            Err(Refusal::SoundCap),
            "the count cap is what makes this fail"
        );

        let victims = plan_evictions(usage, &lru(&resident), MIB, Some(0), |_| false);
        assert_eq!(victims, vec![1]);
        // Accounting after the eviction: one sound fewer, byte total reduced.
        assert_eq!(
            policy::admit(
                Usage {
                    bytes: usage.bytes - MIB,
                    sounds: usage.sounds - 1
                },
                MIB,
                Some(0)
            ),
            Ok(())
        );
    }

    #[test]
    fn eviction_gives_up_when_the_process_ceiling_is_the_reason() {
        let resident = [(1u64, MIB), (2, MIB)];
        let usage = Usage {
            bytes: 2 * MIB,
            sounds: 2,
        };
        let pinned = Some(policy::PROCESS_CEILING);

        // The ceiling is measured once per pass, so dropping our own buffers
        // cannot move the verdict: the pass must not evict anything for it.
        let victims = plan_evictions(usage, &lru(&resident), MIB, pinned, |_| false);
        assert!(
            victims.is_empty(),
            "a process-ceiling refusal must not empty the cache"
        );
        assert_eq!(
            policy::admit(
                Usage {
                    bytes: 0,
                    sounds: 0
                },
                MIB,
                pinned
            ),
            Err(Refusal::ProcessCeiling)
        );
    }

    /// B1: one file that can never fit must not take the cache — and with it the
    /// rest of a preload batch — down with it.
    #[test]
    fn an_unfittable_sound_leaves_the_resident_cache_alone() {
        let engine = Engine::new(1.0, 1.0, 0.0);
        let frame = Arc::new(vec![0.0f32; 96_000]); // one second, stereo
        let base = Instant::now();

        let mut entries = HashMap::new();
        for id in 0..3u64 {
            let mut entry = Playable::new(PathBuf::from(format!("{id}.wav")), 1.0, 0.0);
            entry.samples = Some(frame.clone());
            entry.last_used = base - Duration::from_secs(100 - id);
            entries.insert(id, entry);
        }
        let state = Mutex::new(entries);

        // Larger than one sound may ever be, so no eviction can help.
        let admission = evict_to_fit(&state, &engine, policy::PER_SOUND_RETAIN_BYTES + 1, 99);
        assert_eq!(admission, Admission::Refused(Refusal::TooLarge));

        let state = state.lock().unwrap();
        assert_eq!(
            state
                .values()
                .filter(|entry| entry.samples.is_some())
                .count(),
            3,
            "a refusal that eviction cannot fix must not drop a single buffer"
        );
    }

    /// B1: the loader keeps going. A sound that cannot be kept is skipped (it
    /// streams), the sounds after it still preload, and the skip is reported once.
    #[test]
    fn a_preload_batch_keeps_going_after_a_sound_that_cannot_fit() {
        let dir = temp_dir("batch");
        let first = dir.join("first.wav");
        let too_long = dir.join("too-long.wav");
        let last = dir.join("last.wav");
        write_wav(&first, 1);
        write_wav(&too_long, 40);
        write_wav(&last, 1);

        let engine = Arc::new(Engine::new(1.0, 1.0, 0.0));
        let playback = Playback::new(engine);
        playback.register(41, first, 1.0, 0.0);
        playback.register(42, too_long, 1.0, 0.0);
        playback.register(43, last, 1.0, 0.0);

        // What the RAM pill does: retain everything, then ask for it.
        for id in [41, 42, 43] {
            playback.set_retain(id, true);
            playback.ensure_loaded(id);
        }

        let start = Instant::now();
        let mut loaded = Vec::new();
        let mut refused = Vec::new();
        while start.elapsed() < Duration::from_secs(10) && (loaded.len() < 2 || refused.is_empty())
        {
            for event in playback.poll_events() {
                match event {
                    PlaybackEvent::Loaded { id, .. } => loaded.push(id),
                    PlaybackEvent::Refused { id, .. } => refused.push(id),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(20));
        }

        loaded.sort_unstable();
        assert_eq!(
            loaded,
            vec![41, 43],
            "the sounds around the skipped one must still preload"
        );
        assert_eq!(refused, vec![42], "only the unfittable sound is refused");
        assert_eq!(playback.cache_usage().sounds, 2);
        assert!(playback.info(42).refused);
        assert!(!playback.info(42).loaded);

        // Asking again with the same verdict must not report it twice.
        playback.set_retain(42, true);
        playback.ensure_loaded(42);
        thread::sleep(Duration::from_millis(100));
        assert!(
            !playback
                .poll_events()
                .iter()
                .any(|event| matches!(event, PlaybackEvent::Refused { .. })),
            "a skipped sound is logged once, not once per attempt"
        );

        drop(playback);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_full_cache_drops_the_oldest_buffer_but_never_the_admitted_sound() {
        let engine = Engine::new(1.0, 1.0, 0.0);
        let frame = Arc::new(vec![0.0f32; 96_000]); // one second, stereo
        let bytes = frame.len() * policy::SAMPLE_BYTES;
        let base = Instant::now();
        let admitted_id = 999u64;

        let mut entries = HashMap::new();
        for id in 0..policy::RETAINED_SOUND_CAP as u64 {
            let mut entry = Playable::new(PathBuf::from(format!("{id}.wav")), 1.0, 0.0);
            entry.samples = Some(frame.clone());
            entry.last_used = base - Duration::from_secs(1000 - id);
            entries.insert(id, entry);
        }
        // The sound being admitted is the least recently used of them all: it
        // must still survive, or admitting it would evict itself.
        let mut head = Playable::new(PathBuf::from("head.wav"), 1.0, 0.0);
        head.samples = Some(frame.clone());
        head.last_used = base - Duration::from_secs(10_000);
        entries.insert(admitted_id, head);

        let state = Mutex::new(entries);
        assert_eq!(
            evict_to_fit(&state, &engine, bytes, admitted_id),
            Admission::Admitted
        );

        let state = state.lock().unwrap();
        assert!(
            state
                .get(&admitted_id)
                .map(|entry| entry.samples.is_some())
                .unwrap_or(false),
            "the admitted sound must keep its buffer"
        );
        assert!(
            state
                .get(&0)
                .map(|entry| entry.samples.is_none())
                .unwrap_or(false),
            "the oldest resident buffer should have been dropped"
        );
        assert!(state
            .get(&1)
            .map(|entry| entry.samples.is_some())
            .unwrap_or(false));
        assert_eq!(
            state
                .values()
                .filter(|entry| entry.samples.is_some())
                .count(),
            policy::RETAINED_SOUND_CAP
        );
    }
}
