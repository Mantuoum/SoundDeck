use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, Color32, ComboBox, CornerRadius, Frame, Id, Key, LayerId, Layout,
    Margin, Order, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
    ViewportCommand,
};

use crate::audio::engine::{AudioEvent, DeviceRoute, Engine, Slot};
use crate::audio::wasapi::{self, DeviceInfo};
use crate::hotkeys::{Hotkey, HotkeyService, ServiceEvent, TrayCommand};
use crate::icon;
use crate::model::{
    self, display_name, format_bytes, format_duration, is_supported_file, save_config, AppTheme,
    Config, Layout as SoundLayout, SoundConfig, APP_NAME, SUPPORTED_EXTENSIONS,
};
use crate::playback::{Playback, PlaybackEvent, SoundInfo};
use crate::ui::theme;

const CARD_HEIGHT: f32 = 112.0;
const ROW_HEIGHT: f32 = 46.0;
const SIDEBAR_WIDTH: f32 = 300.0;

const DEVELOPER_NAME: &str = "Your Name";
const DEVELOPER_ROLE: &str = "Independent developer";
const DEVELOPER_LINK: &str = "your-link.example";
const DEVELOPER_BLURB: &str = "Add your bio, links, and contact details here.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum RamState {
    Off,
    On,
    Mixed,
}

pub struct Sound {
    pub id: u64,
    pub name: String,
    pub path: PathBuf,
    pub hotkey: Option<Hotkey>,
    pub hotkey_error: Option<String>,
    pub volume: f32,
    /// Only meaningful for sounds with a hotkey. Everything else always plays
    /// from disk.
    pub ram: bool,
    pub flash_until: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Success,
    Warning,
    Error,
}

impl StatusKind {
    fn color(self) -> Color32 {
        match self {
            Self::Info => theme::muted(),
            Self::Success => theme::accent(),
            Self::Warning => theme::amber(),
            Self::Error => theme::red(),
        }
    }
}

struct StatusEntry {
    kind: StatusKind,
    text: String,
    at: Instant,
}

struct CaptureState {
    sound_id: u64,
    current: Option<Hotkey>,
}

struct RenameState {
    sound_id: u64,
    buffer: String,
    focus: bool,
}

struct DragState {
    sound_id: u64,
    from: usize,
    size: Vec2,
    name: String,
    hotkey: String,
}

struct UndoDelete {
    sound: Sound,
    index: usize,
}

enum Action {
    Play(u64),
    Stop(u64),
    Remove(u64),
    Rename(u64),
    SetHotkey(u64),
    ClearHotkey(u64),
    ToggleRam(u64),
    Reload(u64),
    Locate(u64),
    ShowInExplorer(u64),
    Volume(u64, f32),
    DragStart { id: u64, index: usize, size: Vec2 },
    DragStop,
}

pub struct App {
    cfg: Config,
    cfg_path: PathBuf,
    sounds: Vec<Sound>,
    playback: Arc<Playback>,
    engine: Arc<Engine>,
    hotkeys: HotkeyService,
    quit_flag: Arc<AtomicBool>,
    devices: Vec<DeviceInfo>,
    device_ready: [bool; 2],
    device_note: [Option<String>; 2],
    tray_available: bool,
    hidden: bool,
    quit: bool,
    search: String,
    focus_search: bool,
    selected: Option<u64>,
    statuses: Vec<StatusEntry>,
    show_log: bool,
    show_settings: bool,
    capture: Option<CaptureState>,
    renaming: Option<RenameState>,
    drag: Option<DragState>,
    card_rects: Vec<(usize, Rect)>,
    row_rects: Vec<(usize, Rect)>,
    volume_rects: Vec<(u64, Rect)>,
    undo: Option<UndoDelete>,
    last_dir: Option<PathBuf>,
    dirty: bool,
    last_save: Instant,
    last_tip: String,
    next_tip: Instant,
    last_window_size: Option<[f32; 2]>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        loaded: model::LoadOutcome,
        open_settings: bool,
    ) -> Self {
        let cfg_path = model::config_path();
        let mut cfg = loaded.config;
        theme::install(&cc.egui_ctx, cfg.theme);

        let mut statuses = Vec::new();
        let mut push_status = |kind: StatusKind, text: String| {
            statuses.push(StatusEntry {
                kind,
                text,
                at: Instant::now(),
            });
        };

        if let Some(warning) = &loaded.warning {
            push_status(StatusKind::Warning, warning.clone());
        }

        let mut sounds = Vec::new();
        for sound in &cfg.sounds {
            let path = model::resolve_path(&sound.path);
            let hotkey = match sound.hotkey.as_deref().map(Hotkey::parse) {
                Some(Ok(hk)) => Some(hk),
                Some(Err(err)) => {
                    push_status(
                        StatusKind::Warning,
                        format!("Hotkey for \"{}\" is invalid: {err}", sound.name),
                    );
                    None
                }
                None => None,
            };
            let ram = hotkey.is_some()
                && sound.ram.unwrap_or(cfg.preload_hotkey_sounds);
            sounds.push(Sound {
                id: sound.id,
                name: sound.name.clone(),
                path,
                hotkey,
                hotkey_error: None,
                volume: sound.volume.clamp(0.0, 1.0),
                ram,
                flash_until: None,
            });
        }

        let engine = Arc::new(Engine::new(
            cfg.monitor_volume,
            if cfg.link_volumes {
                cfg.monitor_volume
            } else {
                cfg.cable_volume
            },
            cfg.master_gain_db,
        ));
        if cfg.link_volumes {
            cfg.cable_volume = cfg.monitor_volume;
        }
        let playback = Arc::new(Playback::new(engine.clone()));

        for sound in &sounds {
            playback.register(sound.id, sound.path.clone(), sound.volume);
        }
        for sound in &sounds {
            if sound.hotkey.is_some() && sound.ram {
                playback.set_retain(sound.id, true);
                playback.ensure_loaded(sound.id);
            }
        }

        let devices_result = wasapi::list_devices();
        let mut devices = Vec::new();
        let mut default_id = None;
        match devices_result {
            Ok(list) => {
                devices = list.devices;
                default_id = list.default_id;
            }
            Err(err) => push_status(
                StatusKind::Error,
                format!("Could not list audio devices: {err}"),
            ),
        }

        // Resolve configured endpoints, falling back to the Windows default and
        // then the first available render device.
        let monitor = resolve_device(
            &devices,
            cfg.monitor_device.as_deref(),
            cfg.monitor_name.as_deref(),
        )
        .or_else(|| {
            default_id
                .as_deref()
                .and_then(|id| devices.iter().find(|d| d.id == id).cloned())
        })
        .or_else(|| devices.first().cloned());

        let mut cable = resolve_device(
            &devices,
            cfg.cable_device.as_deref(),
            cfg.cable_name.as_deref(),
        );
        if cable.is_none() {
            cable = devices
                .iter()
                .find(|d| {
                    let name = d.name.to_ascii_lowercase();
                    (name.contains("cable") || name.contains("virtual"))
                        && monitor.as_ref().map(|m| m.id != d.id).unwrap_or(true)
                })
                .cloned();
            if let Some(found) = &cable {
                push_status(
                    StatusKind::Info,
                    format!("Auto-selected virtual cable: {}", found.name),
                );
            }
        }

        if let Some(device) = &monitor {
            cfg.monitor_device = Some(device.id.clone());
            cfg.monitor_name = Some(device.name.clone());
        }
        if let Some(device) = &cable {
            cfg.cable_device = Some(device.id.clone());
            cfg.cable_name = Some(device.name.clone());
        }
        if devices.is_empty() {
            push_status(
                StatusKind::Warning,
                "No audio output devices were found. Plug one in and press Refresh.".into(),
            );
        }

        engine.set_routes(
            monitor.clone().map(|d| DeviceRoute {
                id: d.id,
                name: d.name,
            }),
            cable.clone().map(|d| DeviceRoute {
                id: d.id,
                name: d.name,
            }),
        );

        let icon_rgba = icon::decode_png(include_bytes!("../assets/icon.png"))
            .map(|(rgba, w, h)| (rgba, w, h))
            .unwrap_or_else(|_| (vec![0, 0, 0, 255], 1, 1));
        let tray_icon = icon::resize_rgba(&icon_rgba.0, icon_rgba.1, icon_rgba.2, 32);
        let quit_flag = Arc::new(AtomicBool::new(false));
        let hotkeys = HotkeyService::start(
            tray_icon,
            32,
            format!("{APP_NAME} - {} sounds", sounds.len()),
            playback.clone(),
            quit_flag.clone(),
        );

        let mut app = Self {
            cfg,
            cfg_path,
            sounds,
            playback,
            engine,
            hotkeys,
            quit_flag,
            devices,
            device_ready: [false, false],
            device_note: [None, None],
            tray_available: false,
            hidden: false,
            quit: false,
            search: String::new(),
            focus_search: false,
            selected: None,
            statuses,
            show_log: false,
            show_settings: open_settings,
            capture: None,
            renaming: None,
            drag: None,
            card_rects: Vec::new(),
            row_rects: Vec::new(),
            volume_rects: Vec::new(),
            undo: None,
            last_dir: None,
            dirty: false,
            last_save: Instant::now(),
            last_tip: String::new(),
            next_tip: Instant::now(),
            last_window_size: None,
        };

        app.sync_hotkeys();
        app.log(StatusKind::Info, "Ready");
        app
    }

    // ---------------------------------------------------------------------
    // Events
    // ---------------------------------------------------------------------

    fn poll_events(&mut self, ctx: &egui::Context) {
        for event in self.playback.poll_events() {
            match event {
                PlaybackEvent::Loaded {
                    id,
                    duration,
                    bytes,
                    verbose,
                } => {
                    if verbose {
                        let label = self.sound_name(id);
                        self.log(
                            StatusKind::Success,
                            format!(
                                "Loaded \"{label}\" into RAM ({}, {})",
                                format_bytes(bytes),
                                format_duration(duration)
                            ),
                        );
                    }
                }
                PlaybackEvent::Failed {
                    id,
                    message,
                    missing,
                } => {
                    let label = self.sound_name(id);
                    let text = if missing {
                        format!("File missing for \"{label}\": {message}")
                    } else {
                        format!("Could not load \"{label}\": {message}")
                    };
                    self.log(StatusKind::Error, text);
                }
            }
            ctx.request_repaint();
        }

        for event in self.engine.poll_events() {
            match event {
                AudioEvent::DeviceStarted { slot, .. } => {
                    self.device_ready[slot.index()] = true;
                    self.device_note[slot.index()] = None;
                    ctx.request_repaint();
                }
                AudioEvent::DeviceError {
                    slot,
                    name,
                    message,
                } => {
                    self.device_ready[slot.index()] = false;
                    self.device_note[slot.index()] = Some(message.clone());
                    self.log(
                        StatusKind::Error,
                        format!("{} device \"{name}\" failed: {message}", slot.label()),
                    );
                }
                AudioEvent::Warning(message) => {
                    self.log(StatusKind::Warning, message);
                }
                AudioEvent::Stopped { slot, .. } => {
                    self.device_ready[slot.index()] = false;
                }
            }
            ctx.request_repaint();
        }

        while let Some(event) = self.hotkeys.try_recv() {
            match event {
                ServiceEvent::Pressed(id) => {
                    #[cfg(debug_assertions)]
                    eprintln!("[hotkey] pressed sound {id}");
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                        sound.flash_until = Some(Instant::now() + Duration::from_millis(420));
                    }
                    ctx.request_repaint();
                }
                ServiceEvent::Registered {
                    sound_id,
                    hotkey,
                    error,
                } => {
                    let mut log_line = None;
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == sound_id) {
                        match &error {
                            Some(err) => {
                                sound.hotkey_error = Some(err.clone());
                                log_line = Some(format!(
                                    "Hotkey {} for \"{}\": {err}",
                                    hotkey.label(),
                                    sound.name
                                ));
                            }
                            None => {
                                sound.hotkey_error = None;
                            }
                        }
                    }
                    if let Some(line) = log_line {
                        self.log(StatusKind::Error, line);
                    }
                }
                ServiceEvent::Tray(command) => {
                    self.handle_tray(command, ctx);
                }
                ServiceEvent::TrayReady(ok) => {
                    self.tray_available = ok;
                    if !ok {
                        self.log(
                            StatusKind::Warning,
                            "System tray unavailable; close will quit the app.",
                        );
                    }
                }
                ServiceEvent::Fatal(message) => {
                    self.log(StatusKind::Error, format!("Background services: {message}"));
                }
            }
        }
    }

    fn handle_tray(&mut self, command: TrayCommand, ctx: &egui::Context) {
        match command {
            TrayCommand::Show => {
                self.hidden = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
                ctx.request_repaint();
            }
            TrayCommand::StopAll => {
                self.engine.stop_all();
                self.log(StatusKind::Info, "Stopped all sounds");
                ctx.request_repaint();
            }
            TrayCommand::Quit => {
                self.quit = true;
                self.save_now(ctx);
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let force_quit = self.quit_flag.swap(false, Ordering::SeqCst);
        if ctx.input(|i| i.viewport().close_requested()) {
            if force_quit || self.quit || !self.cfg.close_to_tray || !self.tray_available {
                self.quit = true;
                self.save_now(ctx);
            } else {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.hide_to_tray(ctx);
            }
        }
    }

    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        self.save_now(ctx);
        self.hidden = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.log(
            StatusKind::Info,
            "Minimized to tray - hotkeys still work",
        );
    }

    // ---------------------------------------------------------------------
    // Commands
    // ---------------------------------------------------------------------

    fn handle_actions(&mut self, actions: Vec<Action>, ctx: &egui::Context) {
        for action in actions {
            match action {
                Action::Play(id) => {
                    self.selected = Some(id);
                    if self.playback.info(id).missing {
                        self.locate_sound(id);
                    } else {
                        self.playback.play(id);
                    }
                }
                Action::Stop(id) => {
                    self.playback.stop_sound(id);
                }
                Action::Remove(id) => self.remove_sound(id),
                Action::Rename(id) => {
                    if let Some(sound) = self.sounds.iter().find(|s| s.id == id) {
                        self.renaming = Some(RenameState {
                            sound_id: id,
                            buffer: sound.name.clone(),
                            focus: true,
                        });
                    }
                }
                Action::SetHotkey(id) => {
                    if let Some(sound) = self.sounds.iter().find(|s| s.id == id) {
                        self.capture = Some(CaptureState {
                            sound_id: id,
                            current: sound.hotkey,
                        });
                    }
                }
                Action::ClearHotkey(id) => {
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                        sound.hotkey = None;
                        sound.hotkey_error = None;
                        sound.ram = false;
                    }
                    self.playback.set_retain(id, false);
                    self.sync_hotkeys();
                    self.mark_dirty();
                }
                Action::ToggleRam(id) => self.toggle_sound_ram(id),
                Action::Reload(id) => {
                    self.playback.reload(id, true);
                }
                Action::Locate(id) => self.locate_sound(id),
                Action::ShowInExplorer(id) => {
                    if let Some(sound) = self.sounds.iter().find(|s| s.id == id) {
                        let mut cmd = std::process::Command::new("explorer");
                        if sound.path.exists() {
                            cmd.arg(format!("/select,{}", sound.path.display()));
                        } else if let Some(parent) = sound.path.parent() {
                            cmd.arg(parent);
                        }
                        let _ = cmd.spawn();
                    }
                }
                Action::Volume(id, volume) => {
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                        sound.volume = volume.clamp(0.0, 1.0);
                        self.playback.set_volume(id, sound.volume);
                        self.mark_dirty();
                    }
                }
                Action::DragStart { id, index, size } => {
                    if self.search.trim().is_empty() {
                        if let Some(sound) = self.sounds.iter().find(|s| s.id == id) {
                            self.drag = Some(DragState {
                                sound_id: id,
                                from: index,
                                size,
                                name: sound.name.clone(),
                                hotkey: sound
                                    .hotkey
                                    .map(|h| h.label())
                                    .unwrap_or_default(),
                            });
                        }
                    }
                }
                Action::DragStop => {
                    if let Some(drag) = self.drag.take() {
                        let pointer = ctx.input(|i| i.pointer.interact_pos());
                        if let Some(pointer) = pointer {
                            let target = self
                                .card_rects
                                .iter()
                                .chain(self.row_rects.iter())
                                .find(|(_, rect)| rect.contains(pointer))
                                .map(|(index, _)| *index);
                            if let Some(target) = target {
                                if target != drag.from && target < self.sounds.len() {
                                    let sound = self.sounds.remove(drag.from);
                                    self.sounds.insert(target, sound);
                                    self.selected = Some(drag.sound_id);
                                    self.mark_dirty();
                                }
                            }
                        }
                    }
                }
            }
        }
        ctx.request_repaint();
    }

    fn play_sound(&mut self, id: u64) {
        self.selected = Some(id);
        self.playback.play(id);
    }

    fn remove_sound(&mut self, id: u64) {
        let Some(index) = self.sounds.iter().position(|s| s.id == id) else {
            return;
        };
        let sound = self.sounds.remove(index);
        self.playback.remove(id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        let label = sound.name.clone();
        self.undo = Some(UndoDelete {
            sound,
            index,
        });
        self.sync_hotkeys();
        self.mark_dirty();
        self.log(
            StatusKind::Warning,
            format!("Removed \"{label}\" - Ctrl+Z to undo"),
        );
    }

    fn undo_remove(&mut self) {
        let Some(undo) = self.undo.take() else {
            self.log(StatusKind::Info, "Nothing to undo");
            return;
        };
        let index = undo.index.min(self.sounds.len());
        let sound = undo.sound;
        let id = sound.id;
        let path = sound.path.clone();
        let volume = sound.volume;
        let ram = sound.ram && sound.hotkey.is_some();
        self.playback.register(id, path, volume);
        self.playback.mark_missing(
            id,
            !self.sounds.iter().any(|s| s.id == id && s.path.exists()),
            None,
        );
        let label = sound.name.clone();
        self.sounds.insert(index, sound);
        self.selected = Some(id);
        self.sync_hotkeys();
        self.mark_dirty();
        if ram {
            self.playback.set_retain(id, true);
            self.playback.ensure_loaded(id);
        }
        self.log(StatusKind::Success, format!("Restored \"{label}\""));
    }

    fn open_file_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Audio files", SUPPORTED_EXTENSIONS)
            .add_filter("All files", &["*"])
            .set_title("Add sounds to SoundDeck");
        if let Some(dir) = self.last_dir.clone() {
            dialog = dialog.set_directory(dir);
        } else if let Some(sound) = self.sounds.last() {
            if let Some(parent) = sound.path.parent() {
                dialog = dialog.set_directory(parent);
            }
        }
        if let Some(files) = dialog.pick_files() {
            if let Some(parent) = files.first().and_then(|p| p.parent()) {
                self.last_dir = Some(parent.to_path_buf());
            }
            self.add_files(files);
        }
    }

    fn locate_sound(&mut self, id: u64) {
        let Some(sound) = self.sounds.iter().find(|s| s.id == id) else {
            return;
        };
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Audio files", SUPPORTED_EXTENSIONS)
            .set_title("Locate sound file");
        if let Some(parent) = sound.path.parent() {
            if parent.exists() {
                dialog = dialog.set_directory(parent);
            }
        }
        if let Some(path) = dialog.pick_file() {
            self.relink_sound(id, path);
        }
    }

    fn relink_sound(&mut self, id: u64, path: PathBuf) {
        if !is_supported_file(&path) {
            self.log(
                StatusKind::Error,
                format!("Unsupported audio file: {}", path.display()),
            );
            return;
        }
        if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
            sound.path = path.clone();
        }
        self.playback.set_path(id, path.clone(), true);
        self.mark_dirty();
        self.log(
            StatusKind::Success,
            format!("\"{}\" linked to {}", self.sound_name(id), path.display()),
        );
    }

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let mut added = 0usize;
        let mut skipped = 0usize;
        let mut unsupported = 0usize;
        let mut last_id = None;

        let mut expanded: Vec<PathBuf> = Vec::new();
        for path in paths {
            if path.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&path) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_file() && is_supported_file(&p) {
                            expanded.push(p);
                        }
                    }
                }
            } else {
                expanded.push(path);
            }
        }

        for path in expanded {
            if !path.is_file() || !is_supported_file(&path) {
                unsupported += 1;
                continue;
            }
            let canonical = path
                .canonicalize()
                .unwrap_or_else(|_| path.clone())
                .to_string_lossy()
                .to_lowercase();
            let duplicate = self.sounds.iter().any(|s| {
                s.path
                    .canonicalize()
                    .unwrap_or_else(|_| s.path.clone())
                    .to_string_lossy()
                    .to_lowercase()
                    == canonical
            });
            if duplicate {
                skipped += 1;
                continue;
            }

            let id = self.cfg.next_id;
            self.cfg.next_id += 1;
            let volume = 1.0;
            let name = display_name(&path);
            let sound = Sound {
                id,
                name: name.clone(),
                path: path.clone(),
                hotkey: None,
                hotkey_error: None,
                volume,
                ram: false,
                flash_until: None,
            };
            self.playback.register(id, path, volume);
            self.sounds.push(sound);
            added += 1;
            last_id = Some(id);
        }

        if added > 0 {
            if let Some(id) = last_id {
                self.selected = Some(id);
            }
            self.sync_hotkeys();
            self.mark_dirty();
            self.log(
                StatusKind::Success,
                format!(
                    "Added {added} sound{}{}{}",
                    if added == 1 { "" } else { "s" },
                    if skipped > 0 {
                        format!(" - skipped {skipped} duplicate{}", if skipped == 1 { "" } else { "s" })
                    } else {
                        String::new()
                    },
                    if unsupported > 0 {
                        format!(" - ignored {unsupported} unsupported file{}", if unsupported == 1 { "" } else { "s" })
                    } else {
                        String::new()
                    }
                ),
            );
        } else if skipped > 0 {
            self.log(
                StatusKind::Warning,
                format!("Already in your library ({skipped} duplicate{})", if skipped == 1 { "" } else { "s" }),
            );
        } else if unsupported > 0 {
            self.log(
                StatusKind::Error,
                format!("No supported audio files found ({unsupported} ignored)"),
            );
        }
    }

    fn sync_hotkeys(&mut self) {
        let mut used: HashSet<String> = HashSet::new();
        let mut bindings = Vec::new();
        for sound in &mut self.sounds {
            let Some(hotkey) = sound.hotkey else {
                continue;
            };
            let label = hotkey.label();
            if used.contains(&label) {
                sound.hotkey_error = Some(format!("{label} is also used by another sound"));
                continue;
            }
            used.insert(label);
            bindings.push((sound.id, hotkey));
        }
        self.hotkeys.set_bindings(bindings);
    }

    fn ram_summary(&self) -> Option<RamState> {
        let mut eligible = 0usize;
        let mut on = 0usize;
        for sound in &self.sounds {
            if sound.hotkey.is_some() {
                eligible += 1;
                if sound.ram {
                    on += 1;
                }
            }
        }
        if eligible == 0 {
            None
        } else if on == 0 {
            Some(RamState::Off)
        } else if on == eligible {
            Some(RamState::On)
        } else {
            Some(RamState::Mixed)
        }
    }

    fn toggle_global_ram(&mut self) {
        let target = !matches!(self.ram_summary(), Some(RamState::On));
        self.set_all_hotkey_ram(target);
    }

    fn set_all_hotkey_ram(&mut self, on: bool) {
        let mut changed = false;
        for sound in &mut self.sounds {
            if sound.hotkey.is_none() {
                continue;
            }
            if sound.ram != on {
                sound.ram = on;
                changed = true;
            }
            self.playback.set_retain(sound.id, on);
            if on {
                self.playback.ensure_loaded(sound.id);
            }
        }
        if changed || self.cfg.preload_hotkey_sounds != on {
            self.cfg.preload_hotkey_sounds = on;
            self.mark_dirty();
        }
    }

    fn toggle_sound_ram(&mut self, id: u64) {
        let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if sound.hotkey.is_none() {
            return;
        }
        sound.ram = !sound.ram;
        let ram = sound.ram;
        self.playback.set_retain(id, ram);
        if ram {
            self.playback.ensure_loaded(id);
        }
        self.mark_dirty();
    }

    fn toggle_layout(&mut self) {
        self.cfg.layout = match self.cfg.layout {
            SoundLayout::Grid => SoundLayout::List,
            SoundLayout::List => SoundLayout::Grid,
        };
        self.mark_dirty();
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Mirror the in-memory library into the serialisable config.
    fn refresh_config(&mut self) {
        self.cfg.sounds = self
            .sounds
            .iter()
            .map(|s| SoundConfig {
                id: s.id,
                name: s.name.clone(),
                path: model::portable_path(&s.path),
                hotkey: s.hotkey.map(|h| h.label()),
                volume: s.volume,
                ram: if s.hotkey.is_some() { Some(s.ram) } else { None },
            })
            .collect();
        self.cfg.window_size = self.last_window_size.or(self.cfg.window_size);
        self.cfg.next_id = self
            .cfg
            .next_id
            .max(self.sounds.iter().map(|s| s.id).max().unwrap_or(0) + 1);
    }

    fn save_now(&mut self, ctx: &egui::Context) {
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            self.last_window_size = Some([rect.width(), rect.height()]);
        }
        self.refresh_config();
        if let Err(err) = save_config(&self.cfg_path, &self.cfg) {
            self.log(
                StatusKind::Warning,
                format!("Could not save config: {err}"),
            );
        } else {
            self.dirty = false;
            self.last_save = Instant::now();
        }
    }

    fn log(&mut self, kind: StatusKind, text: impl Into<String>) {
        let text = text.into();
        #[cfg(debug_assertions)]
        eprintln!("[log] {text}");
        self.statuses.push(StatusEntry {
            kind,
            text,
            at: Instant::now(),
        });
        if self.statuses.len() > 200 {
            self.statuses.remove(0);
        }
    }

    fn sound_name(&self, id: u64) -> String {
        self.sounds
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Unknown sound".to_string())
    }

    fn find_conflict(&self, hotkey: Hotkey, except_id: u64) -> Option<&Sound> {
        self.sounds
            .iter()
            .find(|s| s.id != except_id && s.hotkey == Some(hotkey))
    }

    // ---------------------------------------------------------------------
    // Devices
    // ---------------------------------------------------------------------

    fn refresh_devices(&mut self) {
        match wasapi::list_devices() {
            Ok(list) => {
                self.devices = list.devices;
                self.cfg.monitor_device =
                    resolve_device(&self.devices, self.cfg.monitor_device.as_deref(), self.cfg.monitor_name.as_deref())
                        .map(|d| d.id)
                        .or_else(|| self.cfg.monitor_device.clone());
                self.cfg.cable_device =
                    resolve_device(&self.devices, self.cfg.cable_device.as_deref(), self.cfg.cable_name.as_deref())
                        .map(|d| d.id)
                        .or_else(|| self.cfg.cable_device.clone());
                self.apply_routes();
                self.log(
                    StatusKind::Info,
                    format!("Found {} output device(s)", self.devices.len()),
                );
            }
            Err(err) => self.log(StatusKind::Error, format!("Device refresh failed: {err}")),
        }
    }

    fn set_device(&mut self, slot: Slot, id: Option<String>) {
        let name = id.as_ref().and_then(|id| {
            self.devices
                .iter()
                .find(|d| &d.id == id)
                .map(|d| d.name.clone())
        });
        match slot {
            Slot::Monitor => {
                self.cfg.monitor_name = name;
                self.cfg.monitor_device = id;
            }
            Slot::Cable => {
                self.cfg.cable_name = name;
                self.cfg.cable_device = id;
            }
        }
        self.apply_routes();
        self.mark_dirty();
    }

    fn slot_volume(&self, slot: Slot) -> f32 {
        match slot {
            Slot::Monitor => self.cfg.monitor_volume,
            Slot::Cable => self.cfg.cable_volume,
        }
    }

    /// Apply a slot volume, mirroring it to the linked slot when link mode is on.
    fn set_slot_volume(&mut self, slot: Slot, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        let other = match slot {
            Slot::Monitor => Slot::Cable,
            Slot::Cable => Slot::Monitor,
        };
        let targets = if self.cfg.link_volumes {
            [Some(slot), Some(other)]
        } else {
            [Some(slot), None]
        };
        for target in targets.into_iter().flatten() {
            match target {
                Slot::Monitor => self.cfg.monitor_volume = volume,
                Slot::Cable => self.cfg.cable_volume = volume,
            }
            self.engine.set_master(target, volume);
        }
        self.mark_dirty();
    }

    fn apply_routes(&mut self) {
        self.device_ready = [false, false];
        let monitor = self.cfg.monitor_device.as_ref().and_then(|id| {
            self.devices
                .iter()
                .find(|d| &d.id == id)
                .cloned()
                .or_else(|| Some(DeviceInfo {
                    id: id.clone(),
                    name: self.cfg.monitor_name.clone().unwrap_or_else(|| "Device".into()),
                }))
        });
        let cable = self.cfg.cable_device.as_ref().and_then(|id| {
            self.devices
                .iter()
                .find(|d| &d.id == id)
                .cloned()
                .or_else(|| Some(DeviceInfo {
                    id: id.clone(),
                    name: self.cfg.cable_name.clone().unwrap_or_else(|| "Cable".into()),
                }))
        });
        self.engine.set_routes(
            monitor.map(|d| DeviceRoute {
                id: d.id,
                name: d.name,
            }),
            cable.map(|d| DeviceRoute {
                id: d.id,
                name: d.name,
            }),
        );
        self.engine
            .set_master(Slot::Monitor, self.cfg.monitor_volume);
        self.engine.set_master(Slot::Cable, self.cfg.cable_volume);
    }

    // ---------------------------------------------------------------------
    // UI
    // ---------------------------------------------------------------------

    fn title_bar(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let bar_rect = ui.max_rect();
        let drag = ui.interact(bar_rect, Id::new("titlebar_drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }

        ui.horizontal_centered(|ui| {
            let (logo_rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            paint_logo(ui.painter(), logo_rect);
            ui.add_space(2.0);
            ui.label(
                RichText::new(APP_NAME)
                    .font(theme::medium(20.0))
                    .color(theme::text()),
            );

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if window_button(ui, WindowButton::Close, &self.cfg, self.tray_available) {
                    if self.cfg.close_to_tray && self.tray_available {
                        self.hide_to_tray(ctx);
                    } else {
                        self.quit = true;
                        self.save_now(ctx);
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                if window_button(ui, WindowButton::Maximize, &self.cfg, self.tray_available) {
                    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                if window_button(ui, WindowButton::Minimize, &self.cfg, self.tray_available) {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }
            });
            ui.add_space(6.0);
        });
    }

    fn side_panel(&mut self, ui: &mut Ui) {
        let content_height = (ui.available_height() - 84.0).max(120.0);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), content_height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().interact_size.y = 26.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let full = Vec2::new(ui.available_width(), 32.0);
                        if accent_button(ui, "Load sounds", full).clicked() {
                            self.open_file_dialog();
                        }
                        ui.add_space(4.0);
                        let layout_label = match self.cfg.layout {
                            SoundLayout::Grid => "Switch to list layout",
                            SoundLayout::List => "Switch to box layout",
                        };
                        if ghost_button(ui, layout_label, full).clicked() {
                            self.toggle_layout();
                        }

                        ui.add_space(6.0);
                        section_line(ui);
                        ui.add_space(4.0);

                        ui.label(RichText::new("Monitor").size(12.5).color(theme::muted()));
                        self.device_selector(ui, Slot::Monitor);
                        ui.add_space(2.0);
                        self.device_volume(ui, Slot::Monitor);

                        ui.add_space(2.0);
                        let link_rect = ui
                            .allocate_exact_size(
                                Vec2::new(ui.available_width(), 24.0),
                                Sense::hover(),
                            )
                            .0;
                        paint_link_row(ui, link_rect, self.cfg.link_volumes);
                        let link_resp = ui.interact(
                            Rect::from_center_size(link_rect.center(), Vec2::new(80.0, 24.0)),
                            Id::new("link_volumes"),
                            Sense::click(),
                        );
                        if link_resp.clicked() {
                            self.cfg.link_volumes = !self.cfg.link_volumes;
                            if self.cfg.link_volumes {
                                let volume = self.slot_volume(Slot::Monitor);
                                self.set_slot_volume(Slot::Cable, volume);
                            }
                            self.mark_dirty();
                        }
                        link_resp.on_hover_text("Link monitor and cable volumes");

                        ui.add_space(2.0);
                        ui.label(RichText::new("Cable").size(12.5).color(theme::muted()));
                        self.device_selector(ui, Slot::Cable);
                        ui.add_space(2.0);
                        self.device_volume(ui, Slot::Cable);

                        ui.add_space(4.0);
                        let mut gain_db = self.cfg.master_gain_db;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Gain").size(11.5).color(theme::muted()));
                            ui.spacing_mut().slider_width =
                                (ui.available_width() - 84.0).max(70.0);
                            let response = ui.add(
                                egui::Slider::new(&mut gain_db, -60.0..=12.0)
                                    .step_by(0.5)
                                    .show_value(false)
                                    .trailing_fill(true),
                            );
                            let mut reset = response.double_clicked();
                            let changed = response.changed();
                            response.on_hover_text(
                                "Master gain - both outputs. Double-click to reset.",
                            );
                            ui.label(
                                RichText::new(format_gain_db(gain_db))
                                    .size(10.5)
                                    .color(theme::muted())
                                    .monospace(),
                            );
                            reset |= reset_button(ui, gain_db.abs() > 0.05);
                            if reset {
                                gain_db = 0.0;
                            }
                            if changed || reset {
                                self.cfg.master_gain_db = gain_db;
                                self.engine.set_master_gain_db(gain_db);
                                self.mark_dirty();
                            }
                        });

                        ui.add_space(6.0);
                        section_line(ui);
                        ui.add_space(6.0);
                        let ram_state = self.ram_summary();
                        let enabled = ram_state.is_some();
                        let mixed = matches!(ram_state, Some(RamState::Mixed));
                        let on = matches!(ram_state, Some(RamState::On));
                        let row_height = 28.0;
                        let row_rect = Rect::from_min_size(
                            ui.cursor().min,
                            Vec2::new(ui.available_width(), row_height),
                        );
                        let painter = ui.painter().clone();
                        painter.text(
                            row_rect.left_center(),
                            Align2::LEFT_CENTER,
                            "Load hotkey sounds into RAM",
                            theme::ui_font(12.0),
                            if enabled { theme::text() } else { theme::faint() },
                        );
                        let switch_rect = Rect::from_center_size(
                            Pos2::new(row_rect.right() - 20.0, row_rect.center().y),
                            Vec2::new(40.0, 22.0),
                        );
                        paint_switch(&painter, switch_rect, on, mixed, enabled);
                        let refresh_rect = Rect::from_center_size(
                            Pos2::new(row_rect.right() - 68.0, row_rect.center().y),
                            Vec2::splat(24.0),
                        );
                        let response = ui.interact(
                            row_rect,
                            Id::new("ram_mode_toggle"),
                            if enabled { Sense::click() } else { Sense::hover() },
                        );
                        let refresh = ui.interact(
                            refresh_rect,
                            Id::new("refresh_devices_inline"),
                            Sense::click(),
                        );
                        if refresh.hovered() {
                            painter.rect_filled(
                                refresh_rect,
                                CornerRadius::same(5),
                                theme::alpha(theme::accent(), 16),
                            );
                        }
                        paint_rotate_icon(
                            &painter,
                            refresh_rect,
                            if refresh.hovered() {
                                theme::accent()
                            } else {
                                theme::muted()
                            },
                            true,
                        );
                        if refresh.clicked() {
                            self.refresh_devices();
                        }
                        let refresh_hovered = refresh.hovered();
                        refresh.on_hover_text("Refresh audio devices");
                        if response.clicked() && enabled && !refresh_hovered {
                            self.set_all_hotkey_ram(!on);
                        }
                        response.on_hover_text(if enabled {
                            if mixed {
                                "Some hotkey sounds are in RAM. Click to load all."
                            } else if on {
                                "Hotkey sounds are playing from RAM. Click to play from disk."
                            } else {
                                "Hotkey sounds are playing from disk. Click to load all into RAM."
                            }
                        } else {
                            "Assign a hotkey to a sound first"
                        });
                        ui.allocate_space(Vec2::new(0.0, row_height));
                        ui.add_space(6.0);
                    });
            },
        );

        // Developer + Settings pinned to the bottom of the sidebar.
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 84.0),
            Layout::bottom_up(Align::Min),
            |ui| {
                let full = Vec2::new(ui.available_width(), 32.0);
                if ghost_button(ui, "Settings", full)
                    .on_hover_text("Theme, tray behaviour, about (Ctrl+,)")
                    .clicked()
                {
                    self.show_settings = true;
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(DEVELOPER_LINK)
                        .size(10.0)
                        .color(theme::faint()),
                );
                ui.label(
                    RichText::new(format!("Developed by {DEVELOPER_NAME}"))
                        .size(11.0)
                        .color(theme::muted()),
                );
                ui.add_space(2.0);
            },
        );
    }

    fn device_selector(&mut self, ui: &mut Ui, slot: Slot) {
        let (current_id, name) = match slot {
            Slot::Monitor => (
                self.cfg.monitor_device.clone(),
                self.cfg.monitor_name.clone(),
            ),
            Slot::Cable => (self.cfg.cable_device.clone(), self.cfg.cable_name.clone()),
        };
        let fallback = "Select device".to_string();
        let selected_text = name.unwrap_or(fallback);
        let mut chosen: Option<Option<String>> = None;

        let width = ui.available_width();
        ComboBox::from_id_salt(("device", slot.index()))
            .width(width)
            .selected_text(RichText::new(selected_text).size(13.5))
            .show_ui(ui, |ui| {
                ui.set_min_width(width - 20.0);
                if ui
                    .selectable_label(current_id.is_none(), "None (silent)")
                    .clicked()
                {
                    chosen = Some(None);
                    ui.close_menu();
                }
                ui.separator();
                for device in &self.devices {
                    let active = current_id.as_deref() == Some(device.id.as_str());
                    let label = format!("{}", device.name);
                    if ui.selectable_label(active, label).clicked() {
                        chosen = Some(Some(device.id.clone()));
                        ui.close_menu();
                    }
                }
                if self.devices.is_empty() {
                    ui.label(RichText::new("No output devices").color(theme::faint()));
                }
            });

        if let Some(choice) = chosen {
            self.set_device(slot, choice);
        }
    }

    fn device_volume(&mut self, ui: &mut Ui, slot: Slot) {
        let mut volume = self.slot_volume(slot);
        ui.horizontal(|ui| {
            let (icon_rect, _) = ui.allocate_exact_size(Vec2::new(14.0, 14.0), Sense::hover());
            theme::speaker_icon(ui.painter(), icon_rect, theme::muted());
            ui.spacing_mut().slider_width = (ui.available_width() - 44.0).max(60.0);
            let changed = ui
                .add(
                    egui::Slider::new(&mut volume, 0.0..=1.0)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .changed();
            ui.label(
                RichText::new(format!("{:>3.0}%", volume * 100.0))
                    .size(11.0)
                    .color(theme::muted())
                    .monospace(),
            );
            if changed {
                self.set_slot_volume(slot, volume);
            }
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let latest = self.statuses.last();
            let (kind, text) = match latest {
                Some(entry) => (entry.kind, entry.text.clone()),
                None => (
                    StatusKind::Info,
                    "Ready".to_string(),
                ),
            };
            status_dot(ui, kind.color());
            ui.add(
                egui::Label::new(RichText::new(text).size(12.5).color(kind.color()))
                    .truncate(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if tiny_button(ui, "Activity log").clicked() {
                    self.show_log = true;
                }
                if ui
                    .add(
                        Button::new(RichText::new("Stop all").size(11.5).color(theme::red()))
                            .frame(false),
                    )
                    .on_hover_text("Stop every playing sound (Esc)")
                    .clicked()
                {
                    self.engine.stop_all();
                    self.log(StatusKind::Info, "Stopped all sounds");
                }
            });
        });
        let _ = ctx;
    }

    fn main_page(&mut self, ui: &mut Ui) {
        self.toolbar(ui);
        ui.add_space(6.0);

        let filtered = self.filtered_indices();
        let active = self.engine.active();
        let mut progress: HashMap<u64, f32> = HashMap::new();
        for voice in &active {
            progress
                .entry(voice.sound_id)
                .and_modify(|p| *p = p.max(voice.progress))
                .or_insert(voice.progress);
        }

        // Only query playback state for sounds that are actually on screen.
        let infos: HashMap<u64, SoundInfo> = filtered
            .iter()
            .map(|&index| {
                let sound = &self.sounds[index];
                (sound.id, self.playback.info(sound.id))
            })
            .collect();

        let mut actions = Vec::new();
        let mut rects: Vec<(usize, Rect)> = Vec::new();
        let mut rows: Vec<(usize, Rect)> = Vec::new();
        let mut volume_rects: Vec<(u64, Rect)> = Vec::new();
        let allow_drag = self.search.trim().is_empty();
        let capture_id = self.capture.as_ref().map(|c| c.sound_id);

        // Wheel over a volume bar changes that sound's volume instead of
        // scrolling the library. The bar rects come from the previous frame.
        if let Some(pointer) = ui.ctx().pointer_hover_pos() {
            let raw_scroll = ui.ctx().input(|i| i.raw_scroll_delta.y);
            if let Some((id, _)) = self
                .volume_rects
                .iter()
                .find(|(_, rect)| rect.contains(pointer))
                .copied()
            {
                if raw_scroll.abs() > 0.01 {
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                        sound.volume = (sound.volume + 0.05 * raw_scroll.signum()).clamp(0.0, 1.0);
                        let volume = sound.volume;
                        self.playback.set_volume(id, volume);
                        self.mark_dirty();
                    }
                    ui.ctx().input_mut(|i| {
                        i.raw_scroll_delta = Vec2::ZERO;
                        i.smooth_scroll_delta = Vec2::ZERO;
                    });
                }
            }
        }

        if self.sounds.is_empty() {
            self.empty_state(ui);
        } else if filtered.is_empty() {
            self.no_results(ui);
        } else {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    match self.cfg.layout {
                        SoundLayout::Grid => draw_grid(
                            ui,
                            &self.sounds,
                            &filtered,
                            &infos,
                            &progress,
                            allow_drag,
                            capture_id,
                            self.selected,
                            &mut rects,
                            &mut volume_rects,
                            &mut actions,
                        ),
                        SoundLayout::List => draw_list(
                            ui,
                            &self.sounds,
                            &filtered,
                            &infos,
                            &progress,
                            allow_drag,
                            capture_id,
                            self.selected,
                            &mut rows,
                            &mut volume_rects,
                            &mut actions,
                        ),
                    }
                    ui.add_space(10.0);
                });
        }

        self.card_rects = rects;
        self.row_rects = rows;
        self.volume_rects = volume_rects;
        self.handle_actions(actions, ui.ctx());

        // Drop files overlay is drawn in update(); this only consumes the drop.
        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_files(dropped);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 34.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                let pill_width = 164.0;
                let search_width = (ui.available_width() - pill_width - 10.0).max(160.0);
                let response = ui.add_sized(
                    [search_width, 32.0],
                    TextEdit::singleline(&mut self.search)
                        .hint_text("Search sounds...")
                        .text_color(theme::text())
                        .vertical_align(Align::Center),
                );
                if self.focus_search {
                    response.request_focus();
                    self.focus_search = false;
                }

                ui.add_space(10.0);
                let state = self.ram_summary();
                let (label, color, enabled) = match state {
                    None => ("No hotkeys".to_string(), theme::faint(), false),
                    Some(RamState::On) => {
                        ("Hotkey sounds: RAM".to_string(), theme::accent(), true)
                    }
                    Some(RamState::Off) => {
                        ("Hotkey sounds: DISK".to_string(), theme::muted(), true)
                    }
                    Some(RamState::Mixed) => {
                        ("Hotkey sounds: MIXED".to_string(), theme::purple(), true)
                    }
                };
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(pill_width, 28.0),
                    Sense::hover(),
                );
                let response = ui.interact(
                    rect,
                    Id::new("ram_status_pill"),
                    if enabled { Sense::click() } else { Sense::hover() },
                );
                let painter = ui.painter();
                painter.rect(
                    rect,
                    CornerRadius::same(14),
                    theme::alpha(color, 16),
                    Stroke::new(1.0_f32, theme::alpha(color, 70)),
                    StrokeKind::Inside,
                );
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    theme::ui_font(11.5),
                    color,
                );
                if enabled && response.clicked() {
                    self.toggle_global_ram();
                }
                response.on_hover_text(match state {
                    None => "Assign a hotkey to a sound first",
                    Some(RamState::On) => "Click to play hotkey sounds from disk",
                    Some(RamState::Off) => "Click to load hotkey sounds into RAM",
                    Some(RamState::Mixed) => "Mixed. Click to load all hotkey sounds into RAM",
                });
            },
        );
    }

    fn filtered_indices(&self) -> Vec<usize> {
        let needle = self.search.trim().to_lowercase();
        self.sounds
            .iter()
            .enumerate()
            .filter(|(_, sound)| {
                needle.is_empty()
                    || sound.name.to_lowercase().contains(&needle)
                    || sound
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase().contains(&needle))
                        .unwrap_or(false)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let available = ui.available_size();
        ui.allocate_ui_with_layout(available, Layout::top_down(Align::Center), |ui| {
            let width = (available.x - 24.0).min(680.0);
            let height = (available.y * 0.5).clamp(180.0, 300.0);
            ui.add_space(((available.y - height) * 0.5).max(0.0));
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
            let painter = ui.painter();
            painter.rect_filled(
                rect,
                CornerRadius::same(14),
                theme::alpha(theme::card(), 55),
            );
            painter.rect_stroke(
                rect,
                CornerRadius::same(14),
                Stroke::new(1.0_f32,
                    if response.hovered() {
                        theme::alpha(theme::accent(), 160)
                    } else {
                        theme::border_soft()
                    },
                ),
                StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Drag sounds in to add them here",
                theme::medium(30.0),
                theme::text(),
            );
            if response.clicked() {
                self.open_file_dialog();
            }
            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        });
    }

    fn no_results(&mut self, ui: &mut Ui) {
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(format!("No sounds match \"{}\"", self.search.trim()))
                    .size(20.0)
                    .color(theme::muted()),
            );
            ui.add_space(4.0);
            if ghost_button(ui, "Clear search", Vec2::new(130.0, 30.0)).clicked() {
                self.search.clear();
            }
        });
    }

    fn modals(&mut self, ctx: &egui::Context) {
        self.settings_modal(ctx);
        self.capture_modal(ctx);
        self.rename_modal(ctx);
        self.log_modal(ctx);
    }

    fn settings_modal(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let screen = ctx.screen_rect();
        let mut close = false;

        // Dim backdrop that also swallows clicks outside the settings card.
        let overlay = egui::Area::new(Id::new("settings_backdrop"))
            .fixed_pos(screen.min)
            .order(Order::Middle)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::ZERO,
                    theme::alpha(theme::bg(), 210),
                );
                response
            });
        if overlay.inner.clicked() {
            close = true;
        }

        egui::Window::new("settings_window")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .order(Order::Foreground)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(
                Frame::NONE
                    .fill(theme::panel())
                    .stroke(Stroke::new(1.0_f32, theme::border()))
                    .corner_radius(14)
                    .inner_margin(Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_width(600.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Settings")
                            .font(theme::medium(24.0))
                            .color(theme::text()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                CornerRadius::same(6),
                                theme::alpha(theme::red(), 26),
                            );
                        }
                        let color = if response.hovered() {
                            theme::red()
                        } else {
                            theme::muted()
                        };
                        let c = rect.center();
                        let stroke = Stroke::new(1.4_f32, color);
                        ui.painter().line_segment(
                            [c + Vec2::new(-4.5, -4.5), c + Vec2::new(4.5, 4.5)],
                            stroke,
                        );
                        ui.painter().line_segment(
                            [c + Vec2::new(4.5, -4.5), c + Vec2::new(-4.5, 4.5)],
                            stroke,
                        );
                        if response.clicked() {
                            close = true;
                        }
                        response.on_hover_text("Close settings");
                    });
                });
                ui.add_space(10.0);
                section_line(ui);
                ui.add_space(8.0);

                let max_height = (screen.height() - 190.0).max(260.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(max_height)
                    .show(ui, |ui| {
                        section_label(ui, "APPEARANCE");
                        ui.add_space(4.0);
                        let mut selected_theme = self.cfg.theme;
                        ComboBox::from_id_salt("settings_theme")
                            .width(ui.available_width())
                            .selected_text(selected_theme.label())
                            .show_ui(ui, |ui| {
                                for option in AppTheme::ALL {
                                    if ui
                                        .selectable_label(
                                            selected_theme == option,
                                            option.label(),
                                        )
                                        .clicked()
                                    {
                                        selected_theme = option;
                                        ui.close_menu();
                                    }
                                }
                            });
                        if selected_theme != self.cfg.theme {
                            self.cfg.theme = selected_theme;
                            theme::set_theme(selected_theme);
                            theme::apply_style(ctx);
                            self.mark_dirty();
                            ctx.request_repaint();
                        }

                        ui.add_space(14.0);
                        section_line(ui);
                        ui.add_space(8.0);
                        section_label(ui, "BEHAVIOUR");
                        let mut close_to_tray = self.cfg.close_to_tray;
                        if ui
                            .checkbox(&mut close_to_tray, "Close button hides to tray")
                            .on_hover_text("When off, the close button quits SoundDeck.")
                            .changed()
                        {
                            self.cfg.close_to_tray = close_to_tray;
                            self.mark_dirty();
                        }

                        ui.add_space(14.0);
                        section_line(ui);
                        ui.add_space(8.0);
                        section_label(ui, "ABOUT THE DEVELOPER");
                        ui.label(
                            RichText::new(DEVELOPER_NAME)
                                .font(theme::medium(16.0))
                                .color(theme::text()),
                        );
                        ui.label(
                            RichText::new(DEVELOPER_ROLE)
                                .size(11.5)
                                .color(theme::muted()),
                        );
                        ui.label(
                            RichText::new(DEVELOPER_BLURB)
                                .size(11.5)
                                .color(theme::faint()),
                        );
                        ui.label(
                            RichText::new(DEVELOPER_LINK)
                                .size(11.0)
                                .color(theme::accent()),
                        );

                        ui.add_space(14.0);
                        section_line(ui);
                        ui.add_space(8.0);
                        section_label(ui, "ABOUT");
                        ui.label(
                            RichText::new(format!("{APP_NAME} v0.1.0"))
                                .size(12.5)
                                .color(theme::text()),
                        );
                        ui.add_space(4.0);
                        if ghost_button(ui, "Open config folder", Vec2::new(170.0, 28.0))
                            .clicked()
                        {
                            let _ = std::process::Command::new("explorer")
                                .arg(model::app_dir())
                                .spawn();
                        }
                    });
            });

        if close {
            self.show_settings = false;
        }
    }

    fn capture_modal(&mut self, ctx: &egui::Context) {
        let Some(mut capture) = self.capture.take() else {
            return;
        };
        let mut close = false;
        let mut save = false;
        let mut clear = false;

        // Read the keyboard directly so the global hotkey wins over widgets.
        let events = ctx.input(|i| i.events.clone());
        for event in events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                if key == Key::Escape {
                    close = true;
                    continue;
                }
                let plain = !(modifiers.ctrl || modifiers.alt || modifiers.shift);
                if (key == Key::Backspace || key == Key::Delete) && plain {
                    capture.current = None;
                    continue;
                }
                if let Some(hotkey) = Hotkey::from_egui(key, &modifiers) {
                    capture.current = Some(hotkey);
                }
            }
        }

        let sound_name = self.sound_name(capture.sound_id);
        let conflict = capture
            .current
            .and_then(|hk| self.find_conflict(hk, capture.sound_id).map(|s| s.name.clone()));

        egui::Window::new("Set hotkey")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(
                Frame::NONE
                    .fill(theme::panel())
                    .stroke(Stroke::new(1.0_f32, theme::border()))
                    .corner_radius(14)
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Set hotkey")
                            .font(theme::medium(20.0))
                            .color(theme::text()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                Button::new(RichText::new("x").size(15.0).color(theme::muted()))
                                    .frame(false),
                            )
                            .on_hover_text("Close")
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.label(
                    RichText::new(format!("\"{sound_name}\""))
                        .size(13.0)
                        .color(theme::muted()),
                );
                ui.add_space(12.0);

                let box_rect = Rect::from_min_size(
                    ui.cursor().min,
                    Vec2::new(ui.available_width(), 96.0),
                );
                let painter = ui.painter();
                painter.rect_filled(box_rect, CornerRadius::same(10), theme::field());
                painter.rect_stroke(
                    box_rect,
                    CornerRadius::same(10),
                    Stroke::new(1.0_f32, theme::border()),
                    StrokeKind::Inside,
                );
                let label = capture
                    .current
                    .map(|h| h.label())
                    .unwrap_or_else(|| "Press a key combination...".to_string());
                let color = if capture.current.is_some() {
                    theme::accent()
                } else {
                    theme::faint()
                };
                painter.text(
                    box_rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    theme::ui_font(26.0),
                    color,
                );
                ui.allocate_space(box_rect.size());

                ui.add_space(6.0);
                if let Some(conflict) = &conflict {
                    ui.label(
                        RichText::new(format!("Already assigned to \"{conflict}\""))
                            .size(12.5)
                            .color(theme::red()),
                    );
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let can_save = conflict.is_none();
                    if accent_button_enabled(ui, "Save", Vec2::new(100.0, 32.0), can_save).clicked()
                        && can_save
                    {
                        save = true;
                    }
                    if ghost_button(ui, "Cancel", Vec2::new(100.0, 32.0))
                        .on_hover_text("Esc")
                        .clicked()
                    {
                        close = true;
                    }
                    if ghost_button(ui, "Clear", Vec2::new(84.0, 32.0)).clicked() {
                        clear = true;
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Esc cancels")
                        .size(10.5)
                        .color(theme::faint()),
                );
            });

        if close {
            return;
        }
        if save {
            let mut log_line = None;
            let mut ram_change = None;
            if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == capture.sound_id) {
                let had_hotkey = sound.hotkey.is_some();
                sound.hotkey = capture.current;
                sound.hotkey_error = None;
                if capture.current.is_some() {
                    if !had_hotkey {
                        sound.ram = self.cfg.preload_hotkey_sounds;
                    }
                } else {
                    sound.ram = false;
                }
                ram_change = Some((sound.id, sound.ram));
                let label = sound
                    .hotkey
                    .map(|h| h.label())
                    .unwrap_or_else(|| "none".into());
                let name = sound.name.clone();
                log_line = Some(if capture.current.is_some() {
                    format!("Hotkey {label} assigned to \"{name}\"")
                } else {
                    format!("Hotkey cleared for \"{name}\"")
                });
            }
            if let Some(line) = log_line {
                self.log(StatusKind::Info, line);
            }
            if let Some((id, ram)) = ram_change {
                self.playback.set_retain(id, ram);
                if ram {
                    self.playback.ensure_loaded(id);
                }
            }
            self.sync_hotkeys();
            self.mark_dirty();
            return;
        }
        if clear {
            if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == capture.sound_id) {
                sound.hotkey = None;
                sound.hotkey_error = None;
                sound.ram = false;
            }
            self.playback.set_retain(capture.sound_id, false);
            self.sync_hotkeys();
            self.mark_dirty();
            return;
        }
        self.capture = Some(capture);
    }

    fn rename_modal(&mut self, ctx: &egui::Context) {
        let Some(mut renaming) = self.renaming.take() else {
            return;
        };
        let mut close = false;
        let mut commit = false;

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            close = true;
        }

        egui::Window::new("Rename sound")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .frame(
                Frame::NONE
                    .fill(theme::panel())
                    .stroke(Stroke::new(1.0_f32, theme::border()))
                    .corner_radius(14)
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("Rename sound").font(theme::medium(20.0)).color(theme::text()));
                ui.add_space(8.0);
                let response = ui.add_sized(
                    [ui.available_width(), 34.0],
                    TextEdit::singleline(&mut renaming.buffer),
                );
                if renaming.focus {
                    response.request_focus();
                    renaming.focus = false;
                }
                if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    commit = true;
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if accent_button(ui, "Save", Vec2::new(90.0, 32.0)).clicked() {
                        commit = true;
                    }
                    if ghost_button(ui, "Cancel", Vec2::new(90.0, 32.0)).clicked() {
                        close = true;
                    }
                });
            });

        if commit {
            let name = renaming.buffer.trim().to_string();
            if name.is_empty() {
                self.log(StatusKind::Warning, "Name cannot be empty");
                self.renaming = Some(renaming);
                return;
            }
            let mut old_name = None;
            if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == renaming.sound_id) {
                old_name = Some(std::mem::replace(&mut sound.name, name));
            }
            if let Some(old) = old_name {
                self.log(StatusKind::Info, format!("Renamed \"{old}\""));
            }
            self.mark_dirty();
            return;
        }
        if !close {
            self.renaming = Some(renaming);
        }
    }

    fn log_modal(&mut self, ctx: &egui::Context) {
        if !self.show_log {
            return;
        }
        let entries: Vec<(StatusKind, String, String)> = self
            .statuses
            .iter()
            .rev()
            .map(|entry| {
                let secs = entry.at.elapsed().as_secs();
                let when = if secs < 60 {
                    format!("{secs}s ago")
                } else {
                    format!("{}m ago", secs / 60)
                };
                (entry.kind, when, entry.text.clone())
            })
            .collect();
        let mut open = true;
        egui::Window::new("Activity log")
            .open(&mut open)
            .default_size([560.0, 340.0])
            .min_width(460.0)
            .resizable(true)
            .frame(
                Frame::NONE
                    .fill(theme::panel())
                    .stroke(Stroke::new(1.0_f32, theme::border()))
                    .corner_radius(12)
                    .inner_margin(Margin::same(12)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if entries.is_empty() {
                            ui.label(RichText::new("No activity yet.").color(theme::faint()));
                        }
                        for (kind, when, text) in entries.iter() {
                            ui.horizontal_top(|ui| {
                                status_dot(ui, kind.color());
                                ui.label(
                                    RichText::new(text)
                                        .size(12.5)
                                        .color(if *kind == StatusKind::Error {
                                            theme::red()
                                        } else {
                                            theme::text()
                                        }),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(when.clone()).size(10.0).color(theme::faint()),
                                    );
                                });
                            });
                            ui.add_space(2.0);
                        }
                    });
            });
        if !open {
            self.show_log = false;
        }
    }

    fn drag_ghost(&mut self, ctx: &egui::Context) {
        let Some(drag) = &self.drag else {
            return;
        };
        let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        if !ctx.input(|i| i.pointer.primary_down()) {
            return;
        }
        let size = Vec2::new(drag.size.x.min(260.0), 64.0);
        let rect = Rect::from_min_size(pointer + Vec2::new(12.0, 12.0), size);
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("drag_ghost")));
        painter.rect(
            rect,
            CornerRadius::same(10),
            theme::alpha(theme::card_hover(), 235),
            Stroke::new(1.0_f32, theme::accent()),
            StrokeKind::Inside,
        );
        painter.text(
            rect.left_top() + Vec2::new(12.0, 22.0),
            Align2::LEFT_CENTER,
            &drag.name,
            theme::ui_font(17.0),
            theme::text(),
        );
        if !drag.hotkey.is_empty() {
            painter.text(
                rect.left_bottom() + Vec2::new(12.0, -16.0),
                Align2::LEFT_CENTER,
                &drag.hotkey,
                theme::ui_font(12.0),
                theme::muted(),
            );
        }
    }

    fn draw_drop_overlay(&mut self, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !hovering || self.drag.is_some() {
            return;
        }
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("drop_overlay")));
        painter.rect_filled(screen, CornerRadius::ZERO, theme::alpha(theme::bg(), 220));
        painter.rect_stroke(
            screen.shrink(18.0),
            CornerRadius::same(16),
            Stroke::new(2.0_f32, theme::accent()),
            StrokeKind::Inside,
        );
        painter.text(
            screen.center() - Vec2::new(0.0, 10.0),
            Align2::CENTER_CENTER,
            "Drop to add sounds",
            theme::ui_font(34.0),
            theme::text(),
        );
        painter.text(
            screen.center() + Vec2::new(0.0, 28.0),
            Align2::CENTER_CENTER,
            "WAV / MP3 / OGG / FLAC / M4A / AAC",
            theme::ui_font(15.0),
            theme::muted(),
        );
    }

    fn handle_resize(&mut self, ctx: &egui::Context) {
        use eframe::egui::viewport::ResizeDirection;
        use eframe::egui::CursorIcon;

        let Some(pos) = ctx.pointer_hover_pos() else {
            return;
        };
        let screen = ctx.screen_rect();
        let margin = 6.0;
        let left = pos.x <= screen.left() + margin;
        let right = pos.x >= screen.right() - margin;
        let top = pos.y <= screen.top() + margin;
        let bottom = pos.y >= screen.bottom() - margin;
        if !(left || right || top || bottom) {
            return;
        }

        let (direction, cursor) = match (left, right, top, bottom) {
            (true, _, true, _) => (ResizeDirection::NorthWest, CursorIcon::ResizeNwSe),
            (_, true, true, _) => (ResizeDirection::NorthEast, CursorIcon::ResizeNeSw),
            (true, _, _, true) => (ResizeDirection::SouthWest, CursorIcon::ResizeNeSw),
            (_, true, _, true) => (ResizeDirection::SouthEast, CursorIcon::ResizeNwSe),
            (true, _, _, _) => (ResizeDirection::West, CursorIcon::ResizeHorizontal),
            (_, true, _, _) => (ResizeDirection::East, CursorIcon::ResizeHorizontal),
            (_, _, true, _) => (ResizeDirection::North, CursorIcon::ResizeVertical),
            (_, _, _, true) => (ResizeDirection::South, CursorIcon::ResizeVertical),
            _ => return,
        };
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
        }
    }

    fn global_shortcuts(&mut self, ctx: &egui::Context) {
        if self.capture.is_some() || self.renaming.is_some() {
            return;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Comma)) {
            self.show_settings = true;
            return;
        }

        // Esc is a universal "back": close the current overlay, cancel a drag,
        // clear a search, and only then stop all playback.
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.show_settings {
                self.show_settings = false;
            } else if self.show_log {
                self.show_log = false;
            } else if self.drag.is_some() {
                self.drag = None;
            } else if !self.search.is_empty() {
                self.search.clear();
            } else {
                self.engine.stop_all();
                self.log(StatusKind::Info, "Stopped all sounds");
            }
            ctx.request_repaint();
            return;
        }

        if self.show_settings {
            return;
        }

        let typing = ctx.memory(|m| m.focused().is_some());
        let (open, find, toggle_layout, undo, quit, delete, space, f2) = ctx.input(|i| {
            (
                i.modifiers.ctrl && i.key_pressed(Key::O),
                i.modifiers.ctrl && i.key_pressed(Key::F),
                i.modifiers.ctrl && i.key_pressed(Key::L),
                i.modifiers.ctrl && i.key_pressed(Key::Z),
                i.modifiers.ctrl && i.key_pressed(Key::Q),
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::Space),
                i.key_pressed(Key::F2),
            )
        });

        if open {
            self.open_file_dialog();
        }
        if find {
            self.focus_search = true;
        }
        if toggle_layout {
            self.toggle_layout();
        }
        if undo {
            self.undo_remove();
        }
        if quit {
            self.quit = true;
            self.save_now(ctx);
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if typing {
            return;
        }
        if delete {
            if let Some(id) = self.selected {
                self.remove_sound(id);
            }
        }
        if space {
            if let Some(id) = self.selected {
                self.play_sound(id);
            }
        }
        if f2 {
            if let Some(id) = self.selected {
                if let Some(sound) = self.sounds.iter().find(|s| s.id == id) {
                    self.renaming = Some(RenameState {
                        sound_id: id,
                        buffer: sound.name.clone(),
                        focus: true,
                    });
                }
            }
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        if self.dirty && self.last_save.elapsed() > Duration::from_millis(700) {
            self.save_now(ctx);
        }
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            self.last_window_size = Some([rect.width(), rect.height()]);
        }

        let now = Instant::now();
        if now >= self.next_tip {
            self.next_tip = now + Duration::from_secs(3);
            let loaded = self.playback.loaded_count();
            let playing = self
                .engine
                .active()
                .iter()
                .map(|v| v.sound_id)
                .collect::<HashSet<_>>()
                .len();
            let tip = if playing > 0 {
                format!("{APP_NAME} - {playing} playing - {loaded} in RAM")
            } else {
                format!("{APP_NAME} - {} sounds - {loaded} in RAM", self.sounds.len())
            };
            if tip != self.last_tip {
                self.hotkeys.set_tooltip(tip.clone());
                self.last_tip = tip;
            }
        }

        // Keep hotkeys responsive while the window is minimized or hidden.
        ctx.request_repaint_after(Duration::from_millis(120));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events(ctx);
        self.handle_close_request(ctx);
        if self.quit {
            return;
        }
        self.global_shortcuts(ctx);
        self.handle_resize(ctx);
        self.tick(ctx);

        let screen = ctx.screen_rect();
        let titlebar_rect = Rect::from_min_size(screen.min, Vec2::new(screen.width(), 38.0));
        let status_rect = Rect::from_min_max(
            Pos2::new(screen.left(), screen.bottom() - 34.0),
            screen.right_bottom(),
        );

        egui::TopBottomPanel::top("titlebar")
            .exact_height(38.0)
            .frame(
                Frame::NONE
                    .fill(theme::panel_deep())
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ctx, |ui| self.title_bar(ctx, ui));

        egui::TopBottomPanel::bottom("statusbar")
            .exact_height(34.0)
            .frame(
                Frame::NONE
                    .fill(theme::panel_deep())
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ctx, |ui| self.status_bar(ctx, ui));

        let side = egui::SidePanel::right("sidebar")
            .resizable(false)
            .exact_width(SIDEBAR_WIDTH)
            .frame(
                Frame::NONE
                    .fill(theme::panel())
                    .inner_margin(Margin::symmetric(14, 12)),
            )
            .show(ctx, |ui| self.side_panel(ui));

        // Separator between the library and the sidebar. Clamp it to the area
        // between the title bar and the status bar so it never bleeds into the
        // status strip (which is drawn on top of the sidebar's layer).
        let divider = egui::Rangef::new(
            titlebar_rect.bottom() + 1.0,
            status_rect.top(),
        );
        ctx.layer_painter(LayerId::new(Order::Middle, Id::new("sidebar_border")))
            .vline(
                side.response.rect.left(),
                divider,
                Stroke::new(1.0_f32, theme::border()),
            );

        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(theme::bg()).inner_margin(Margin::symmetric(14, 10)))
            .show(ctx, |ui| self.main_page(ui));

        // Window chrome: keep it below modals/drop overlays so popups are never
        // crossed by the title/status separators.
        let painter = ctx.layer_painter(LayerId::new(Order::Middle, Id::new("chrome")));
        painter.hline(
            screen.x_range(),
            titlebar_rect.bottom(),
            Stroke::new(1.0_f32, theme::border()),
        );
        painter.hline(
            screen.x_range(),
            status_rect.top(),
            Stroke::new(1.0_f32, theme::border()),
        );
        painter.rect_stroke(
            screen.shrink(0.5),
            CornerRadius::ZERO,
            Stroke::new(1.0_f32, theme::border()),
            StrokeKind::Inside,
        );

        self.draw_drop_overlay(ctx);
        self.drag_ghost(ctx);
        self.modals(ctx);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(rect) = self.last_window_size {
            self.cfg.window_size = Some(rect);
        }
        self.refresh_config();
        let _ = save_config(&self.cfg_path, &self.cfg);
    }
}

// ---------------------------------------------------------------------------
// Card / list drawing
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn draw_grid(
    ui: &mut Ui,
    sounds: &[Sound],
    filtered: &[usize],
    infos: &HashMap<u64, SoundInfo>,
    progress: &HashMap<u64, f32>,
    drag_enabled: bool,
    capture_id: Option<u64>,
    selected: Option<u64>,
    rects: &mut Vec<(usize, Rect)>,
    volume_rects: &mut Vec<(u64, Rect)>,
    actions: &mut Vec<Action>,
) {
    let spacing = 10.0;
    // Leave room for the floating scrollbar so the last column never touches
    // (or hides behind) the sidebar.
    let width = (ui.available_width() - 9.0).max(180.0);
    let columns = (((width + spacing) / 238.0).floor() as usize).clamp(1, 6);
    let card_width = ((width - spacing * (columns.saturating_sub(1)) as f32) / columns as f32)
        .max(170.0);

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        for chunk in filtered.chunks(columns) {
            ui.horizontal_top(|ui| {
                for (position, &index) in chunk.iter().enumerate() {
                    if position > 0 {
                        ui.add_space(spacing);
                    }
                    let sound = &sounds[index];
                    let info = infos.get(&sound.id).cloned().unwrap_or_default();
                    let playing = progress.get(&sound.id).copied();
                    let (rect, card_actions) = sound_card(
                        ui,
                        Vec2::new(card_width, CARD_HEIGHT),
                        sound,
                        &info,
                        playing,
                        drag_enabled,
                        capture_id == Some(sound.id),
                        selected == Some(sound.id),
                        index,
                        volume_rects,
                    );
                    rects.push((index, rect));
                    actions.extend(card_actions);
                }
            });
            ui.add_space(spacing);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_list(
    ui: &mut Ui,
    sounds: &[Sound],
    filtered: &[usize],
    infos: &HashMap<u64, SoundInfo>,
    progress: &HashMap<u64, f32>,
    drag_enabled: bool,
    capture_id: Option<u64>,
    selected: Option<u64>,
    rects: &mut Vec<(usize, Rect)>,
    volume_rects: &mut Vec<(u64, Rect)>,
    actions: &mut Vec<Action>,
) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        for &index in filtered {
            let sound = &sounds[index];
            let info = infos.get(&sound.id).cloned().unwrap_or_default();
            let playing = progress.get(&sound.id).copied();
            let (rect, row_actions) = sound_row(
                ui,
                sound,
                &info,
                playing,
                drag_enabled,
                capture_id == Some(sound.id),
                selected == Some(sound.id),
                index,
                volume_rects,
            );
            rects.push((index, rect));
            actions.extend(row_actions);
            ui.add_space(6.0);
        }
    });
}

fn close_button(ui: &mut Ui, painter: &egui::Painter, rect: Rect, id: Id) -> egui::Response {
    let response = ui.interact(rect, id, Sense::click());
    if response.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::same(5),
            theme::alpha(theme::red(), 26),
        );
    }
    let stroke = Stroke::new(
        1.4_f32,
        if response.hovered() {
            theme::red()
        } else {
            theme::muted()
        },
    );
    let center = rect.center();
    painter.line_segment(
        [
            center + Vec2::new(-4.0, -4.0),
            center + Vec2::new(4.0, 4.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            center + Vec2::new(4.0, -4.0),
            center + Vec2::new(-4.0, 4.0),
        ],
        stroke,
    );
    response
}

/// Hotkey pill shared by cards and rows. Returns true when clicked.
fn hotkey_pill(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    id: Id,
    sound: &Sound,
    capture_active: bool,
    font_size: f32,
) -> bool {
    let pill = ui.interact(rect, id, Sense::click());
    let has_error = sound.hotkey_error.is_some() || capture_active;
    let border = if has_error {
        theme::red()
    } else if pill.hovered() {
        theme::accent()
    } else {
        theme::border()
    };
    painter.rect(
        rect,
        CornerRadius::same(6),
        if pill.hovered() {
            theme::alpha(theme::accent(), 20)
        } else {
            theme::field()
        },
        Stroke::new(1.0_f32, border),
        StrokeKind::Inside,
    );
    let (label, color) = match sound.hotkey {
        Some(hotkey) => (
            hotkey.label(),
            if has_error { theme::red() } else { theme::text() },
        ),
        None => ("Set hotkey".to_string(), theme::faint()),
    };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::ui_font(font_size),
        color,
    );
    let clicked = pill.clicked();
    if let Some(error) = &sound.hotkey_error {
        pill.on_hover_text(error);
    } else {
        pill.on_hover_text("Set a global hotkey (works while hidden)");
    }
    clicked
}

/// RAM / disk / missing chip. Returns the action for a click, if any.
fn ram_chip(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    id: Id,
    sound: &Sound,
    info: &SoundInfo,
    font_size: f32,
) -> Option<Action> {
    let missing = info.error.is_some() && !info.loading || info.missing;
    let has_hotkey = sound.hotkey.is_some();
    if !has_hotkey && !missing {
        return None;
    }
    let ram_active = has_hotkey && sound.ram;
    let (label, color) = if missing {
        ("MISSING", theme::red())
    } else if info.loading {
        ("LOADING", theme::muted())
    } else if ram_active {
        ("RAM", theme::purple())
    } else {
        ("DISK", theme::faint())
    };
    let chip = ui.interact(rect, id, Sense::click());
    painter.rect(
        rect,
        CornerRadius::same(5),
        theme::alpha(color, if chip.hovered() { 20 } else { 10 }),
        Stroke::new(1.0_f32, theme::alpha(color, 60)),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::ui_font(font_size),
        color,
    );
    let clicked = chip.clicked();
    chip.on_hover_text(if missing {
        "File is missing - click to locate"
    } else if ram_active {
        "In RAM - click to play from disk"
    } else {
        "Playing from disk - click to keep in RAM"
    });
    if !clicked {
        return None;
    }
    Some(if missing {
        Action::Locate(sound.id)
    } else {
        Action::ToggleRam(sound.id)
    })
}

fn play_progress(painter: &egui::Painter, track: Rect, progress: f32, alpha: u8) {
    painter.rect_filled(
        track,
        CornerRadius::same(1),
        theme::alpha(theme::accent(), alpha),
    );
    let filled = Rect::from_min_max(
        track.left_top(),
        Pos2::new(
            track.left() + track.width() * progress.clamp(0.0, 1.0),
            track.bottom(),
        ),
    );
    painter.rect_filled(filled, CornerRadius::same(1), theme::accent());
}

fn sound_card(
    ui: &mut Ui,
    size: Vec2,
    sound: &Sound,
    info: &SoundInfo,
    progress: Option<f32>,
    drag_enabled: bool,
    capture_active: bool,
    selected: bool,
    index: usize,
    volume_rects: &mut Vec<(u64, Rect)>,
) -> (Rect, Vec<Action>) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let mut actions = Vec::new();
    let painter = ui.painter().clone();
    let has_hotkey = sound.hotkey.is_some();
    let ram_active = has_hotkey && sound.ram;
    let playing = progress.is_some();
    let missing = info.error.is_some() && !info.loading || info.missing;
    let flash = sound
        .flash_until
        .map(|t| t > Instant::now())
        .unwrap_or(false);

    let mut fill = theme::card();
    if ram_active {
        fill = theme::mix(fill, theme::purple(), 0.06);
    }
    if playing {
        fill = theme::mix(fill, theme::accent(), 0.10);
    }
    if missing {
        fill = theme::mix(fill, theme::red(), 0.10);
    }
    if response.hovered() {
        fill = theme::mix(fill, theme::text(), 0.03);
    }
    let border = if flash {
        theme::accent()
    } else if playing {
        theme::accent()
    } else if missing {
        theme::red()
    } else if selected {
        theme::accent()
    } else if ram_active {
        theme::alpha(theme::purple(), 110)
    } else {
        theme::border()
    };
    painter.rect(
        rect,
        CornerRadius::same(10),
        fill,
        Stroke::new(if selected || flash { 1.5_f32 } else { 1.0_f32 }, border),
        StrokeKind::Inside,
    );
    if flash {
        painter.rect(
            rect,
            CornerRadius::same(10),
            theme::alpha(theme::accent(), 18),
            Stroke::NONE,
            StrokeKind::Inside,
        );
    }

    if response.clicked() {
        actions.push(Action::Play(sound.id));
    }
    if drag_enabled && response.drag_started() {
        actions.push(Action::DragStart {
            id: sound.id,
            index,
            size,
        });
    }
    if response.drag_stopped() {
        actions.push(Action::DragStop);
    }
    response.context_menu(|ui| {
        sound_context_menu(ui, sound, info, &mut actions);
    });

    // Compact RAM / disk / missing chip at the top-left.
    let chip_rect = Rect::from_min_size(
        rect.left_top() + Vec2::new(10.0, 8.0),
        Vec2::new(54.0, 20.0),
    );
    if let Some(action) = ram_chip(
        ui,
        &painter,
        chip_rect,
        ui.id().with(("ram", sound.id)),
        sound,
        info,
        10.5,
    ) {
        actions.push(action);
    }

    // Close button: clean vector X, exactly centered.
    let close_rect = Rect::from_min_size(
        rect.right_top() + Vec2::new(-30.0, 7.0),
        Vec2::new(22.0, 22.0),
    );
    let close = close_button(ui, &painter, close_rect, ui.id().with(("close", sound.id)));
    if close.clicked() {
        actions.push(Action::Remove(sound.id));
    }
    close.on_hover_text("Remove sound");

    // Name.
    let name_rect = Rect::from_min_max(
        rect.left_top() + Vec2::new(13.0, 30.0),
        Pos2::new(rect.right() - 38.0, rect.top() + 68.0),
    );
    let name = theme::truncate(&painter, &sound.name, theme::medium(17.0), name_rect.width());
    painter.text(
        name_rect.left_center(),
        Align2::LEFT_CENTER,
        name,
        theme::medium(17.0),
        if missing { theme::mix(theme::text(), theme::red(), 0.4) } else { theme::text() },
    );

    // Hotkey pill.
    let pill_width = if sound.hotkey.is_some() { 78.0 } else { 88.0 };
    let pill_rect = Rect::from_min_size(
        rect.left_bottom() + Vec2::new(12.0, -34.0),
        Vec2::new(pill_width, 24.0),
    );
    if hotkey_pill(
        ui,
        &painter,
        pill_rect,
        ui.id().with(("hotkey", sound.id)),
        sound,
        capture_active,
        12.5,
    ) {
        actions.push(Action::SetHotkey(sound.id));
    }

    // Volume slider (bottom right).
    let slider_rect = Rect::from_min_max(
        Pos2::new(pill_rect.right() + 10.0, rect.bottom() - 34.0),
        Pos2::new(rect.right() - 12.0, rect.bottom() - 10.0),
    );
    volume_rects.push((sound.id, slider_rect));
    volume_bar(
        ui,
        slider_rect,
        ui.id().with(("cardvol", sound.id)),
        sound.volume,
        |v| actions.push(Action::Volume(sound.id, v)),
        false,
    );

    // Playing progress line.
    if let Some(progress) = progress {
        let track = Rect::from_min_max(
            Pos2::new(rect.left() + 10.0, rect.bottom() - 4.0),
            Pos2::new(rect.right() - 10.0, rect.bottom() - 2.0),
        );
        play_progress(&painter, track, progress, 45);
    }

    if response.double_clicked() {
        actions.push(Action::Rename(sound.id));
    }
    response.on_hover_text(sound.path.display().to_string())
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    (rect, actions)
}

fn sound_row(
    ui: &mut Ui,
    sound: &Sound,
    info: &SoundInfo,
    progress: Option<f32>,
    drag_enabled: bool,
    capture_active: bool,
    selected: bool,
    index: usize,
    volume_rects: &mut Vec<(u64, Rect)>,
) -> (Rect, Vec<Action>) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), Sense::click_and_drag());
    let mut actions = Vec::new();
    let painter = ui.painter().clone();
    let has_hotkey = sound.hotkey.is_some();
    let ram_active = has_hotkey && sound.ram;
    let missing = info.error.is_some() && !info.loading || info.missing;
    let playing = progress.is_some();
    let flash = sound.flash_until.map(|t| t > Instant::now()).unwrap_or(false);

    let mut fill = theme::card();
    if ram_active {
        fill = theme::mix(fill, theme::purple(), 0.06);
    }
    if playing {
        fill = theme::mix(fill, theme::accent(), 0.10);
    }
    if missing {
        fill = theme::mix(fill, theme::red(), 0.08);
    }
    if response.hovered() {
        fill = theme::mix(fill, theme::text(), 0.03);
    }
    let border = if flash {
        theme::accent()
    } else if playing {
        theme::accent()
    } else if missing {
        theme::red()
    } else if selected {
        theme::accent()
    } else if ram_active {
        theme::alpha(theme::purple(), 100)
    } else {
        theme::border_soft()
    };
    painter.rect(
        rect,
        CornerRadius::same(8),
        fill,
        Stroke::new(1.0_f32, border),
        StrokeKind::Inside,
    );

    if response.clicked() {
        actions.push(Action::Play(sound.id));
    }
    if drag_enabled && response.drag_started() {
        actions.push(Action::DragStart {
            id: sound.id,
            index,
            size: Vec2::new(220.0, ROW_HEIGHT),
        });
    }
    if response.drag_stopped() {
        actions.push(Action::DragStop);
    }
    response.context_menu(|ui| {
        sound_context_menu(ui, sound, info, &mut actions);
    });

    // Name.
    let name_rect = Rect::from_min_max(
        rect.left_center() + Vec2::new(16.0, -14.0),
        Pos2::new(rect.left() + rect.width() * 0.38, rect.center().y + 14.0),
    );
    let name = theme::truncate(&painter, &sound.name, theme::medium(15.0), name_rect.width());
    painter.text(
        name_rect.left_center(),
        Align2::LEFT_CENTER,
        name,
        theme::medium(15.0),
        if missing { theme::mix(theme::text(), theme::red(), 0.4) } else { theme::text() },
    );

    // Volume slider in the middle.
    let pill_width = if sound.hotkey.is_some() { 76.0 } else { 84.0 };
    let indicator_width = if has_hotkey || missing { 58.0 } else { 0.0 };
    let volume_rect = Rect::from_min_max(
        Pos2::new(rect.left() + rect.width() * 0.40, rect.top() + 14.0),
        Pos2::new(
            rect.right() - 44.0 - pill_width - 8.0 - indicator_width,
            rect.top() + 32.0,
        ),
    );
    if volume_rect.width() > 40.0 {
        volume_rects.push((sound.id, volume_rect));
        volume_bar(
            ui,
            volume_rect,
            ui.id().with(("rowvol", sound.id)),
            sound.volume,
            |v| actions.push(Action::Volume(sound.id, v)),
            true,
        );
    }

    // Hotkey + close.
    let close_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 18.0, rect.center().y),
        Vec2::new(24.0, 24.0),
    );
    let close = close_button(ui, &painter, close_rect, ui.id().with(("rowclose", sound.id)));
    if close.clicked() {
        actions.push(Action::Remove(sound.id));
    }
    close.on_hover_text("Remove sound");

    let pill_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 44.0 - pill_width, rect.center().y - 11.0),
        Vec2::new(pill_width, 22.0),
    );
    if hotkey_pill(
        ui,
        &painter,
        pill_rect,
        ui.id().with(("rowhotkey", sound.id)),
        sound,
        capture_active,
        12.0,
    ) {
        actions.push(Action::SetHotkey(sound.id));
    }

    // RAM / disk / missing chip just before the hotkey pill.
    let chip_rect = Rect::from_min_size(
        Pos2::new(pill_rect.left() - indicator_width, rect.center().y - 11.0),
        Vec2::new(indicator_width - 8.0, 22.0),
    );
    if let Some(action) = ram_chip(
        ui,
        &painter,
        chip_rect,
        ui.id().with(("rowram", sound.id)),
        sound,
        info,
        10.0,
    ) {
        actions.push(action);
    }

    if let Some(progress) = progress {
        let track = Rect::from_min_max(
            Pos2::new(rect.left() + 8.0, rect.bottom() - 3.0),
            Pos2::new(rect.right() - 8.0, rect.bottom() - 1.0),
        );
        play_progress(&painter, track, progress, 40);
    }

    if response.double_clicked() {
        actions.push(Action::Rename(sound.id));
    }
    let tooltip = match &info.error {
        Some(error) => error.clone(),
        None => sound.path.display().to_string(),
    };
    response.on_hover_text(tooltip)
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    (rect, actions)
}

fn sound_context_menu(ui: &mut Ui, sound: &Sound, info: &SoundInfo, actions: &mut Vec<Action>) {
    if ui.button("Play").clicked() {
        actions.push(Action::Play(sound.id));
        ui.close_menu();
    }
    if ui.button("Stop").clicked() {
        actions.push(Action::Stop(sound.id));
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Rename...").clicked() {
        actions.push(Action::Rename(sound.id));
        ui.close_menu();
    }
    if ui.button("Set hotkey...").clicked() {
        actions.push(Action::SetHotkey(sound.id));
        ui.close_menu();
    }
    if sound.hotkey.is_some() && ui.button("Clear hotkey").clicked() {
        actions.push(Action::ClearHotkey(sound.id));
        ui.close_menu();
    }
    if sound.hotkey.is_some() {
        ui.separator();
        if sound.ram {
            if ui.button("Play from disk").clicked() {
                actions.push(Action::ToggleRam(sound.id));
                ui.close_menu();
            }
            if ui.button("Reload in RAM").clicked() {
                actions.push(Action::Reload(sound.id));
                ui.close_menu();
            }
        } else if ui.button("Keep in RAM").clicked() {
            actions.push(Action::ToggleRam(sound.id));
            ui.close_menu();
        }
    }
    if info.error.is_some() || info.missing {
        if ui.button("Locate file...").clicked() {
            actions.push(Action::Locate(sound.id));
            ui.close_menu();
        }
    }
    if ui.button("Show in Explorer").clicked() {
        actions.push(Action::ShowInExplorer(sound.id));
        ui.close_menu();
    }
    ui.separator();
    if ui
        .button(RichText::new("Remove").color(theme::red()))
        .clicked()
    {
        actions.push(Action::Remove(sound.id));
        ui.close_menu();
    }
}

fn volume_bar(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    mut value: f32,
    mut on_change: impl FnMut(f32),
    compact: bool,
) {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let painter = ui.painter();
    let label_width = if compact { 0.0 } else { 34.0 };
    let track_left = if compact {
        rect.left() + 2.0
    } else {
        let speaker_size = 14.0;
        let speaker_rect = Rect::from_center_size(
            Pos2::new(rect.left() + speaker_size * 0.5, rect.center().y),
            Vec2::splat(speaker_size),
        );
        theme::speaker_icon(
            painter,
            speaker_rect,
            if response.hovered() {
                theme::muted()
            } else {
                theme::faint()
            },
        );
        speaker_rect.right() + 8.0
    };
    let track = Rect::from_min_max(
        Pos2::new(track_left, rect.center().y - 2.0),
        Pos2::new(rect.right() - label_width - 2.0, rect.center().y + 2.0),
    );
    painter.rect_filled(track, CornerRadius::same(2), theme::field());
    let filled = Rect::from_min_max(
        track.left_top(),
        Pos2::new(track.left() + track.width() * value.clamp(0.0, 1.0), track.bottom()),
    );
    painter.rect_filled(filled, CornerRadius::same(2), theme::accent());
    let knob = Pos2::new(filled.right(), track.center().y);
    painter.circle_filled(knob, if response.hovered() { 5.0 } else { 4.0 }, theme::text());
    if !compact {
        painter.text(
            Pos2::new(rect.right() - 2.0, rect.center().y),
            Align2::RIGHT_CENTER,
            format!("{:.0}%", value * 100.0),
            theme::ui_font(11.0),
            theme::muted(),
        );
    }

    if response.dragged() || response.clicked() {
        if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
            let t = ((pointer.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
            value = t;
            on_change(value);
        }
    }
    response.on_hover_text(format!("Volume {:.0}% - scroll to adjust", value * 100.0));
}

// ---------------------------------------------------------------------------
// Small widgets
// ---------------------------------------------------------------------------

fn accent_button(ui: &mut Ui, text: &str, size: Vec2) -> egui::Response {
    accent_button_enabled(ui, text, size, true)
}

fn accent_button_enabled(ui: &mut Ui, text: &str, size: Vec2, enabled: bool) -> egui::Response {
    let button = Button::new(RichText::new(text).size(15.0).color(if enabled {
        theme::accent()
    } else {
        theme::faint()
    }))
    .fill(if enabled {
        theme::alpha(theme::accent(), 22)
    } else {
        theme::field()
    })
    .stroke(Stroke::new(1.0_f32,
        if enabled { theme::accent() } else { theme::border() },
    ))
    .corner_radius(8);
    let mut response = ui.add_sized(size, button);
    if !enabled {
        response = response.on_disabled_hover_text("Not available");
    }
    response
}

fn ghost_button(ui: &mut Ui, text: &str, size: Vec2) -> egui::Response {
    ui.add_sized(
        size,
        Button::new(RichText::new(text).size(13.5).color(theme::text()))
            .fill(theme::card())
            .stroke(Stroke::new(1.0_f32, theme::border()))
            .corner_radius(8),
    )
}

fn tiny_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        Button::new(RichText::new(text).size(11.5).color(theme::muted()))
            .frame(false),
    )
}

fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(10.5)
            .color(theme::faint())
            .extra_letter_spacing(1.5),
    );
}

fn section_line(ui: &mut Ui) {
    let rect = Rect::from_min_size(
        ui.cursor().min,
        Vec2::new(ui.available_width(), 1.0),
    );
    ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::border_soft());
    ui.allocate_space(rect.size());
}

fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}

fn paint_logo(painter: &egui::Painter, rect: Rect) {
    painter.rect(
        rect,
        CornerRadius::same(6),
        theme::alpha(theme::accent(), 28),
        Stroke::new(1.0_f32, theme::accent()),
        StrokeKind::Inside,
    );
    let base = rect.left_bottom() + Vec2::new(5.0, -5.0);
    let heights = [7.0, 11.0, 5.0];
    for (i, height) in heights.iter().enumerate() {
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(base.x + i as f32 * 4.6, base.y - height),
                Vec2::new(2.8, *height),
            ),
            CornerRadius::same(1),
            if i == 1 { theme::accent() } else { theme::text() },
        );
    }
}

fn paint_link_row(ui: &mut Ui, rect: Rect, linked: bool) {
    let painter = ui.painter();
    painter.line_segment(
        [
            Pos2::new(rect.left() + 20.0, rect.center().y),
            Pos2::new(rect.center().x - 24.0, rect.center().y),
        ],
        Stroke::new(1.0_f32, theme::border_soft()),
    );
    painter.line_segment(
        [
            Pos2::new(rect.center().x + 24.0, rect.center().y),
            Pos2::new(rect.right() - 20.0, rect.center().y),
        ],
        Stroke::new(1.0_f32, theme::border_soft()),
    );
    let button = Rect::from_center_size(rect.center(), Vec2::new(38.0, 26.0));
    let hovered = ui
        .ctx()
        .pointer_hover_pos()
        .map(|p| button.contains(p))
        .unwrap_or(false);
    painter.rect(
        button,
        CornerRadius::same(7),
        if linked {
            theme::alpha(theme::accent(), 24)
        } else {
            theme::field()
        },
        Stroke::new(1.0_f32,
            if linked {
                theme::accent()
            } else if hovered {
                theme::muted()
            } else {
                theme::border()
            },
        ),
        StrokeKind::Inside,
    );
    theme::chain_icon(
        painter,
        button.shrink(8.0),
        if linked { theme::accent() } else { theme::muted() },
        linked,
    );
}

enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

fn window_button(
    ui: &mut Ui,
    kind: WindowButton,
    cfg: &Config,
    tray_available: bool,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(36.0, 28.0), Sense::click());
    let hovered = response.hovered();
    let close = matches!(kind, WindowButton::Close);
    if hovered {
        ui.painter().rect_filled(
            rect.shrink2(Vec2::new(2.0, 2.0)),
            CornerRadius::same(6),
            if close {
                theme::alpha(theme::red(), 50)
            } else {
                theme::card_hover()
            },
        );
    }
    let color = if close && hovered {
        theme::red()
    } else if hovered {
        theme::text()
    } else {
        theme::muted()
    };
    let c = rect.center();
    let stroke = Stroke::new(1.2_f32, color);
    match kind {
        WindowButton::Minimize => {
            ui.painter()
                .line_segment([Pos2::new(c.x - 5.0, c.y + 4.0), Pos2::new(c.x + 5.0, c.y + 4.0)], stroke);
        }
        WindowButton::Maximize => {
            ui.painter().rect_stroke(
                Rect::from_center_size(c, Vec2::new(10.0, 10.0)),
                CornerRadius::same(1),
                stroke,
                StrokeKind::Inside,
            );
        }
        WindowButton::Close => {
            ui.painter().line_segment(
                [c + Vec2::new(-4.5, -4.5), c + Vec2::new(4.5, 4.5)],
                stroke,
            );
            ui.painter().line_segment(
                [c + Vec2::new(4.5, -4.5), c + Vec2::new(-4.5, 4.5)],
                stroke,
            );
        }
    }
    let tooltip = match kind {
        WindowButton::Minimize => "Minimize",
        WindowButton::Maximize => "Maximize",
        WindowButton::Close => {
            if cfg.close_to_tray && tray_available {
                "Hide to tray (hotkeys keep working)"
            } else {
                "Quit SoundDeck"
            }
        }
    };
    let response = response.on_hover_text(tooltip);
    response.clicked()
}

fn format_gain_db(db: f32) -> String {
    if db.abs() < 0.05 {
        "0.0 dB".to_string()
    } else {
        format!("{db:+.1} dB")
    }
}

fn paint_rotate_icon(painter: &egui::Painter, rect: Rect, color: Color32, clockwise: bool) {
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) * 0.30).max(3.0);
    let start = -0.6f32;
    let sweep = if clockwise { 4.6f32 } else { -4.6f32 };
    let stroke = Stroke::new(1.3_f32, color);
    let mut points = Vec::with_capacity(19);
    for i in 0..=18 {
        let angle = start + sweep * i as f32 / 18.0;
        points.push(center + Vec2::new(angle.cos(), angle.sin()) * radius);
    }
    painter.add(egui::Shape::line(points, stroke));
    let end = start + sweep;
    let tip = center + Vec2::new(end.cos(), end.sin()) * radius;
    let travel = Vec2::new(-end.sin(), end.cos()) * sweep.signum();
    let normal = Vec2::new(end.cos(), end.sin());
    painter.line_segment([tip, tip - travel * 4.0 + normal * 1.5], stroke);
    painter.line_segment([tip, tip - travel * 4.0 - normal * 1.5], stroke);
}

/// Small circular-arrow button. Returns true when clicked while `active`.
fn reset_button(ui: &mut Ui, active: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
    let color = if !active {
        theme::faint()
    } else if response.hovered() {
        theme::accent()
    } else {
        theme::muted()
    };
    if active && response.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(5),
            theme::alpha(theme::accent(), 16),
        );
    }
    paint_rotate_icon(ui.painter(), rect.shrink(2.0), color, false);
    let clicked = response.clicked() && active;
    response.on_hover_text("Reset gain to 0.0 dB");
    clicked
}

fn paint_switch(painter: &egui::Painter, rect: Rect, on: bool, mixed: bool, enabled: bool) {
    let track = if !enabled {
        theme::field()
    } else if on {
        theme::accent()
    } else if mixed {
        theme::alpha(theme::purple(), 90)
    } else {
        theme::card_hover()
    };
    painter.rect_filled(rect, CornerRadius::same(11), track);
    let border = if !enabled || !(on || mixed) {
        theme::border()
    } else if on {
        theme::accent()
    } else {
        theme::purple()
    };
    painter.rect_stroke(
        rect,
        CornerRadius::same(11),
        Stroke::new(1.0_f32, border),
        StrokeKind::Inside,
    );
    let knob_x = if on {
        rect.right() - 11.0
    } else if mixed {
        rect.center().x
    } else {
        rect.left() + 11.0
    };
    painter.circle_filled(
        Pos2::new(knob_x, rect.center().y),
        8.0,
        if enabled { theme::text() } else { theme::faint() },
    );
}

fn resolve_device(
    devices: &[DeviceInfo],
    id: Option<&str>,
    name: Option<&str>,
) -> Option<DeviceInfo> {
    if let Some(id) = id {
        if let Some(found) = devices.iter().find(|d| d.id == id) {
            return Some(found.clone());
        }
    }
    if let Some(name) = name {
        if let Some(found) = devices.iter().find(|d| d.name == name) {
            return Some(found.clone());
        }
    }
    None
}

// Extend the theme with the alpha helper name used in this file.
