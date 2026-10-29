use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, Color32, ComboBox, CornerRadius, FontId, Frame, Id, Key, LayerId,
    Layout, Margin, Order, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
    ViewportCommand,
};

use crate::audio::engine::{
    AudioEvent, DeviceRoute, Engine, Slot, SOUND_GAIN_MAX_DB, SOUND_GAIN_MIN_DB,
};
use crate::audio::stream::LoopState;
use crate::audio::wasapi::{self, DeviceInfo};
use crate::hotkeys::{
    blocked_reason, chord_owner, duplicate_sounds, held_chord, main_window_minimized,
    mods_from_egui, parse_bindable, ChordOwner, Hotkey, HotkeyService, ServiceEvent, TrayCommand,
};
use crate::icon;
use crate::model::{
    self, display_name, format_bytes, format_duration, is_supported_file, Layout as SoundLayout,
    APP_NAME, SUPPORTED_EXTENSIONS,
};
use crate::playback::policy;
use crate::playback::{Playback, PlaybackEvent, SoundInfo};
use crate::stats;
use crate::storage;
use crate::ui::anim;
use crate::ui::theme;
use crate::ui::volume;

const CARD_HEIGHT: f32 = 112.0;
const ROW_HEIGHT: f32 = 46.0;
const SIDEBAR_WIDTH: f32 = 300.0;

// Board metrics (B4). The toolbar and the grid both size themselves from
// `board_content_width`, so the RAM pill and the last column end on the same
// pixel at any window width instead of each guessing its own right margin.
/// Gap between grid columns, and between grid rows.
const GRID_GAP: f32 = 10.0;
/// Column pitch: the width one card plus one gap wants before the grid adds a
/// column.
const GRID_COLUMN_PITCH: f32 = 238.0;
/// Hard floor for a card, so a narrow board still shows something.
const GRID_MIN_CARD_W: f32 = 170.0;
const GRID_MAX_COLUMNS: usize = 6;
/// Board content never gets narrower than this, whatever the window does.
const BOARD_CONTENT_MIN_W: f32 = 180.0;
/// The search field keeps at least this much of the toolbar.
const SEARCH_MIN_W: f32 = 160.0;

// Sidebar stop-all widget (B7): one hit rect for the body, one for the pill.
const STOP_ALL_PILL_W: f32 = 96.0;
const STOP_ALL_PILL_H: f32 = 24.0;
/// Gap between the body's hit rect and the pill's, so they never overlap.
const STOP_ALL_BODY_GAP: f32 = 8.0;
/// Master gain slider range. `Settings::normalize` clamps to the same bounds;
/// the UI only ever hands it a value from inside this range.
const MASTER_GAIN_RANGE: std::ops::RangeInclusive<f32> = 0.0..=12.0;
/// Right-clicking the gain slider returns it here: 0 dB, i.e. unity.
const MASTER_GAIN_NEUTRAL_DB: f32 = 0.0;

// Settings layout metrics (SPEC FR-5.9.8 spacing, §7.2 widths). Every number the
// settings screen uses comes from here so the four pages keep one rhythm.
const SETTINGS_CONTENT_MAX_W: f32 = 780.0;
const SETTINGS_ABOUT_MAX_W: f32 = 680.0;
const SETTINGS_CARD_PADDING: i8 = 10;
/// Title of a group to the top of its card.
const SETTINGS_GROUP_TITLE_GAP: f32 = 5.0;
/// Card to the next group title, and the header to the first group.
const SETTINGS_GROUP_GAP: f32 = 8.0;
/// Height of one label/control row (and of the controls inside it).
const SETTINGS_ROW_H: f32 = 28.0;
/// Space between a row's label and its control.
const SETTINGS_ROW_LABEL_GAP: f32 = 16.0;
const SETTINGS_ROW_LABEL_MIN_W: f32 = 160.0;
/// Content width below which a row stacks its label above the control. This is a
/// width breakpoint; it is unrelated to the sidebar's 620 px *height* rule.
const SETTINGS_ROW_STACK_W: f32 = 620.0;
/// Space above and below a divider rule.
const SETTINGS_DIVIDER_GAP: f32 = 7.0;
/// Row to the muted detail line under it.
const SETTINGS_DETAIL_GAP: f32 = 6.0;
/// A stacked row's label to the control under it.
const SETTINGS_STACK_LABEL_GAP: f32 = 6.0;
/// Every settings button shares one size, so the rows line up.
const SETTINGS_BUTTON_SIZE: Vec2 = Vec2::new(96.0, SETTINGS_ROW_H);
const SETTINGS_SWITCH_SIZE: Vec2 = Vec2::new(40.0, 22.0);
/// Wide text value column (licence, version): long values wrap instead of being
/// clipped, and `right_to_left` keeps the text flush with the other controls.
const SETTINGS_TEXT_COL_W: f32 = 200.0;
const SETTINGS_THEME_W: f32 = 240.0;
const SETTINGS_RULE_H: f32 = 1.0;
const SETTINGS_RAIL_ITEM_H: f32 = 34.0;
const SETTINGS_RAIL_ITEM_GAP: f32 = 3.0;
const SETTINGS_RAIL_SECTION_GAP: f32 = 8.0;
const SETTINGS_RAIL_RULE_GAP: f32 = 6.0;
/// Left inset of a rail item's glyph centre and of its text, shared by Back and
/// the tab items so the rail lines up.
const SETTINGS_RAIL_GLYPH_X: f32 = 18.0;
const SETTINGS_RAIL_TEXT_X: f32 = 34.0;
/// What the pinned About item needs at the bottom of the rail: the rule, the gap
/// under it and the item itself.
const SETTINGS_RAIL_FOOTER_H: f32 = SETTINGS_RULE_H + SETTINGS_RAIL_RULE_GAP + SETTINGS_RAIL_ITEM_H;

/// Width of the chip the drag ghost shows for a list row (a card-sized ghost
/// would be wider than the row it came from).
const ROW_DRAG_WIDTH: f32 = 220.0;

/// Narrowest row volume track worth showing; below this the rows are squashed
/// and the bar would be a stub, so it is dropped entirely.
const MIN_VOLUME_WIDTH: f32 = 80.0;

/// Inner margin of a modal body; the activity log tightens it to fit more rows.
const MODAL_MARGIN: i8 = 18;
const MODAL_MARGIN_COMPACT: i8 = 12;

/// Body width of a confirmation dialog (delete library, import, reset): wide
/// enough for the action row and a target combobox without wrapping.
const CONFIRM_MODAL_WIDTH: f32 = 460.0;

/// Default file name an export offers.
const DEFAULT_BUNDLE_NAME: &str = "sounddeck-config.json";

/// Level-meter motion: instant attack, `METER_RELEASE_DB_PER_SEC` release, and a
/// peak that holds for `METER_PEAK_HOLD` before falling at its own rate.
const METER_FLOOR_DB: f32 = -60.0;
const METER_RELEASE_DB_PER_SEC: f32 = 24.0;
const METER_PEAK_RELEASE_DB_PER_SEC: f32 = 18.0;
const METER_PEAK_HOLD: Duration = Duration::from_millis(900);

/// Hard cap on rendered search results; protects a pathological library.
const MAX_SEARCH_RESULTS: usize = 500;

/// Hard cap on the activity log's memory; the oldest entry is dropped.
const MAX_STATUS_ENTRIES: usize = 200;

/// Hotkey pill text for a sound that has no binding yet.
const HOTKEY_UNBOUND_LABEL: &str = "Set hotkey";

/// One click of the per-sound trim stepper (FR-5.3.17). The range itself lives
/// with the audio engine, where the trim is turned into a gain.
const GAIN_STEP_DB: f32 = 1.0;

const DEVELOPER_NAME: &str = "Mantum";

/// Personal links shown under the credit line in About. Entries with an empty
/// URL are skipped, so nothing renders until the URL is real.
const DEVELOPER_LINKS: &[(&str, &str)] = &[
    ("YouTube", "https://youtube.com/@mantu_um"),
    ("GitHub", "https://github.com/mantuoum"),
];

/// About -> project links. These assume the repository is called `SoundDeck`;
/// change the URLs (and `Cargo.toml`) if it ends up with another name.
const ABOUT_LINKS: &[(&str, &str)] = &[
    ("Support", "https://github.com/mantuoum/SoundDeck/issues"),
    ("Releases", "https://github.com/mantuoum/SoundDeck/releases"),
];

const LICENSE_NAME: &str = "MIT";

#[derive(Clone, Copy, PartialEq, Eq)]
enum RamState {
    Off,
    On,
    Mixed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Playback,
    Appearance,
    Window,
    About,
}

impl SettingsTab {
    const ALL: [Self; 4] = [Self::Playback, Self::Appearance, Self::Window, Self::About];

    fn title(self) -> &'static str {
        match self {
            Self::Playback => "Playback",
            Self::Appearance => "Appearance",
            Self::Window => "Window",
            Self::About => "About",
        }
    }

    /// The `SettingsGroup` the page's Reset button restores. About has no fields
    /// of its own — only the whole-config actions — so it has no group.
    fn group(self) -> Option<storage::SettingsGroup> {
        match self {
            Self::Playback => Some(storage::SettingsGroup::Playback),
            Self::Appearance => Some(storage::SettingsGroup::Appearance),
            Self::Window => Some(storage::SettingsGroup::Window),
            Self::About => None,
        }
    }
}

pub struct Sound {
    pub id: u64,
    pub name: String,
    pub path: PathBuf,
    pub hotkey: Option<Hotkey>,
    pub volume: f32,
    /// Per-sound trim in dB on top of `volume`, −24..=+12 (FR-5.3.17).
    pub gain_db: f32,
    /// Only meaningful for sounds with a hotkey. Everything else always plays
    /// from disk.
    pub ram: bool,
    pub library_id: u64,
    /// Lower-cased name + file name, built once so search never re-allocates.
    pub search_key: String,
    /// Hotkey pill text, built with the other cached labels so painting never
    /// formats it; `None` until the sound has a binding.
    pub hotkey_label: Option<String>,
    /// Path as the card tooltip and the activity log show it.
    pub path_label: String,
    pub flash_until: Option<Instant>,
}

#[derive(Clone, Debug)]
struct Library {
    id: u64,
    name: String,
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

/// What the user has to know about one output slot beyond "ready": a
/// substitution (the configured device is gone and another one is playing) or
/// the engine's last failure. One array per slot, because Settings shows one
/// status line per slot (FR-5.5.8).
struct DeviceNote {
    text: String,
    kind: StatusKind,
}

impl DeviceNote {
    fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusKind::Warning,
        }
    }

    fn failure(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusKind::Error,
        }
    }

    /// A start confirms the route works, so a failure clears — but a
    /// substitution stays: audio is coming out of a device the user did not
    /// choose, and that is still true once it is running.
    fn cleared_by_start(self) -> Option<Self> {
        if self.kind == StatusKind::Error {
            None
        } else {
            Some(self)
        }
    }
}

/// Append one activity entry. A repeated message refreshes the newest entry
/// instead of stacking copies (UI-6), and the log stays at
/// [`MAX_STATUS_ENTRIES`] by dropping the oldest — one rule for every caller.
fn record_status(statuses: &mut VecDeque<StatusEntry>, kind: StatusKind, text: String) {
    let now = Instant::now();
    if let Some(last) = statuses.back_mut() {
        if last.text == text {
            last.kind = kind;
            last.at = now;
            return;
        }
    }
    statuses.push_back(StatusEntry {
        kind,
        text,
        at: now,
    });
    if statuses.len() > MAX_STATUS_ENTRIES {
        statuses.pop_front();
    }
}

#[derive(Clone, Copy)]
enum CaptureTarget {
    Sound(u64),
    StopAll,
}

struct CaptureState {
    target: CaptureTarget,
    current: Option<Hotkey>,
}

struct RenameState {
    target: RenameTarget,
    buffer: String,
    focus: bool,
}

#[derive(Clone, Copy)]
enum RenameTarget {
    Sound(u64),
    Library(u64),
}

struct DragState {
    sound_id: u64,
    size: Vec2,
    name: String,
    hotkey: String,
}

struct UndoDelete {
    sound: Sound,
    index: usize,
}

/// Buffers the board rebuilds from scratch every frame. Immediate mode throws
/// this data away at the end of the frame, so it must never be cached across
/// frames — but the allocations can be, which is all this is for.
#[derive(Default)]
struct BoardScratch {
    /// Indices into `App::sounds` of the sounds the current filter shows.
    filtered: Vec<usize>,
    /// Ids of those sounds, for the one playback-state query per frame.
    ids: Vec<u64>,
    /// Sound id -> progress bar while it plays.
    playing: HashMap<u64, Option<f32>>,
    /// Actions collected while painting, applied after the board is drawn.
    actions: Vec<Action>,
}

impl BoardScratch {
    fn clear(&mut self) {
        self.filtered.clear();
        self.ids.clear();
        self.playing.clear();
        self.actions.clear();
    }
}

/// What confirming the delete-library dialog does with the library's sounds.
#[derive(Clone, Copy)]
enum LibraryDeleteOutcome {
    Move(u64),
    Remove,
}

/// Open delete-library dialog. `target_id` is the library the move option sends
/// the sounds to (the first remaining one until the user picks another).
struct DeleteLibraryState {
    library_id: u64,
    target_id: u64,
}

/// A parsed, validated bundle waiting for the user to confirm the swap. Nothing
/// is applied until then; closing the dialog drops this and changes nothing.
struct ImportState {
    /// File name for the dialog and the log, never the whole path.
    name: String,
    bundle: storage::bundle::Bundle,
    sounds: usize,
    libraries: usize,
}

enum Action {
    Play(u64),
    Stop(u64),
    Remove(u64),
    Rename(u64),
    SetHotkey(u64),
    ClearHotkey(u64),
    ToggleRam(u64),
    MoveSound(u64, u64),
    Reload(u64),
    Locate(u64),
    ShowInExplorer(u64),
    Volume(u64, f32),
    /// Per-sound trim in dB (FR-5.3.17).
    Gain(u64, f32),
    DragStart {
        id: u64,
        size: Vec2,
    },
    DragStop,
}

/// The Playback page's readouts. Recomputing them every frame walks the whole
/// sound list and locks the playback registry, so they are cached and refreshed
/// only when something that can change them happens.
#[derive(Default)]
struct PlaybackStats {
    fresh: bool,
    ram: Option<RamState>,
    usage: policy::Usage,
    /// The cache line, formatted when the numbers change rather than per frame.
    label: String,
}

pub struct App {
    cfg: storage::Settings,
    sounds: Vec<Sound>,
    libraries: Vec<Library>,
    active_library: u64,
    playback: Arc<Playback>,
    engine: Arc<Engine>,
    hotkeys: HotkeyService,
    devices: Vec<DeviceInfo>,
    /// The Windows default render endpoint from the last enumeration; the Monitor
    /// fallback when the configured device is gone.
    default_device: Option<String>,
    device_ready: [bool; 2],
    device_note: [Option<DeviceNote>; 2],
    tray_available: bool,
    hidden: bool,
    /// Set when the window is restored from the tray so the OS-minimize
    /// detector cannot re-hide it on the same or the next frame.
    restored_at: Instant,
    /// Previous frame's minimized state, so only a real minimize transition
    /// hides the window (a stale level signal used to undo every restore).
    was_minimized: bool,
    /// Last time the Win32 minimize fallback ran; it is throttled to 4 Hz.
    minimize_checked_at: Instant,
    /// Repaint interval chosen for this frame, for `--stats`.
    last_interval_ms: u32,
    quit: bool,
    search: String,
    /// Debounced, lower-cased needle the list is actually filtered by.
    filter_needle: String,
    /// When the text box last changed; None means the filter is current.
    search_pending: Option<Instant>,
    focus_search: bool,
    selected: Option<u64>,
    /// Reused per-frame buffers for the board (see [`BoardScratch`]).
    board_scratch: BoardScratch,
    /// Newest last; the oldest entry falls off the front at `MAX_STATUS_ENTRIES`.
    statuses: VecDeque<StatusEntry>,
    show_log: bool,
    show_settings: bool,
    /// The rect the board (or the settings body) was painted into this frame.
    /// Modals centre on it, so one opened from Settings is not pulled left by
    /// the rail that is not on screen (B13).
    content_rect: Rect,
    capture: Option<CaptureState>,
    /// Global stop-all binding, parsed from settings.
    stop_all: Option<Hotkey>,
    renaming: Option<RenameState>,
    deleting: Option<DeleteLibraryState>,
    /// A validated config bundle awaiting confirmation (A5).
    importing: Option<ImportState>,
    /// "Reset all settings" confirmation is open (A6).
    confirm_reset: bool,
    drag: Option<DragState>,
    card_rects: Vec<(u64, Rect)>,
    row_rects: Vec<(u64, Rect)>,
    undo: Option<UndoDelete>,
    last_dir: Option<PathBuf>,
    next_id: u64,
    /// When the current page (tab / layout) started fading in.
    page_fade: Instant,
    /// When the settings modal was opened, used for the backdrop/rise tween.
    settings_opened: Instant,
    settings_was_open: bool,
    settings_tab: SettingsTab,
    settings_tab_fade: Instant,
    /// Theme ids/names for the Appearance dropdown; rebuilt only on reload.
    theme_choices: Vec<(String, String)>,
    playback_stats: PlaybackStats,
    dirty: bool,
    save_failed: bool,
    last_save: Instant,
    last_settings_json: String,
    last_libraries_json: String,
    /// `(playing, total)` the tray tooltip was last built for; the text is only
    /// formatted when this changes.
    last_tip: Option<(usize, usize)>,
    next_tip: Instant,
    meter_db: [f32; 2],
    meter_peak_db: [f32; 2],
    meter_peak_at: [Instant; 2],
    loop_state: LoopState,
    loop_missing: bool,
    loop_checked_at: Instant,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        loaded: storage::Loaded,
        open_settings: bool,
    ) -> Self {
        let storage::Loaded {
            settings,
            libraries: stored_libraries,
            warnings,
        } = loaded;
        let mut cfg = settings;

        // Motion preferences: user toggle AND the OS animation preference.
        anim::set_user(cfg.animate_ui);
        anim::set_os(anim::os_animations_enabled());

        let mut statuses = VecDeque::new();
        // Same cap/de-dup rule as `log()`: startup repeats collapse instead of
        // stacking, so the activity log reads the same however it was filled.
        let mut push_status = |kind: StatusKind, text: String| {
            record_status(&mut statuses, kind, text);
        };

        for warning in warnings {
            push_status(StatusKind::Warning, warning);
        }

        // Themes are read from config/themes/*.md before anything is drawn.
        for warning in theme::install(&cc.egui_ctx, &cfg.theme) {
            push_status(StatusKind::Warning, warning);
        }
        let theme_was_corrected = !theme::available().iter().any(|(id, _)| id == &cfg.theme);
        if theme_was_corrected {
            // `install` already fell back (and logged why); just persist the id in use.
            cfg.theme = "graphite".to_string();
        }
        // Read once: the Appearance dropdown draws from this list every frame.
        let theme_choices = theme::available();

        // The stop-all binding is stored as a label string like every other
        // hotkey; a hand-edited file must not take the app down, and a blocked
        // chord is never allowed (F5).
        let stop_all = match cfg.stop_all_hotkey.as_deref().map(parse_bindable) {
            Some(Ok(hotkey)) => Some(hotkey),
            Some(Err(err)) => {
                push_status(
                    StatusKind::Warning,
                    format!("Stop-all hotkey is invalid: {err}"),
                );
                cfg.stop_all_hotkey = None;
                None
            }
            None => None,
        };
        let (libraries, sounds, board_warnings) =
            build_board(&stored_libraries, cfg.preload_hotkey_sounds);
        for warning in board_warnings {
            push_status(StatusKind::Warning, warning);
        }

        let active_library = cfg.active_library;

        let engine = Arc::new(Engine::new(
            cfg.monitor_volume,
            if cfg.link_volumes {
                cfg.monitor_volume
            } else {
                cfg.cable_volume
            },
            cfg.master_gain_db,
        ));
        engine.set_loop_volume(cfg.loop_volume);
        engine.set_loop_outputs(cfg.loop_on_monitor, cfg.loop_on_cable);
        link_output_volumes(&mut cfg);
        let playback = Arc::new(Playback::new(engine.clone()));

        register_sounds(&playback, &sounds);

        let devices_result = wasapi::list_devices();
        let mut devices = Vec::new();
        let default_device = match devices_result {
            Ok(list) => {
                devices = list.devices;
                list.default_id
            }
            Err(err) => {
                push_status(
                    StatusKind::Error,
                    format!("Could not list audio devices: {err}"),
                );
                None
            }
        };

        // The one resolution rule: stored id -> friendly name -> Windows default
        // -> first device (Monitor), plus a "cable"/"virtual" device for Cable.
        let selection = resolve_outputs(&devices, default_device.as_deref(), &mut cfg);
        let (monitor, cable) = (selection.monitor, selection.cable);
        if let Some(note) = selection.note {
            push_status(StatusKind::Info, note);
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
            .unwrap_or_else(|_| (vec![0, 0, 0, 255], 1, 1));
        let tray_icon = icon::resize_rgba(&icon_rgba.0, icon_rgba.1, icon_rgba.2, 32);
        let hotkeys = HotkeyService::start(
            tray_icon,
            32,
            format!("{APP_NAME} - {} sounds", sounds.len()),
            playback.clone(),
            cc.egui_ctx.clone(),
        );

        let mut app = Self {
            cfg,
            sounds,
            libraries,
            active_library,
            playback,
            engine,
            hotkeys,
            devices,
            default_device,
            device_ready: [false, false],
            device_note: [None, None],
            tray_available: false,
            hidden: false,
            restored_at: since(10),
            was_minimized: false,
            minimize_checked_at: since(10),
            last_interval_ms: 0,
            quit: false,
            search: String::new(),
            filter_needle: String::new(),
            search_pending: None,
            focus_search: false,
            selected: None,
            board_scratch: BoardScratch::default(),
            statuses,
            show_log: false,
            show_settings: open_settings,
            content_rect: Rect::ZERO,
            capture: None,
            stop_all,
            renaming: None,
            deleting: None,
            importing: None,
            confirm_reset: false,
            drag: None,
            card_rects: Vec::new(),
            row_rects: Vec::new(),
            undo: None,
            last_dir: None,
            next_id: 1,
            // Starts long finished so the first frame is fully opaque.
            page_fade: since(10),
            settings_opened: since(10),
            settings_was_open: false,
            settings_tab: SettingsTab::Playback,
            settings_tab_fade: since(10),
            theme_choices,
            playback_stats: PlaybackStats::default(),
            dirty: theme_was_corrected,
            save_failed: false,
            last_save: Instant::now(),
            last_settings_json: String::new(),
            last_libraries_json: String::new(),
            last_tip: None,
            next_tip: Instant::now(),
            meter_db: [METER_FLOOR_DB; 2],
            meter_peak_db: [METER_FLOOR_DB; 2],
            meter_peak_at: [Instant::now(); 2],
            loop_state: LoopState::Idle,
            loop_missing: false,
            loop_checked_at: since(10),
        };

        // Fingerprints so a slider move never rewrites the library file.
        app.last_settings_json = storage::serialize_settings(&app.cfg).unwrap_or_default();
        let libraries = app.storage_libraries();
        app.last_libraries_json = storage::serialize_libraries(&libraries).unwrap_or_default();
        app.next_id = app.sounds.iter().map(|s| s.id).max().unwrap_or(0) + 1;

        app.sync_active_library();
        app.log(StatusKind::Info, "Ready");
        app
    }

    // ---------------------------------------------------------------------
    // Events
    // ---------------------------------------------------------------------

    fn poll_events(&mut self, ctx: &egui::Context) {
        for event in self.playback.poll_events() {
            // Every load/refusal/eviction changes what the Playback page reports.
            self.invalidate_playback_stats();
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
                                "Loaded \"{label}\" ({}, {})",
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
                PlaybackEvent::Refused { id, reason } => {
                    let label = self.sound_name(id);
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                        sound.ram = false;
                    }
                    // The refusal owns its wording (`policy::Refusal::message`);
                    // the board only supplies the name the user knows.
                    self.log(StatusKind::Warning, reason.message(&label));
                }
                PlaybackEvent::Warning(message) => {
                    self.log(StatusKind::Warning, message);
                }
            }
            ctx.request_repaint();
        }

        for event in self.engine.poll_events() {
            match event {
                AudioEvent::DeviceStarted { slot, .. } => {
                    self.device_ready[slot.index()] = true;
                    self.device_note[slot.index()] = self.device_note[slot.index()]
                        .take()
                        .and_then(DeviceNote::cleared_by_start);
                    ctx.request_repaint();
                }
                AudioEvent::DeviceError {
                    slot,
                    name,
                    message,
                } => {
                    self.device_ready[slot.index()] = false;
                    self.device_note[slot.index()] = Some(DeviceNote::failure(message.clone()));
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
                ServiceEvent::StopAll => {
                    // Audio has already stopped on the hotkey thread; this is
                    // only the visible acknowledgement.
                    self.log(StatusKind::Info, "Stopped all sounds");
                    ctx.request_repaint();
                }
                ServiceEvent::Pressed(id) => {
                    #[cfg(debug_assertions)]
                    eprintln!("[hotkey] pressed sound {id}");
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                        sound.flash_until = Some(Instant::now() + Duration::from_millis(420));
                    }
                    ctx.request_repaint();
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
            TrayCommand::Toggle => {
                if self.hidden || main_window_minimized() {
                    self.show_from_tray(ctx);
                } else {
                    self.hide_to_tray(ctx);
                }
            }
            TrayCommand::StopAll => {
                self.engine.stop_all();
                self.log(StatusKind::Info, "Stopped all sounds");
                ctx.request_repaint();
            }
            TrayCommand::Quit => {
                self.quit = true;
                self.save_now();
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn show_from_tray(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        self.restored_at = Instant::now();
        self.was_minimized = false;
        self.meter_db = [METER_FLOOR_DB; 2];
        self.meter_peak_db = [METER_FLOOR_DB; 2];
        crate::hotkeys::set_taskbar_tab(true);
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        // Viewport commands are applied after this frame, and winit can keep
        // reporting the window as minimized in the meantime. Restore it with
        // Win32 directly so the next frame sees a visible, unminimized window.
        crate::hotkeys::focus_main_window();
        ctx.request_repaint();
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) {
            self.quit = true;
            self.save_now();
        }
    }

    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        self.save_now();
        self.hidden = true;
        // Minimize instead of hiding: winit stops delivering paint events to a
        // hidden HWND, which leaves eframe spinning in `ControlFlow::Poll` at a
        // full core and freezes every tray/UI event. A minimized window keeps
        // receiving frames, so events still flow and the idle timer applies.
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        crate::hotkeys::set_taskbar_tab(false);
        self.log(StatusKind::Info, "Minimized to tray - hotkeys still work");
    }

    /// Hides to the tray only when the window actually becomes minimized.
    ///
    /// A level check is wrong here: after a restore winit can still report
    /// `minimized = true` for a frame, so the old code hid the window again
    /// immediately and the tray icon appeared dead.
    fn handle_os_minimize(&mut self, ctx: &egui::Context) {
        let mut minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));

        // Fallback for the custom title bar, where winit may miss the OS
        // minimize. EnumWindows is not free, so this runs at most 4x/second.
        let now = Instant::now();
        if !minimized
            && !self.hidden
            && self.cfg.minimize_to_tray
            && self.tray_available
            && now.duration_since(self.minimize_checked_at) > Duration::from_millis(250)
        {
            self.minimize_checked_at = now;
            minimized = main_window_minimized();
        }

        let became_minimized = minimized && !self.was_minimized;
        self.was_minimized = minimized;

        if became_minimized
            && !self.hidden
            && self.cfg.minimize_to_tray
            && self.tray_available
            && self.restored_at.elapsed() > Duration::from_millis(400)
        {
            self.hide_to_tray(ctx);
        }
    }

    // ---------------------------------------------------------------------
    // Commands
    // ---------------------------------------------------------------------

    fn handle_actions(&mut self, actions: &mut Vec<Action>, ctx: &egui::Context) {
        // An empty list means nothing changed. Requesting a frame here is what
        // kept the board at 60 fps (and a hidden window at 100% of a core).
        if actions.is_empty() {
            return;
        }
        for action in actions.drain(..) {
            match action {
                Action::Play(id) => self.play_sound(id),
                Action::Stop(id) => {
                    self.playback.stop_sound(id);
                }
                Action::Remove(id) => self.remove_sound(id),
                Action::Rename(id) => {
                    if let Some(sound) = self.sounds.iter().find(|s| s.id == id) {
                        self.renaming = Some(RenameState {
                            target: RenameTarget::Sound(id),
                            buffer: sound.name.clone(),
                            focus: true,
                        });
                    }
                }
                Action::SetHotkey(id) => {
                    if let Some(sound) = self.sounds.iter().find(|s| s.id == id) {
                        self.capture = Some(CaptureState {
                            target: CaptureTarget::Sound(id),
                            current: sound.hotkey,
                        });
                    }
                }
                Action::ClearHotkey(id) => {
                    if self.clear_sound_hotkey(id).is_some() {
                        self.sync_active_library();
                        self.mark_dirty();
                    }
                }
                Action::ToggleRam(id) => self.toggle_sound_ram(id),
                Action::MoveSound(id, library_id) => {
                    self.move_sound_to_library(id, library_id);
                }
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
                Action::Gain(id, gain_db) => {
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                        sound.gain_db = gain_db.clamp(SOUND_GAIN_MIN_DB, SOUND_GAIN_MAX_DB);
                        // Same live path as a volume move: a sounding voice takes
                        // the new product immediately.
                        self.playback.set_gain(id, sound.gain_db);
                        self.mark_dirty();
                    }
                }
                Action::DragStart { id, size } => {
                    if self.search.trim().is_empty() {
                        if let Some(sound) = self.sounds.iter().find(|s| s.id == id) {
                            self.drag = Some(DragState {
                                sound_id: id,
                                size,
                                name: sound.name.clone(),
                                hotkey: sound.hotkey.map(|h| h.label()).unwrap_or_default(),
                            });
                        }
                    }
                }
                Action::DragStop => {
                    if let Some(drag) = self.drag.take() {
                        let pointer = ctx.input(|i| i.pointer.interact_pos());
                        if let Some(pointer) = pointer {
                            // Rects carry ids, so a list that shifted since the
                            // drag started still resolves to the right sound.
                            let target = drop_target(&self.card_rects, pointer)
                                .or_else(|| drop_target(&self.row_rects, pointer));
                            if let Some(target) = target {
                                if move_sound_to_id(&mut self.sounds, drag.sound_id, target) {
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
        if self.playback.info(id).missing {
            self.log_missing_sound(id);
            return;
        }
        self.playback.play(id);
    }

    /// A missing file is reported, never silently played and never a surprise
    /// file dialog: relinking is an explicit menu action (FR-5.3.6).
    fn log_missing_sound(&mut self, id: u64) {
        let label = self.sound_name(id);
        self.log(
            StatusKind::Error,
            format!("\"{label}\": file missing - right-click > Locate"),
        );
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
        self.undo = Some(UndoDelete { sound, index });
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
        let id = restore_sound(&mut self.sounds, &self.playback, undo);
        self.selected = Some(id);
        self.sync_active_library();
        self.mark_dirty();
        self.log(
            StatusKind::Success,
            format!("Restored \"{}\"", self.sound_name(id)),
        );
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

    // ---------------------------------------------------------------------
    // Loop track
    // ---------------------------------------------------------------------

    /// Re-check the loop file at most every two seconds. A cheap metadata call,
    /// enough to notice a file appearing or disappearing between actions.
    fn refresh_loop_missing(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.loop_checked_at) < Duration::from_secs(2) {
            return;
        }
        self.loop_checked_at = now;
        self.loop_missing = self.cfg.loop_enabled
            && self
                .cfg
                .loop_path
                .as_deref()
                .map(|path| !model::resolve_path(path).is_file())
                .unwrap_or(false);
    }

    fn pick_loop_file(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Audio files", SUPPORTED_EXTENSIONS)
            .add_filter("All files", &["*"])
            .set_title("Choose a looping track");
        if let Some(stored) = self.cfg.loop_path.as_deref() {
            let current = model::resolve_path(stored);
            if let Some(parent) = current.parent().filter(|parent| parent.exists()) {
                dialog = dialog.set_directory(parent);
            }
        } else if let Some(dir) = self.last_dir.clone() {
            dialog = dialog.set_directory(dir);
        }

        let Some(path) = dialog.pick_file() else {
            return;
        };
        if let Some(parent) = path.parent() {
            self.last_dir = Some(parent.to_path_buf());
        }

        let stored = model::portable_path(&path);
        if self.cfg.loop_path.as_deref() == Some(stored.as_str()) {
            return;
        }
        let was_active = self.loop_state.is_active();
        let name = display_name(&path);
        self.cfg.loop_path = Some(stored);
        self.mark_dirty();
        self.loop_missing = false;
        self.loop_checked_at = since(10);

        if was_active {
            match self.engine.start_loop(path) {
                Ok(()) => self.log(StatusKind::Info, format!("Loop track: {name}")),
                Err(err) => self.log(StatusKind::Error, format!("Loop track: {err}")),
            }
        }
        self.loop_state = self.engine.loop_state();
        self.refresh_loop_missing(true);
    }

    fn toggle_loop(&mut self) {
        if self.loop_state.is_active() {
            self.engine.stop_loop();
            self.loop_state = LoopState::Idle;
            self.log(StatusKind::Info, "Loop track stopped");
            return;
        }

        let Some(stored) = self.cfg.loop_path.clone() else {
            return;
        };
        let path = model::resolve_path(&stored);
        let name = display_name(&path);
        match self.engine.start_loop(path) {
            Ok(()) => {
                self.loop_state = self.engine.loop_state();
                self.log(StatusKind::Info, format!("Loop track: {name}"));
            }
            Err(err) => {
                self.log(StatusKind::Error, format!("Loop track: {err}"));
            }
        }
        self.refresh_loop_missing(true);
    }

    fn set_loop_volume(&mut self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        if (self.cfg.loop_volume - volume).abs() < 1e-4 {
            return;
        }
        self.cfg.loop_volume = volume;
        self.engine.set_loop_volume(volume);
        self.mark_dirty();
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
            // Search matches the name and the *file* name; a relink must not keep
            // the old path's key (or the old tooltip).
            sound.refresh_labels();
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

        // One canonical key per sound already on the board, then one per
        // candidate: the naive loop re-canonicalized every existing path once
        // per dropped file, which is O(files x library) syscalls.
        let mut claimed: HashSet<String> = self
            .sounds
            .iter()
            .map(|sound| canonical_key(&sound.path))
            .collect();
        // One running id for the whole batch instead of a full scan per file.
        let mut next_id = free_sound_id(&self.sounds, self.next_id);

        for path in expanded {
            if !path.is_file() || !is_supported_file(&path) {
                unsupported += 1;
                continue;
            }
            if !claim_unique(&mut claimed, &path) {
                skipped += 1;
                continue;
            }

            let id = next_id;
            next_id += 1;
            let volume = 1.0;
            let name = display_name(&path);
            let mut sound = Sound {
                id,
                name,
                path: path.clone(),
                hotkey: None,
                volume,
                gain_db: 0.0,
                ram: false,
                library_id: self.active_library,
                search_key: String::new(),
                hotkey_label: None,
                path_label: String::new(),
                flash_until: None,
            };
            sound.refresh_labels();
            self.playback.register(id, path, volume, 0.0);
            self.sounds.push(sound);
            added += 1;
            last_id = Some(id);
        }

        if added > 0 {
            self.next_id = next_id;
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
                        format!(
                            " - skipped {skipped} duplicate{}",
                            if skipped == 1 { "" } else { "s" }
                        )
                    } else {
                        String::new()
                    },
                    if unsupported > 0 {
                        format!(
                            " - ignored {unsupported} unsupported file{}",
                            if unsupported == 1 { "" } else { "s" }
                        )
                    } else {
                        String::new()
                    }
                ),
            );
        } else if skipped > 0 {
            self.log(
                StatusKind::Warning,
                format!(
                    "Already in your library ({skipped} duplicate{})",
                    if skipped == 1 { "" } else { "s" }
                ),
            );
        } else if unsupported > 0 {
            self.log(
                StatusKind::Error,
                format!("No supported audio files found ({unsupported} ignored)"),
            );
        }
    }

    /// The active library's sounds in tab order, in the shape the chord rules
    /// want. Only the active library is registered (FR-5.7.7), so only it can
    /// conflict.
    fn chord_sounds(&self) -> impl Iterator<Item = (u64, Option<Hotkey>)> + '_ {
        self.sounds
            .iter()
            .filter(|sound| sound.library_id == self.active_library)
            .map(|sound| (sound.id, sound.hotkey))
    }

    /// The stop-all chord that is actually live, if the feature is enabled.
    fn stop_all_chord(&self) -> Option<Hotkey> {
        if self.cfg.stop_all_hotkey_enabled {
            self.stop_all
        } else {
            None
        }
    }

    /// How a chord conflict is worded in the capture dialog and the log.
    fn owner_label(&self, owner: ChordOwner) -> String {
        match owner {
            ChordOwner::StopAll => "Stop all".to_string(),
            ChordOwner::Sound(id) => format!("\"{}\"", self.sound_name(id)),
        }
    }

    fn sync_hotkeys(&mut self) {
        // One owner for the duplicate rules (FR-5.7.5): the stop-all chord beats
        // sounds and the first sound in tab order beats later ones. Every loser is
        // unbound like a manual clear, logged, and the change is persisted.
        let mut changed = false;
        for id in duplicate_sounds(self.chord_sounds(), self.stop_all_chord()) {
            if let Some((name, hotkey)) = self.clear_sound_hotkey(id) {
                self.log(
                    StatusKind::Warning,
                    format!(
                        "Hotkey {} is already in use; cleared for \"{name}\"",
                        hotkey.label()
                    ),
                );
                changed = true;
            }
        }
        let bindings: Vec<(u64, Hotkey)> = self
            .chord_sounds()
            .filter_map(|(id, hotkey)| hotkey.map(|hotkey| (id, hotkey)))
            .collect();
        self.hotkeys.set_stop_all(self.stop_all_chord());
        self.hotkeys.set_bindings(bindings);
        if changed {
            self.mark_dirty();
        }
    }

    /// Drops a sound's hotkey and returns it with the sound's name, so the caller
    /// can explain the change. The one unbind path (FR-5.3.5): RAM off, not
    /// retained, no stale error.
    fn clear_sound_hotkey(&mut self, id: u64) -> Option<(String, Hotkey)> {
        let sound = self.sounds.iter_mut().find(|sound| sound.id == id)?;
        let hotkey = unbind(sound)?;
        let name = sound.name.clone();
        self.playback.set_retain(id, false);
        Some((name, hotkey))
    }

    /// Only the active library is live: its hotkeys are registered and only its
    /// hotkey sounds may stay in RAM. Switching tabs swaps the whole set.
    fn sync_active_library(&mut self) {
        self.sync_hotkeys();
        for sound in &self.sounds {
            self.playback.ensure_lock(sound.id);
            let keep = ram_eligible(sound, self.active_library) && sound.ram;
            self.playback.set_retain(sound.id, keep);
            if keep {
                self.playback.ensure_loaded(sound.id);
            }
        }
        self.invalidate_playback_stats();
    }

    /// The Playback page's RAM state. Cached because the same refresh also
    /// locks the playback registry for the cache line; invalidated by
    /// [`App::invalidate_playback_stats`] rather than recomputed per frame.
    fn playback_stats(&mut self) -> Option<RamState> {
        if !self.playback_stats.fresh {
            let usage = self.playback.cache_usage();
            self.playback_stats.ram = self.ram_summary();
            self.playback_stats.usage = usage;
            self.playback_stats.label = format!(
                "{} of {} MB - {} of {} sounds",
                usage.bytes / (1024 * 1024),
                policy::RETAINED_BYTES_CAP / (1024 * 1024),
                usage.sounds,
                policy::RETAINED_SOUND_CAP
            );
            self.playback_stats.fresh = true;
        }
        self.playback_stats.ram
    }

    /// Call from anything that changes RAM eligibility, retention or the decoded
    /// cache: RAM toggles, tab switches, playback events, opening the screen.
    fn invalidate_playback_stats(&mut self) {
        self.playback_stats.fresh = false;
    }

    fn ram_summary(&self) -> Option<RamState> {
        let mut eligible = 0usize;
        let mut on = 0usize;
        for sound in &self.sounds {
            if !ram_eligible(sound, self.active_library) {
                continue;
            }
            eligible += 1;
            if sound.ram {
                on += 1;
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
        let active = self.active_library;
        let mut touched = Vec::new();
        let mut changed = false;
        for sound in &mut self.sounds {
            // Inactive libraries and hotkey-less sounds are never RAM-eligible;
            // they keep following the sync path instead of being retained here.
            if !ram_eligible(sound, active) {
                continue;
            }
            if sound.ram != on {
                sound.ram = on;
                changed = true;
            }
            touched.push(sound.id);
        }
        for id in touched {
            self.playback.set_retain(id, on);
            if on {
                self.playback.ensure_loaded(id);
            }
        }
        if changed || self.cfg.preload_hotkey_sounds != on {
            self.cfg.preload_hotkey_sounds = on;
            self.mark_dirty();
        }
        self.invalidate_playback_stats();
    }

    fn toggle_sound_ram(&mut self, id: u64) {
        let active = self.active_library;
        let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if !ram_eligible(sound, active) {
            return;
        }
        sound.ram = !sound.ram;
        let ram = sound.ram;
        self.playback.set_retain(id, ram);
        if ram {
            self.playback.ensure_loaded(id);
        }
        self.mark_dirty();
        self.invalidate_playback_stats();
    }

    fn toggle_layout(&mut self) {
        self.cfg.layout = match self.cfg.layout {
            SoundLayout::Grid => SoundLayout::List,
            SoundLayout::List => SoundLayout::Grid,
        };
        self.page_fade = Instant::now();
        self.mark_dirty();
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Snapshot the runtime tabs/sounds into their storage form.
    fn storage_libraries(&self) -> Vec<storage::LibraryConfig> {
        self.libraries
            .iter()
            .map(|library| storage::LibraryConfig {
                id: library.id,
                name: library.name.clone(),
                sounds: self
                    .sounds
                    .iter()
                    .filter(|sound| sound.library_id == library.id)
                    .map(|sound| storage::SoundConfig {
                        id: sound.id,
                        name: sound.name.clone(),
                        path: model::portable_path(&sound.path),
                        hotkey: sound.hotkey.map(|h| h.label()),
                        volume: sound.volume,
                        gain_db: sound.gain_db,
                        ram: if sound.hotkey.is_some() {
                            Some(sound.ram)
                        } else {
                            None
                        },
                    })
                    .collect(),
            })
            .collect()
    }

    /// Write only the files whose content actually changed.
    fn persist(&mut self) -> bool {
        self.write_back_runtime_settings();
        let base = model::app_dir();
        let mut ok = true;

        let settings_json = match storage::serialize_settings(&self.cfg) {
            Ok(json) => json,
            Err(err) => {
                if !self.save_failed {
                    self.save_failed = true;
                    self.log(
                        StatusKind::Warning,
                        format!("Could not serialize settings: {err}"),
                    );
                }
                return false;
            }
        };
        if settings_json != self.last_settings_json {
            let path = storage::settings_path(&base);
            match storage::io::write_json_atomic(&path, &settings_json) {
                Ok(()) => self.last_settings_json = settings_json,
                Err(err) => {
                    ok = false;
                    if !self.save_failed {
                        self.save_failed = true;
                        self.log(
                            StatusKind::Warning,
                            format!("Could not save config/settings.json: {err}"),
                        );
                    }
                }
            }
        }

        let libraries = self.storage_libraries();
        let libraries_json = match storage::serialize_libraries(&libraries) {
            Ok(json) => json,
            Err(err) => {
                if !self.save_failed {
                    self.save_failed = true;
                    self.log(
                        StatusKind::Warning,
                        format!("Could not serialize libraries: {err}"),
                    );
                }
                return false;
            }
        };
        if libraries_json != self.last_libraries_json {
            let path = storage::libraries_path(&base);
            match storage::io::write_json_atomic(&path, &libraries_json) {
                Ok(()) => self.last_libraries_json = libraries_json,
                Err(err) => {
                    ok = false;
                    if !self.save_failed {
                        self.save_failed = true;
                        self.log(
                            StatusKind::Warning,
                            format!("Could not save config/libraries.json: {err}"),
                        );
                    }
                }
            }
        }

        ok
    }

    fn save_now(&mut self) {
        self.last_save = Instant::now();
        if self.persist() {
            self.dirty = false;
            self.save_failed = false;
        }
    }

    /// The two settings that live on `App` until a save: the active tab and the
    /// stop-all label. `persist` and the config export both stamp them, so a
    /// bundle carries what is actually on screen.
    fn write_back_runtime_settings(&mut self) {
        self.cfg.active_library = self.active_library;
        self.cfg.stop_all_hotkey = self.stop_all.map(|hotkey| hotkey.label());
    }

    // ---------------------------------------------------------------------
    // Config data (export / import / reset)
    // ---------------------------------------------------------------------

    /// Write settings + libraries to one versioned JSON bundle. Audio files are
    /// references and are never copied (FR-5.3.14).
    fn export_config(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("SoundDeck configuration", &["json"])
            .set_title("Export configuration")
            .set_file_name(DEFAULT_BUNDLE_NAME)
            .set_directory(model::app_dir());
        let Some(path) = dialog.save_file() else {
            return;
        };

        self.write_back_runtime_settings();
        let libraries = self.storage_libraries();
        let result = storage::bundle::to_json(&self.cfg, &libraries)
            .and_then(|json| storage::io::write_text_atomic(&path, &json));
        match result {
            Ok(()) => self.log(
                StatusKind::Success,
                format!(
                    "Exported configuration to {}",
                    storage::io::file_label(&path)
                ),
            ),
            Err(err) => self.log(StatusKind::Error, format!("Export failed: {err}")),
        }
    }

    /// Pick a bundle, validate it and open the confirmation. A rejected file is
    /// only a log line: nothing on the board changes.
    fn import_config_dialog(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("SoundDeck configuration", &["json"])
            .add_filter("All files", &["*"])
            .set_title("Import configuration")
            .set_directory(model::app_dir());
        let Some(path) = dialog.pick_file() else {
            return;
        };

        match storage::bundle::read(&path) {
            Ok(bundle) => {
                let sounds = bundle
                    .libraries
                    .iter()
                    .map(|library| library.sounds.len())
                    .sum();
                let libraries = bundle.libraries.len();
                self.importing = Some(ImportState {
                    name: storage::io::file_label(&path),
                    bundle,
                    sounds,
                    libraries,
                });
            }
            Err(err) => self.log(StatusKind::Error, err.message()),
        }
    }

    /// The import confirmation. Nothing is applied until Replace is pressed;
    /// closing (X, Esc, backdrop, Cancel) drops the bundle.
    fn import_modal(&mut self, ctx: &egui::Context) {
        let Some(ImportState {
            name,
            bundle,
            sounds,
            libraries,
        }) = self.importing.take()
        else {
            return;
        };
        let mut close = ctx.input(|input| input.key_pressed(Key::Escape));
        let mut replace = false;

        let content = self.content_rect;
        modal_shell(
            ctx,
            content,
            Id::new("import_modal"),
            CONFIRM_MODAL_WIDTH,
            MODAL_MARGIN,
            &mut close,
            |ui, dismiss| {
                modal_header(ui, "Import configuration", Id::new("import_close"), dismiss);
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Replace current board? {} sound{} across {} librar{} will be replaced.",
                        sounds,
                        if sounds == 1 { "" } else { "s" },
                        libraries,
                        if libraries == 1 { "y" } else { "ies" }
                    ))
                    .size(11.5)
                    .color(theme::text()),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "From {name}. Audio files are not copied or deleted."
                    ))
                    .size(10.5)
                    .color(theme::faint()),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if accent_button(ui, "Replace", Vec2::new(100.0, 32.0)).clicked() {
                        replace = true;
                    }
                    if ghost_button(ui, "Cancel", Vec2::new(90.0, 32.0)).clicked() {
                        *dismiss = true;
                    }
                });
            },
        );

        if replace {
            self.apply_import(ctx, bundle);
            return;
        }
        if !close {
            self.importing = Some(ImportState {
                name,
                bundle,
                sounds,
                libraries,
            });
        }
    }

    /// Swap the whole board for an imported bundle: stop audio, retire the old
    /// registry entries, rebuild from the bundle's storage shapes, apply the
    /// settings and save. Audio files are untouched (FR-5.3.14).
    fn apply_import(&mut self, ctx: &egui::Context, bundle: storage::bundle::Bundle) {
        let storage::bundle::Bundle {
            settings,
            libraries,
        } = bundle;

        self.engine.stop_all();
        self.engine.stop_loop();
        self.loop_state = LoopState::Idle;

        let retired: Vec<u64> = self.sounds.iter().map(|sound| sound.id).collect();
        for id in retired {
            self.playback.remove(id);
        }

        self.cfg = settings;
        let (new_libraries, new_sounds, warnings) =
            build_board(&libraries, self.cfg.preload_hotkey_sounds);
        for warning in warnings {
            self.log(StatusKind::Warning, warning);
        }
        self.libraries = new_libraries;
        self.sounds = new_sounds;
        register_sounds(&self.playback, &self.sounds);

        self.active_library = self.cfg.active_library;
        self.next_id = self.sounds.iter().map(|sound| sound.id).max().unwrap_or(0) + 1;
        self.selected = None;
        self.undo = None;
        self.drag = None;
        self.search.clear();
        self.filter_needle.clear();
        self.search_pending = None;

        let (sound_count, library_count) = (self.sounds.len(), self.libraries.len());
        self.apply_config(ctx);
        self.save_now();
        self.log(
            StatusKind::Success,
            format!(
                "Imported {sound_count} sound{} across {library_count} librar{}",
                if sound_count == 1 { "" } else { "s" },
                if library_count == 1 { "y" } else { "ies" }
            ),
        );
    }

    /// The "Reset all settings" confirmation. Libraries, sounds and audio files
    /// are never part of it.
    fn reset_modal(&mut self, ctx: &egui::Context) {
        if !self.confirm_reset {
            return;
        }
        let mut close = ctx.input(|input| input.key_pressed(Key::Escape));
        let mut confirm = false;

        let content = self.content_rect;
        modal_shell(
            ctx,
            content,
            Id::new("reset_settings_modal"),
            CONFIRM_MODAL_WIDTH,
            MODAL_MARGIN,
            &mut close,
            |ui, dismiss| {
                modal_header(
                    ui,
                    "Reset all settings",
                    Id::new("reset_settings_close"),
                    dismiss,
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Reset all settings? Libraries and sounds are not affected.")
                        .size(11.5)
                        .color(theme::text()),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ghost_button_colored(ui, "Reset all", Vec2::new(110.0, 32.0), theme::red())
                        .clicked()
                    {
                        confirm = true;
                    }
                    if ghost_button(ui, "Cancel", Vec2::new(90.0, 32.0)).clicked() {
                        *dismiss = true;
                    }
                });
            },
        );

        if confirm {
            self.confirm_reset = false;
            self.reset_all_settings(ctx);
            return;
        }
        if close {
            self.confirm_reset = false;
        }
    }

    /// Restore one settings page to its defaults and re-apply the side effects.
    /// No confirmation (a page reset is small and repeatable) and no effect on
    /// libraries or sounds.
    fn reset_settings_group(&mut self, ctx: &egui::Context, group: storage::SettingsGroup) {
        self.cfg.reset_group(group);
        self.apply_config(ctx);
        self.log(
            StatusKind::Info,
            format!("{} settings reset to defaults", group.title()),
        );
    }

    /// Every settings field back to its default. The config is saved at once so a
    /// crash right after cannot resurrect the old values.
    fn reset_all_settings(&mut self, ctx: &egui::Context) {
        self.cfg.reset_all();
        // The active tab is a settings field too: keep it on a tab that exists.
        let ids: Vec<u64> = self.libraries.iter().map(|library| library.id).collect();
        self.cfg.clamp_active_library(&ids);
        if self.active_library != self.cfg.active_library {
            self.engine.stop_all();
            self.active_library = self.cfg.active_library;
            self.page_fade = Instant::now();
        }
        self.apply_config(ctx);
        self.save_now();
        self.log(StatusKind::Success, "All settings reset to defaults");
    }

    /// Push every `Settings` field into the runtime: motion, theme, output levels
    /// and gain, loop routing, endpoint resolution and the bound hotkeys. Used
    /// after an import and after a reset so both behave like a fresh start.
    ///
    /// The loop is never started here: a restart starts it stopped too.
    fn apply_config(&mut self, ctx: &egui::Context) {
        anim::set_user(self.cfg.animate_ui);

        // An unknown theme id falls back to Graphite, exactly as on load. Resolve
        // first so the fallback is logged instead of silently swapping the theme.
        for warning in theme::set_theme(&self.cfg.theme) {
            self.log(StatusKind::Warning, warning);
        }
        if !self
            .theme_choices
            .iter()
            .any(|(id, _)| id == &self.cfg.theme)
        {
            self.cfg.theme = "graphite".to_string();
        }
        theme::apply_style(ctx);

        link_output_volumes(&mut self.cfg);
        self.engine
            .set_master(Slot::Monitor, self.cfg.monitor_volume);
        self.engine.set_master(Slot::Cable, self.cfg.cable_volume);
        self.engine.set_master_gain_db(self.cfg.master_gain_db);
        self.engine.set_loop_volume(self.cfg.loop_volume);
        self.engine
            .set_loop_outputs(self.cfg.loop_on_monitor, self.cfg.loop_on_cable);
        if !self.cfg.loop_enabled {
            self.engine.stop_loop();
            self.loop_state = LoopState::Idle;
        }
        self.loop_checked_at = since(10);
        self.refresh_loop_missing(true);

        let selection =
            resolve_outputs(&self.devices, self.default_device.as_deref(), &mut self.cfg);
        if let Some(note) = selection.note {
            self.log(StatusKind::Info, note);
        }
        self.apply_routes();

        // Stop-all is stored as a label like every other hotkey, and a blocked
        // chord is refused exactly as on load.
        self.stop_all = match self.cfg.stop_all_hotkey.as_deref().map(parse_bindable) {
            Some(Ok(hotkey)) => Some(hotkey),
            Some(Err(err)) => {
                self.log(
                    StatusKind::Warning,
                    format!("Stop-all hotkey is invalid: {err}"),
                );
                self.cfg.stop_all_hotkey = None;
                None
            }
            None => None,
        };

        self.invalidate_playback_stats();
        self.sync_active_library();
        self.mark_dirty();
    }

    fn log(&mut self, kind: StatusKind, text: impl Into<String>) {
        let text = text.into();
        #[cfg(debug_assertions)]
        eprintln!("[log] {text}");
        record_status(&mut self.statuses, kind, text);
    }

    fn sound_name(&self, id: u64) -> String {
        self.sounds
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Unknown sound".to_string())
    }

    fn library_name(&self, id: u64) -> String {
        self.libraries
            .iter()
            .find(|library| library.id == id)
            .map(|library| library.name.clone())
            .unwrap_or_else(|| "Unknown library".to_string())
    }

    // ---------------------------------------------------------------------
    // Devices
    // ---------------------------------------------------------------------

    fn refresh_devices(&mut self) {
        match wasapi::list_devices() {
            Ok(list) => {
                self.devices = list.devices;
                self.default_device = list.default_id;
                // Stored endpoints are re-resolved by id then name so a device
                // that came back under a new id is picked up again, and the
                // stored labels follow a device Windows has renamed.
                let monitor = self.retune_slot(Slot::Monitor);
                let cable = self.retune_slot(Slot::Cable);
                if monitor || cable {
                    self.mark_dirty();
                }
                self.apply_routes();
                self.log(
                    StatusKind::Info,
                    format!("Found {} output device(s)", self.devices.len()),
                );
            }
            Err(err) => self.log(StatusKind::Error, format!("Device refresh failed: {err}")),
        }
    }

    /// Point one slot's stored endpoint at the device it resolves to now.
    ///
    /// Returns whether anything changed (the caller then saves). The id is only
    /// replaced when the stored one no longer exists — a name match on a
    /// re-plugged device — and a renamed device keeps its id and takes the new
    /// label, so the sidebar and Settings stop showing a stale name.
    fn retune_slot(&mut self, slot: Slot) -> bool {
        let (id, name) = slot_endpoint(&self.cfg, slot);
        let Some(found) = resolve_device(&self.devices, id.as_deref(), name.as_deref()) else {
            return false;
        };
        let unchanged = id.as_deref() == Some(found.id.as_str())
            && name.as_deref() == Some(found.name.as_str());
        if unchanged {
            return false;
        }
        set_slot_endpoint(&mut self.cfg, slot, Some(found.id), Some(found.name));
        true
    }

    fn set_device(&mut self, slot: Slot, id: Option<String>) {
        let silent = id.is_none();
        let name = id.as_ref().and_then(|id| {
            self.devices
                .iter()
                .find(|d| &d.id == id)
                .map(|d| d.name.clone())
        });
        // Picking "None (silent)" stores the choice, not an empty field, so it
        // survives a restart instead of being re-resolved to a device.
        let name = name.or_else(|| silent.then(|| SILENT_DEVICE_LABEL.to_string()));
        set_slot_endpoint(&mut self.cfg, slot, id, name);
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
        let monitor = route_slot(
            &self.devices,
            self.default_device.as_deref(),
            Slot::Monitor,
            &self.cfg,
            None,
        );
        let cable = route_slot(
            &self.devices,
            self.default_device.as_deref(),
            Slot::Cable,
            &self.cfg,
            monitor.device.as_ref().map(|device| device.id.as_str()),
        );

        // A substitution is user-visible, not a silent fix-up: it is logged and
        // it is what Settings shows for that slot until the device comes back.
        for (slot, route) in [(Slot::Monitor, &monitor), (Slot::Cable, &cable)] {
            let note = match &route.substituted {
                Some(text) => {
                    self.log(StatusKind::Warning, format!("{}: {text}", slot.label()));
                    Some(DeviceNote::warning(text.clone()))
                }
                None => None,
            };
            self.device_note[slot.index()] = note;
        }

        self.engine.set_routes(
            monitor.device.map(|d| DeviceRoute {
                id: d.id,
                name: d.name,
            }),
            cable.device.map(|d| DeviceRoute {
                id: d.id,
                name: d.name,
            }),
        );
        self.engine
            .set_master(Slot::Monitor, self.cfg.monitor_volume);
        self.engine.set_master(Slot::Cable, self.cfg.cable_volume);
        // A route the engine did not need to restart emits no event, so read the
        // real state instead of clearing the status and waiting for one.
        for slot in [Slot::Monitor, Slot::Cable] {
            self.device_ready[slot.index()] = self.engine.device_running(slot);
        }
    }

    // ---------------------------------------------------------------------
    // UI
    // ---------------------------------------------------------------------

    fn title_bar(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let bar_rect = ui.max_rect();
        let drag = ui.interact(bar_rect, Id::new("titlebar_drag"), Sense::click_and_drag());
        let mut tabs_rect = Rect::NOTHING;

        ui.horizontal_centered(|ui| {
            let (logo_rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            paint_logo(ui.painter(), logo_rect);
            ui.add_space(2.0);
            ui.label(
                RichText::new(APP_NAME)
                    .font(theme::medium(20.0))
                    .color(theme::text()),
            );

            // Browser-style tabs live in the title bar, after the app name.
            let buttons_width = 116.0;
            let tabs_width = (ui.available_width() - buttons_width - 10.0).max(90.0);
            ui.add_space(16.0);
            let tabs = ui.allocate_ui_with_layout(
                Vec2::new(tabs_width, 30.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    if self.show_settings {
                        ui.label(RichText::new("Settings").size(13.0).color(theme::muted()));
                    } else {
                        self.library_bar(ui, tabs_width);
                    }
                },
            );
            tabs_rect = tabs.response.rect;

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if window_button(ui, WindowButton::Close, self.tray_available) {
                    self.quit = true;
                    self.save_now();
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                if window_button(ui, WindowButton::Maximize, self.tray_available) {
                    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                if window_button(ui, WindowButton::Minimize, self.tray_available) {
                    if self.cfg.minimize_to_tray && self.tray_available {
                        self.hide_to_tray(ctx);
                    } else {
                        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                    }
                }
            });
            ui.add_space(6.0);
        });

        // Dragging or double-clicking a tab must not move/maximize the window.
        let over_tabs = ctx
            .pointer_interact_pos()
            .map(|pos| tabs_rect.contains(pos))
            .unwrap_or(false);
        if drag.drag_started() && !over_tabs {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if drag.double_clicked() && !over_tabs {
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }
    }

    fn side_panel(&mut self, ui: &mut Ui) {
        // The window starts at 960x540, so the sidebar needs a tighter rhythm
        // on short windows or the pinned footer ends up below the fold.
        let compact = ui.available_height() < 620.0;
        let gap = if compact { 0.35 } else { 1.0 };
        // On the 960x540 default size the device captions would eat the room the
        // pinned footer needs; they reappear when the window is taller.
        let hide_device_labels = compact;
        // Pinned footer: the Settings button, the peak meter and the credit.
        // Devices, gain, stop-all and loop all live in the scroll area above it,
        // so a short window scrolls instead of hiding a widget.
        let meter_reserve = if compact { 36.0 } else { 49.0 };
        let reserve = (if compact { 44.0 } else { 84.0 }) + meter_reserve;
        let content_height = (ui.available_height() - reserve).max(120.0);
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
                        ui.add_space(4.0 * gap);
                        self.layout_selector(ui);

                        ui.add_space(6.0 * gap);
                        section_line(ui);
                        ui.add_space(4.0 * gap);

                        if !hide_device_labels {
                            ui.label(RichText::new("Monitor").size(12.5).color(theme::muted()));
                        }
                        self.device_selector(ui, Slot::Monitor);
                        ui.add_space(2.0);
                        self.device_volume(ui, Slot::Monitor);

                        ui.add_space(2.0 * gap);
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

                        ui.add_space(2.0 * gap);
                        if !hide_device_labels {
                            ui.label(RichText::new("Cable").size(12.5).color(theme::muted()));
                        }
                        self.device_selector(ui, Slot::Cable);
                        ui.add_space(2.0);
                        self.device_volume(ui, Slot::Cable);

                        if self.cfg.show_gain_slider {
                            ui.add_space(4.0 * gap);
                            let mut gain_db = self.cfg.master_gain_db;
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Gain").size(11.5).color(theme::muted()));
                                // Readout on the right, slider filling what is left
                                // (same rule as the volume rows).
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(format_gain_db(gain_db))
                                            .size(10.5)
                                            .color(theme::muted())
                                            .monospace(),
                                    );
                                    ui.spacing_mut().slider_width = ui.available_width();
                                    let response = ui.add(
                                        egui::Slider::new(&mut gain_db, MASTER_GAIN_RANGE)
                                            .step_by(0.5)
                                            .show_value(false)
                                            .trailing_fill(true),
                                    );
                                    // A slider only senses drags, so its response never
                                    // reports a secondary click; the hit test is "pointer
                                    // over the slider" plus the raw click event (B9).
                                    let reset = response.hovered()
                                        && ui.input(|i| i.pointer.secondary_clicked());
                                    if reset {
                                        gain_db = MASTER_GAIN_NEUTRAL_DB;
                                        ui.ctx().request_repaint();
                                    }
                                    let changed = response.changed();
                                    response.on_hover_text(
                                        "Master gain - both outputs. Right-click to reset.",
                                    );
                                    if changed || reset {
                                        self.cfg.master_gain_db = gain_db;
                                        self.engine.set_master_gain_db(gain_db);
                                        self.mark_dirty();
                                    }
                                });
                            });
                        }

                        if self.cfg.stop_all_hotkey_enabled {
                            ui.add_space(6.0 * gap);
                            self.stop_all_card(ui);
                        }
                        if self.cfg.loop_enabled {
                            ui.add_space(6.0 * gap);
                            self.loop_card(ui, compact);
                        }

                        ui.add_space(6.0 * gap);
                    });
            },
        );

        // Meter, Settings and the credit stay pinned below the scroll area.
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), reserve),
            Layout::bottom_up(Align::Min),
            |ui| {
                let full = Vec2::new(ui.available_width(), 32.0);
                if ghost_button(ui, "Settings", full)
                    .on_hover_text("Playback, audio, themes, about (Ctrl+,)")
                    .clicked()
                {
                    self.open_settings(ui.ctx());
                }
                ui.add_space(6.0);
                self.level_meter(ui, compact);
                if !compact {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Developed by {DEVELOPER_NAME}"))
                            .font(theme::medium(12.5))
                            .color(theme::faint()),
                    );
                    ui.add_space(2.0);
                }
            },
        );
    }

    /// Sidebar widget: stop everything now, and own the stop-all hotkey.
    ///
    /// The card is drawn by hand rather than with a `Label`, because the whole
    /// body has to be one click target: a label only senses its own text, which
    /// left most of the card dead (B7).
    fn stop_all_card(&mut self, ui: &mut Ui) {
        let mut stopped = false;
        let mut capture = false;
        let mut clear = false;
        Frame::NONE
            .fill(theme::card())
            .stroke(Stroke::new(1.0_f32, theme::border_soft()))
            .corner_radius(theme::RADIUS_MD)
            .inner_margin(Margin::symmetric(10, 4))
            .show(ui, |ui| {
                let (row, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), STOP_ALL_PILL_H),
                    Sense::hover(),
                );
                let painter = ui.painter().clone();
                // The pill owns the right end of the row; the body is everything
                // left of it. The two hit rects never overlap, so a body click
                // cannot also fire the capture pill.
                let pill_rect = Rect::from_min_max(
                    Pos2::new(
                        row.right() - STOP_ALL_PILL_W,
                        row.center().y - STOP_ALL_PILL_H * 0.5,
                    ),
                    row.right_bottom(),
                );
                let body_rect = Rect::from_min_max(
                    row.min,
                    Pos2::new(pill_rect.left() - STOP_ALL_BODY_GAP, row.max.y),
                );

                let body = ui.interact(body_rect, Id::new("stop_all_body"), Sense::click());
                if body.hovered() {
                    painter.rect_filled(
                        body_rect,
                        CornerRadius::same(theme::RADIUS_SM),
                        theme::alpha(theme::text(), 8),
                    );
                }
                painter.text(
                    body_rect.left_center(),
                    Align2::LEFT_CENTER,
                    "Stop all",
                    theme::ui_font(12.0),
                    theme::text(),
                );
                stopped = body.clicked();
                body.on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("Stop every playing sound now");

                let unset = self.stop_all.is_none();
                let label = self
                    .stop_all
                    .map(|hotkey| hotkey.label())
                    .unwrap_or_else(|| "Set hotkey".to_string());
                // Same surface as the hotkey pill on a sound card.
                painter.rect(
                    pill_rect,
                    CornerRadius::same(theme::RADIUS_MD),
                    theme::field(),
                    Stroke::new(1.0_f32, theme::border_soft()),
                    StrokeKind::Inside,
                );
                painter.text(
                    pill_rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    theme::ui_font(11.0),
                    if unset {
                        theme::faint()
                    } else {
                        theme::muted()
                    },
                );
                let pill = ui.interact(pill_rect, Id::new("stop_all_hotkey"), Sense::click());
                capture = pill.clicked();
                clear = pill.secondary_clicked() && !unset;
                pill.on_hover_text("Click to set, right-click to clear");
            });

        if stopped {
            self.engine.stop_all();
            self.log(StatusKind::Info, "Stopped all sounds");
        }
        if clear {
            self.stop_all = None;
            self.sync_active_library();
            self.mark_dirty();
        }
        if capture {
            self.capture = Some(CaptureState {
                target: CaptureTarget::StopAll,
                current: self.stop_all,
            });
        }
    }

    /// Pill-shaped Box/List segmented control.
    fn layout_selector(&mut self, ui: &mut Ui) {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            theme::field(),
            Stroke::new(1.0_f32, theme::border_soft()),
            StrokeKind::Inside,
        );
        let inner = rect.shrink(3.0);
        let half = inner.width() * 0.5;
        let target = if self.cfg.layout == SoundLayout::Grid {
            0.0
        } else {
            1.0
        };
        let pos = anim::tween(ui, Id::new("layout_pill"), target, 0.09);
        let smear = anim::tween(ui, Id::new("layout_pill_smear"), target, 0.16);
        let pill = Rect::from_min_size(
            Pos2::new(inner.left() + pos * half, inner.top()),
            Vec2::new(half, inner.height()),
        );
        let smear_pill = Rect::from_min_size(
            Pos2::new(inner.left() + smear * half, inner.top()),
            Vec2::new(half, inner.height()),
        );
        painter.rect_filled(
            smear_pill,
            CornerRadius::same(theme::RADIUS_SM),
            theme::alpha(theme::accent(), 70),
        );
        painter.rect_filled(pill, CornerRadius::same(theme::RADIUS_SM), theme::accent());

        for (index, layout) in [SoundLayout::Grid, SoundLayout::List]
            .into_iter()
            .enumerate()
        {
            let segment = Rect::from_min_size(
                Pos2::new(inner.left() + index as f32 * half, inner.top()),
                Vec2::new(half, inner.height()),
            );
            let active = self.cfg.layout == layout;
            let response = ui.interact(segment, Id::new(("layout_segment", index)), Sense::click());
            if !active && response.hovered() {
                painter.rect_filled(
                    segment,
                    CornerRadius::same(theme::RADIUS_SM),
                    theme::alpha(theme::text(), 10),
                );
            }
            let color = if active {
                Color32::WHITE
            } else {
                let hover = anim::hover(ui, Id::new(("layout_hover", index)), response.hovered());
                let base = theme::muted();
                theme::mix(base, theme::text(), hover * 0.4)
            };
            let label = match layout {
                SoundLayout::Grid => "Box",
                SoundLayout::List => "List",
            };
            let font = if active {
                theme::medium(12.5)
            } else {
                theme::ui_font(12.5)
            };
            let text_width = painter
                .layout_no_wrap(label.to_owned(), font.clone(), color)
                .size()
                .x;
            let group_width = 10.0 + 6.0 + text_width;
            let start = segment.center().x - group_width * 0.5;
            let icon_rect = Rect::from_center_size(
                Pos2::new(start + 5.0, segment.center().y),
                Vec2::splat(10.0),
            );
            match layout {
                SoundLayout::Grid => paint_box_icon(painter, icon_rect, color),
                SoundLayout::List => paint_list_icon(painter, icon_rect, color),
            }
            painter.text(
                Pos2::new(start + 16.0, segment.center().y),
                Align2::LEFT_CENTER,
                label,
                font,
                color,
            );
            if response.clicked() && !active {
                self.cfg.layout = layout;
                self.mark_dirty();
            }
            response.on_hover_text(label);
        }
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
        let mut refresh = false;

        let width = ui.available_width();
        ComboBox::from_id_salt(("device", slot.index()))
            .width(width)
            .selected_text(RichText::new(selected_text).size(13.5))
            .show_ui(ui, |ui| {
                ui.set_min_width(width - 20.0);
                if ui
                    .selectable_label(current_id.is_none(), SILENT_DEVICE_LABEL)
                    .clicked()
                {
                    chosen = Some(None);
                    ui.close_menu();
                }
                ui.separator();
                for device in &self.devices {
                    let active = current_id.as_deref() == Some(device.id.as_str());
                    let label = device.name.to_string();
                    if ui.selectable_label(active, label).clicked() {
                        chosen = Some(Some(device.id.clone()));
                        ui.close_menu();
                    }
                }
                if self.devices.is_empty() {
                    ui.label(RichText::new("No output devices").color(theme::faint()));
                }
                ui.separator();
                if ui.button("Refresh devices").clicked() {
                    refresh = true;
                    ui.close_menu();
                }
            });

        if let Some(choice) = chosen {
            self.set_device(slot, choice);
        } else if refresh {
            self.refresh_devices();
        }
    }

    fn device_volume(&mut self, ui: &mut Ui, slot: Slot) {
        let mut position = volume::to_position(self.slot_volume(slot));
        ui.horizontal(|ui| {
            // The readout claims its natural width on the right and the slider
            // fills the rest. Sizing the slider from a guessed reserve left the
            // row wider than the panel, which pushed every later card in the
            // scroll area past the right margin (B4).
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(volume::percent_text(position))
                        .size(11.0)
                        .color(theme::muted())
                        .monospace(),
                );
                ui.spacing_mut().slider_width = ui.available_width();
                let changed = ui
                    .add(
                        egui::Slider::new(&mut position, 0.0..=1.0)
                            .show_value(false)
                            .trailing_fill(true),
                    )
                    .changed();
                if changed {
                    self.set_slot_volume(slot, volume::from_position(position));
                }
            });
        });
    }

    /// Background loop player. Drawn only when enabled in Settings.
    fn loop_card(&mut self, ui: &mut Ui, compact: bool) {
        let path = self.cfg.loop_path.as_deref().map(model::resolve_path);
        let name = path
            .as_ref()
            .map(|path| display_name(path))
            .unwrap_or_else(|| "No loop file".to_string());
        let active = self.loop_state.is_active();
        let error = self.loop_state.error().map(str::to_string);
        let missing = path.is_some() && self.loop_missing;
        let has_file = path.is_some() && !missing;
        let playable = has_file && error.is_none();
        let retry = has_file && error.is_some();
        let height = if compact { 44.0 } else { 84.0 };
        let (rect, card_response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        let hover = anim::hover(ui, Id::new("loop_card"), ui.rect_contains_pointer(rect));
        let painter = ui.painter();

        let fill = theme::mix(theme::card(), theme::text(), 0.03 * hover);
        let fill = if active {
            theme::mix(fill, theme::accent(), 0.08)
        } else {
            fill
        };
        let border = if missing || error.is_some() {
            theme::alpha(theme::red(), 150)
        } else if active {
            theme::alpha(theme::accent(), 150)
        } else {
            theme::border_soft()
        };
        painter.rect(
            rect,
            CornerRadius::same(theme::RADIUS_LG),
            fill,
            Stroke::new(1.0_f32, border),
            StrokeKind::Inside,
        );

        let (transport_rect, pick_rect) = if compact {
            (
                Rect::from_min_size(
                    Pos2::new(rect.left() + 7.0, rect.top() + 2.0),
                    Vec2::splat(22.0),
                ),
                Rect::from_min_size(
                    Pos2::new(rect.right() - 23.0, rect.top() + 5.0),
                    Vec2::splat(16.0),
                ),
            )
        } else {
            (
                Rect::from_min_size(
                    Pos2::new(rect.left() + 10.0, rect.top() + 30.0),
                    Vec2::new(78.0, 26.0),
                ),
                Rect::from_min_size(
                    Pos2::new(rect.right() - 26.0, rect.top() + 10.0),
                    Vec2::splat(16.0),
                ),
            )
        };

        let transport = ui.interact(
            transport_rect,
            Id::new("loop_transport"),
            if playable || retry || active {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let pick = ui.interact(pick_rect, Id::new("loop_pick"), Sense::click());

        // Transport button: filled accent while playing, outlined otherwise.
        let (bg, fg, stroke) = if active {
            (theme::accent(), theme::bg(), theme::accent())
        } else if playable || retry {
            (theme::field(), theme::text(), theme::border())
        } else {
            (theme::field(), theme::faint(), theme::border_soft())
        };
        let bg = if transport.hovered() && (playable || retry || active) {
            theme::mix(bg, theme::text(), 0.08)
        } else {
            bg
        };
        painter.rect(
            transport_rect,
            CornerRadius::same(theme::RADIUS_MD),
            bg,
            Stroke::new(1.0_f32, stroke),
            StrokeKind::Inside,
        );
        let press = anim::press(
            ui,
            Id::new("loop_transport_press"),
            transport.is_pointer_button_down_on(),
        );
        press_overlay(painter, transport_rect.shrink(1.5), theme::RADIUS_MD, press);

        let icon_rect = Rect::from_center_size(
            Pos2::new(transport_rect.left() + 14.0, transport_rect.center().y),
            Vec2::splat(12.0),
        );
        if active {
            paint_stop_icon(painter, icon_rect, fg);
        } else {
            paint_play_icon(painter, icon_rect, fg);
        }
        if !compact {
            painter.text(
                Pos2::new(transport_rect.left() + 25.0, transport_rect.center().y),
                Align2::LEFT_CENTER,
                if active {
                    "Stop"
                } else if retry {
                    "Retry"
                } else {
                    "Play"
                },
                theme::ui_font(11.5),
                fg,
            );
        }

        // Pick / Locate.
        let pick_hover = anim::hover(ui, Id::new("loop_pick_hover"), pick.hovered());
        if pick_hover > 0.01 {
            painter.rect_filled(
                pick_rect.expand(3.0),
                CornerRadius::same(theme::RADIUS_MD),
                theme::alpha(theme::accent(), (16.0 * pick_hover) as u8),
            );
        }
        paint_folder_icon(
            painter,
            pick_rect,
            if pick.hovered() {
                theme::accent()
            } else {
                theme::muted()
            },
        );

        // FILE / LOOP badge.
        let name_left = if compact {
            rect.left() + 62.0
        } else {
            rect.left() + 52.0
        };
        let name_right = pick_rect.left() - 6.0;
        if compact {
            painter.text(
                Pos2::new(rect.left() + 37.0, rect.top() + 13.0),
                Align2::LEFT_CENTER,
                "LOOP",
                theme::ui_font(7.5),
                theme::accent(),
            );
        } else {
            let badge = Rect::from_min_size(
                Pos2::new(rect.left() + 10.0, rect.top() + 11.0),
                Vec2::new(34.0, 14.0),
            );
            painter.rect(
                badge,
                CornerRadius::same(theme::RADIUS_SM),
                theme::alpha(theme::accent(), 16),
                Stroke::new(1.0_f32, theme::alpha(theme::accent(), 70)),
                StrokeKind::Inside,
            );
            painter.text(
                badge.center(),
                Align2::CENTER_CENTER,
                "LOOP",
                theme::ui_font(8.5),
                theme::accent(),
            );
        }

        let name_color = if missing || error.is_some() {
            theme::red()
        } else if playable || active {
            theme::text()
        } else {
            theme::faint()
        };
        let shown = if compact {
            if missing {
                "File missing"
            } else {
                error.as_deref().unwrap_or(name.as_str())
            }
        } else {
            name.as_str()
        };
        let name_width = (name_right - name_left).max(20.0);
        let font = theme::ui_font(if compact { 11.0 } else { 11.5 });
        painter.text(
            Pos2::new(name_left, rect.top() + if compact { 13.0 } else { 18.0 }),
            Align2::LEFT_CENTER,
            theme::truncate(painter, shown, font, name_width),
            theme::ui_font(if compact { 11.0 } else { 11.5 }),
            name_color,
        );

        if !compact {
            let status = if missing {
                "File missing".to_string()
            } else {
                match &self.loop_state {
                    LoopState::Idle => "Ready".to_string(),
                    LoopState::Playing => "Playing".to_string(),
                    LoopState::Error(message) => message.clone(),
                }
            };
            let status_color = if missing || error.is_some() {
                theme::red()
            } else if active {
                theme::accent()
            } else {
                theme::faint()
            };
            let status_left = transport_rect.right() + 9.0;
            let status_width = (rect.right() - 10.0 - status_left).max(20.0);
            painter.text(
                Pos2::new(status_left, transport_rect.center().y),
                Align2::LEFT_CENTER,
                theme::truncate(painter, &status, theme::ui_font(10.5), status_width),
                theme::ui_font(10.5),
                status_color,
            );
        }

        // Loop level, independent of the Monitor/Cable volumes.
        let volume_rect = if compact {
            Rect::from_min_max(
                Pos2::new(rect.left() + 7.0, rect.top() + 27.0),
                Pos2::new(rect.right() - 7.0, rect.top() + 43.0),
            )
        } else {
            Rect::from_min_max(
                Pos2::new(rect.left() + 8.0, rect.top() + 59.0),
                Pos2::new(rect.right() - 8.0, rect.top() + 79.0),
            )
        };
        volume::bar(
            ui,
            volume_rect,
            Id::new("loop_volume"),
            self.cfg.loop_volume,
            |volume| self.set_loop_volume(volume),
            compact,
        );

        let transport = transport.on_hover_text(if active {
            "Stop the looping track"
        } else if retry {
            "Try loading the loop file again"
        } else {
            "Play the looping track"
        });
        let pick = pick.on_hover_text(if missing {
            "Locate the loop file"
        } else {
            "Choose a loop file"
        });
        if pick.clicked() {
            self.pick_loop_file();
            ui.ctx().request_repaint();
        }
        if transport.clicked() && (playable || retry || active) {
            self.toggle_loop();
            ui.ctx().request_repaint();
        }
        if let Some(text) = error
            .clone()
            .or_else(|| path.as_ref().map(|path| path.display().to_string()))
        {
            card_response.on_hover_text(text);
        }
    }

    /// Compact two-output peak meter with a dBFS scale.
    fn level_meter(&self, ui: &mut Ui, compact: bool) {
        let width = ui.available_width();
        let label_w = 48.0;
        let bar_left = label_w;
        let bar_right = width;
        let row_h = 15.0;
        // The dB scale is the first thing to go on short windows.
        let scale_h = if compact { 0.0 } else { 13.0 };
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(width, row_h * 2.0 + scale_h), Sense::hover());
        let painter = ui.painter();

        for (slot, label) in ["Monitor", "Cable"].iter().enumerate() {
            let y = rect.top() + slot as f32 * row_h;
            painter.text(
                Pos2::new(rect.left(), y + row_h * 0.5),
                Align2::LEFT_CENTER,
                *label,
                theme::ui_font(10.5),
                theme::muted(),
            );
            let bar = Rect::from_min_max(
                Pos2::new(rect.left() + bar_left, y + 3.0),
                Pos2::new(rect.left() + bar_right, y + 11.0),
            );
            draw_level_bar(painter, bar, self.meter_db[slot], self.meter_peak_db[slot]);
        }

        if compact {
            return;
        }

        let scale_y = rect.top() + row_h * 2.0 + 1.0;
        for db in [-60.0, -48.0, -36.0, -24.0, -12.0, 0.0] {
            let x = level_x(
                Rect::from_min_max(
                    Pos2::new(rect.left() + bar_left, scale_y),
                    Pos2::new(rect.left() + bar_right, scale_y),
                ),
                db,
            );
            painter.line_segment(
                [Pos2::new(x, scale_y), Pos2::new(x, scale_y + 3.0)],
                Stroke::new(1.0_f32, theme::alpha(theme::faint(), 120)),
            );
            let align = if db <= -60.0 {
                Align2::LEFT_TOP
            } else if db >= 0.0 {
                Align2::RIGHT_TOP
            } else {
                Align2::CENTER_TOP
            };
            painter.text(
                Pos2::new(x, scale_y + 3.0),
                align,
                format!("{}", db as i32),
                theme::ui_font(8.5),
                theme::faint(),
            );
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            // Borrowed, not cloned: this runs once per frame while the newest
            // line usually has not changed at all.
            let latest = self.statuses.back();
            let (kind, text) = match latest {
                Some(entry) => (entry.kind, entry.text.as_str()),
                None => (StatusKind::Info, "Ready"),
            };
            status_dot(ui, kind.color());
            ui.add(egui::Label::new(RichText::new(text).size(12.5).color(kind.color())).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if tiny_button(ui, "Activity log").clicked() {
                    self.show_log = true;
                }
            });
        });
        let _ = ctx;
    }

    fn main_page(&mut self, ui: &mut Ui) {
        let fade = if anim::enabled() {
            anim::ease_out(self.page_fade.elapsed().as_secs_f32() / 0.09)
        } else {
            1.0
        };
        if fade < 1.0 {
            ui.ctx().request_repaint();
        }
        ui.set_opacity(fade);
        self.toolbar(ui);
        ui.add_space(6.0);

        // The board's per-frame buffers live on `App`; taking them out lets the
        // paint closures borrow the board immutably while filling them, and the
        // next frame reuses the same allocations.
        let mut scratch = std::mem::take(&mut self.board_scratch);
        scratch.clear();

        self.filtered_indices(&mut scratch.filtered);
        let active = self.engine.active();
        // `Some(progress)` while a voice plays; the inner option is the bar, which
        // stays `None` until the stream's duration is known.
        for voice in &active {
            scratch
                .playing
                .entry(voice.sound_id)
                .and_modify(|known| {
                    if let Some(progress) = voice.progress {
                        *known = Some(known.unwrap_or(0.0).max(progress));
                    }
                })
                .or_insert(voice.progress);
        }

        // Only query playback state for sounds that are actually on screen.
        for &index in &scratch.filtered {
            scratch.ids.push(self.sounds[index].id);
        }
        let infos = self.playback.info_map(&scratch.ids);

        let mut rects: Vec<(u64, Rect)> = Vec::new();
        let mut rows: Vec<(u64, Rect)> = Vec::new();
        let allow_drag = self.search.trim().is_empty();
        let capture_id = self
            .capture
            .as_ref()
            .and_then(|capture| match capture.target {
                CaptureTarget::Sound(id) => Some(id),
                CaptureTarget::StopAll => None,
            });

        if self.sounds.is_empty()
            || !self
                .sounds
                .iter()
                .any(|sound| sound.library_id == self.active_library)
        {
            self.empty_state(ui);
        } else if scratch.filtered.is_empty() {
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
                            &self.libraries,
                            &scratch.filtered,
                            &infos,
                            &scratch.playing,
                            allow_drag,
                            capture_id,
                            self.selected,
                            self.cfg.show_ram_state,
                            &mut rects,
                            &mut scratch.actions,
                        ),
                        SoundLayout::List => draw_list(
                            ui,
                            &self.sounds,
                            &self.libraries,
                            &scratch.filtered,
                            &infos,
                            &scratch.playing,
                            allow_drag,
                            capture_id,
                            self.selected,
                            self.cfg.show_ram_state,
                            &mut rows,
                            &mut scratch.actions,
                        ),
                    }
                    ui.add_space(10.0);
                });
        }

        self.card_rects = rects;
        self.row_rects = rows;
        // Drain, then hand the buffer back so the next frame reuses its capacity.
        self.handle_actions(&mut scratch.actions, ui.ctx());
        self.board_scratch = scratch;

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

    fn library_bar(&mut self, ui: &mut Ui, width: f32) {
        let mut switch_to = None;
        let mut rename = None;
        let mut delete = None;
        let mut add = false;

        let tabs_width = (width - 32.0).max(60.0);
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(tabs_width, 28.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    egui::ScrollArea::horizontal()
                        .id_salt("library_tabs")
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([true, true])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for library in &self.libraries {
                                    let active = library.id == self.active_library;
                                    let width = library_tab_width(ui, &library.name, active);
                                    let (rect, response) = ui.allocate_exact_size(
                                        Vec2::new(width, 28.0),
                                        Sense::click(),
                                    );
                                    let hover = anim::hover(
                                        ui,
                                        Id::new(("tab_hover", library.id)),
                                        response.hovered(),
                                    );
                                    paint_library_tab(
                                        ui.painter(),
                                        rect,
                                        &library.name,
                                        active,
                                        hover,
                                    );
                                    if response.clicked() {
                                        switch_to = Some(library.id);
                                    }
                                    let can_delete = self.libraries.len() > 1;
                                    response.context_menu(|ui| {
                                        if ui.button("Rename...").clicked() {
                                            rename = Some(library.id);
                                            ui.close_menu();
                                        }
                                        if can_delete
                                            && ui
                                                .button(RichText::new("Delete").color(theme::red()))
                                                .on_hover_text(
                                                    "Choose whether its sounds move or go",
                                                )
                                                .clicked()
                                        {
                                            delete = Some(library.id);
                                            ui.close_menu();
                                        }
                                    });
                                }
                            });
                        });
                },
            );
            if plus_button(ui) {
                add = true;
            }
        });

        if let Some(id) = switch_to {
            self.switch_library(id);
        }
        if let Some(id) = rename {
            if let Some(library) = self.libraries.iter().find(|l| l.id == id) {
                self.renaming = Some(RenameState {
                    target: RenameTarget::Library(id),
                    buffer: library.name.clone(),
                    focus: true,
                });
            }
        }
        if let Some(id) = delete {
            self.delete_library(id);
        }
        if add {
            self.add_library();
        }
    }

    /// Drops the per-tab view state (search text, selection) and re-derives what
    /// the board shows for the now-active library: bindings, RAM retention, and
    /// the pending save. Callers that also change the active library set it
    /// first.
    fn reset_view_state(&mut self) {
        self.search.clear();
        self.selected = None;
        self.sync_active_library();
        self.mark_dirty();
    }

    fn switch_library(&mut self, id: u64) {
        if id == self.active_library || !self.libraries.iter().any(|l| l.id == id) {
            return;
        }
        self.engine.stop_all();
        self.active_library = id;
        self.page_fade = Instant::now();
        self.reset_view_state();
        self.log(
            StatusKind::Info,
            format!(
                "Switched to \"{}\"",
                self.libraries
                    .iter()
                    .find(|l| l.id == id)
                    .map(|l| l.name.clone())
                    .unwrap_or_default()
            ),
        );
    }

    fn add_library(&mut self) {
        let id = self.libraries.iter().map(|l| l.id).max().unwrap_or(0) + 1;
        let name = format!("Library {}", self.libraries.len() + 1);
        self.libraries.push(Library {
            id,
            name: name.clone(),
        });
        self.active_library = id;
        self.reset_view_state();
        self.renaming = Some(RenameState {
            target: RenameTarget::Library(id),
            buffer: name,
            focus: true,
        });
    }

    /// Opens the delete-library dialog (FR-5.2.3). Nothing is deleted until the
    /// user chooses; the last remaining library is refused here, so the dialog can
    /// always offer somewhere to move its sounds.
    fn delete_library(&mut self, id: u64) {
        if self.libraries.len() <= 1 {
            self.log(StatusKind::Warning, "At least one library is required");
            return;
        }
        if !self.libraries.iter().any(|library| library.id == id) {
            return;
        }
        let target_id = self
            .libraries
            .iter()
            .find(|library| library.id != id)
            .map(|library| library.id);
        let Some(target_id) = target_id else {
            return;
        };
        self.deleting = Some(DeleteLibraryState {
            library_id: id,
            target_id,
        });
    }

    /// Applies the dialog: move the sounds to `target`, or drop them from the
    /// board. Neither touches the audio files (FR-5.3.14).
    fn confirm_delete_library(&mut self, id: u64, outcome: LibraryDeleteOutcome) {
        let Some(fallback) = self
            .libraries
            .iter()
            .find(|library| library.id != id)
            .map(|library| library.id)
        else {
            self.log(StatusKind::Warning, "At least one library is required");
            return;
        };
        let Some(index) = self.libraries.iter().position(|library| library.id == id) else {
            return;
        };
        let removed = self.libraries.remove(index);
        let label = removed.name;
        let was_active = self.active_library == removed.id;
        if was_active {
            self.engine.stop_all();
        }

        // A pending undo may hold a sound from this library; it is not on the
        // board, so the loops below cannot reach it.
        let undo_in_removed = self
            .undo
            .as_ref()
            .map(|undo| undo.sound.library_id == removed.id)
            .unwrap_or(false);
        if undo_in_removed {
            match outcome {
                LibraryDeleteOutcome::Move(target) => {
                    if let Some(undo) = self.undo.as_mut() {
                        undo.sound.library_id = target;
                    }
                }
                LibraryDeleteOutcome::Remove => self.undo = None,
            }
        }

        let (kind, summary) = match outcome {
            LibraryDeleteOutcome::Move(target) => {
                // The combo can point at a library deleted between frames; fall
                // back to the one every branch of the dialog is guaranteed.
                let target = if self.libraries.iter().any(|library| library.id == target) {
                    target
                } else {
                    fallback
                };
                let mut moved = 0usize;
                for sound in &mut self.sounds {
                    if sound.library_id == id {
                        sound.library_id = target;
                        moved += 1;
                    }
                }
                if was_active {
                    self.active_library = target;
                }
                (
                    StatusKind::Info,
                    format!(
                        "moved {moved} sound{} to \"{}\"",
                        if moved == 1 { "" } else { "s" },
                        self.library_name(target)
                    ),
                )
            }
            LibraryDeleteOutcome::Remove => {
                let doomed: Vec<u64> = self
                    .sounds
                    .iter()
                    .filter(|sound| sound.library_id == id)
                    .map(|sound| sound.id)
                    .collect();
                let count = doomed.len();
                self.sounds.retain(|sound| sound.library_id != id);
                for sound_id in doomed {
                    self.playback.remove(sound_id);
                    if self.selected == Some(sound_id) {
                        self.selected = None;
                    }
                }
                if was_active {
                    self.active_library = fallback;
                }
                (
                    StatusKind::Warning,
                    format!(
                        "removed {count} sound{} from the board",
                        if count == 1 { "" } else { "s" }
                    ),
                )
            }
        };

        if was_active {
            self.reset_view_state();
        } else {
            self.sync_active_library();
            self.mark_dirty();
        }
        self.log(kind, format!("Deleted \"{label}\" - {summary}"));
    }

    /// The delete-library dialog (FR-5.2.3): the sound count, the move target, and
    /// the two irreversible choices plus cancel. Nothing is deleted until the user
    /// picks; closing (X, Esc, backdrop) leaves the library untouched.
    fn delete_library_modal(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.deleting.take() else {
            return;
        };
        let Some(library) = self
            .libraries
            .iter()
            .find(|library| library.id == state.library_id)
            .cloned()
        else {
            return;
        };
        let targets: Vec<Library> = self
            .libraries
            .iter()
            .filter(|target| target.id != library.id)
            .cloned()
            .collect();
        let Some(first) = targets.first() else {
            return;
        };
        if !targets.iter().any(|target| target.id == state.target_id) {
            state.target_id = first.id;
        }
        let selected_name = targets
            .iter()
            .find(|target| target.id == state.target_id)
            .map(|target| target.name.clone())
            .unwrap_or_default();
        let count = self
            .sounds
            .iter()
            .filter(|sound| sound.library_id == library.id)
            .count();

        let mut close = ctx.input(|i| i.key_pressed(Key::Escape));
        let mut outcome = None;
        let content = self.content_rect;
        modal_shell(
            ctx,
            content,
            Id::new("delete_library_modal"),
            CONFIRM_MODAL_WIDTH,
            MODAL_MARGIN,
            &mut close,
            |ui, dismiss| {
                let title = format!(
                    "Delete \"{}\" - {count} sound{}",
                    library.name,
                    if count == 1 { "" } else { "s" }
                );
                modal_header(ui, &title, Id::new("delete_library_close"), dismiss);
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Move the sounds to another library or remove them from the board. \
                         Audio files stay on disk.",
                    )
                    .size(11.5)
                    .color(theme::muted()),
                );
                ui.add_space(10.0);
                settings_value(ui, "Move to", 190.0, |ui| {
                    ComboBox::from_id_salt("delete_library_target")
                        .width(190.0)
                        .selected_text(RichText::new(&selected_name).size(13.5))
                        .show_ui(ui, |ui| {
                            ui.set_min_width(170.0);
                            for target in &targets {
                                if ui
                                    .selectable_label(target.id == state.target_id, &target.name)
                                    .clicked()
                                {
                                    state.target_id = target.id;
                                    ui.close_menu();
                                }
                            }
                        });
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if accent_button(ui, "Move sounds", Vec2::new(120.0, 32.0)).clicked() {
                        outcome = Some(LibraryDeleteOutcome::Move(state.target_id));
                    }
                    if ghost_button_colored(
                        ui,
                        "Delete sounds",
                        Vec2::new(120.0, 32.0),
                        theme::red(),
                    )
                    .clicked()
                    {
                        outcome = Some(LibraryDeleteOutcome::Remove);
                    }
                    if ghost_button(ui, "Cancel", Vec2::new(90.0, 32.0)).clicked() {
                        *dismiss = true;
                    }
                });
            },
        );

        if let Some(outcome) = outcome {
            self.confirm_delete_library(library.id, outcome);
            return;
        }
        if !close {
            self.deleting = Some(state);
        }
    }

    fn move_sound_to_library(&mut self, sound_id: u64, library_id: u64) {
        let Some(library) = self.libraries.iter().find(|l| l.id == library_id) else {
            return;
        };
        let label = library.name.clone();
        let active = self.active_library;
        let Some(sound) = self.sounds.iter_mut().find(|s| s.id == sound_id) else {
            return;
        };
        if sound.library_id == library_id {
            return;
        }
        sound.library_id = library_id;
        let name = sound.name.clone();
        if self.selected == Some(sound_id) {
            self.selected = None;
        }
        if library_id != active {
            self.engine.stop_sound(sound_id);
        }
        self.mark_dirty();
        self.sync_active_library();
        self.log(StatusKind::Info, format!("Moved \"{name}\" to \"{label}\""));
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        // One content width for the whole board, so the RAM pill ends exactly
        // where the grid's last column does (B4).
        let content_width = board_content_width(ui);
        ui.allocate_ui_with_layout(
            Vec2::new(content_width, 34.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                // Zero item spacing so the widths add up exactly and the pill
                // ends flush with the search field's right edge.
                ui.spacing_mut().item_spacing.x = 0.0;
                // The pill is optional: with RAM state hidden the field gets the
                // whole row instead of leaving a 174 px hole.
                let show_pill = self.cfg.show_ram_state;
                let pill_width = if show_pill { 164.0 } else { 0.0 };
                let gap = if show_pill { 10.0 } else { 0.0 };
                let search_width = search_field_width(content_width, pill_width, gap);
                let response = ui
                    .scope(|ui| {
                        // Match the RAM pill: fields and pills share one radius.
                        let radius = CornerRadius::same(theme::RADIUS_XL);
                        let widgets = &mut ui.visuals_mut().widgets;
                        widgets.inactive.corner_radius = radius;
                        widgets.hovered.corner_radius = radius;
                        widgets.active.corner_radius = radius;
                        ui.add_sized(
                            [search_width, 32.0],
                            TextEdit::singleline(&mut self.search)
                                .hint_text("Search sounds...")
                                .margin(Margin {
                                    left: 12,
                                    right: 10,
                                    top: 2,
                                    bottom: 2,
                                })
                                .text_color(theme::text())
                                .vertical_align(Align::Center),
                        )
                    })
                    .inner;
                if self.focus_search {
                    response.request_focus();
                    self.focus_search = false;
                }

                if self.cfg.show_ram_state {
                    ui.add_space(gap);
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
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(pill_width, 28.0), Sense::hover());
                    let response = ui.interact(
                        rect,
                        Id::new("ram_status_pill"),
                        if enabled {
                            Sense::click()
                        } else {
                            Sense::hover()
                        },
                    );
                    let painter = ui.painter();
                    painter.rect(
                        rect,
                        CornerRadius::same(theme::RADIUS_XL),
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
                }
            },
        );
    }

    /// Fill `out` with the indices of the sounds the board shows, in tab order.
    /// The buffer belongs to the caller so a frame does not allocate a new one.
    fn filtered_indices(&self, out: &mut Vec<usize>) {
        out.clear();
        let needle = self.filter_needle.trim();
        for (index, sound) in self.sounds.iter().enumerate() {
            if sound.library_id != self.active_library {
                continue;
            }
            if !needle.is_empty() && !sound.search_key.contains(needle) {
                continue;
            }
            out.push(index);
            if out.len() >= MAX_SEARCH_RESULTS {
                break;
            }
        }
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
                CornerRadius::same(theme::RADIUS_XL),
                theme::alpha(theme::card(), 55),
            );
            painter.rect_stroke(
                rect,
                CornerRadius::same(theme::RADIUS_XL),
                Stroke::new(
                    1.0_f32,
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
        self.capture_modal(ctx);
        self.rename_modal(ctx);
        self.delete_library_modal(ctx);
        self.import_modal(ctx);
        self.reset_modal(ctx);
        self.log_modal(ctx);
    }

    // ---------------------------------------------------------------------
    // Settings screen
    // ---------------------------------------------------------------------

    fn select_settings_tab(&mut self, tab: SettingsTab, ctx: &egui::Context) {
        if self.settings_tab != tab {
            self.settings_tab = tab;
            self.settings_tab_fade = Instant::now();
            ctx.request_repaint();
        }
    }

    fn settings_rail(&mut self, ui: &mut Ui) {
        let open = if anim::enabled() {
            let t = anim::ease_out(self.settings_opened.elapsed().as_secs_f32() / 0.09);
            if t < 1.0 {
                ui.ctx().request_repaint();
            }
            t
        } else {
            1.0
        };
        ui.set_opacity(open);

        if settings_back(ui) {
            self.close_settings(ui.ctx());
        }
        ui.add_space(SETTINGS_RAIL_SECTION_GAP);
        for tab in SettingsTab::ALL
            .into_iter()
            .filter(|tab| *tab != SettingsTab::About)
        {
            if settings_nav_item(ui, tab, self.settings_tab == tab) {
                self.select_settings_tab(tab, ui.ctx());
            }
            ui.add_space(SETTINGS_RAIL_ITEM_GAP);
        }

        // About pinned to the bottom; everything else is a normal tab.
        ui.add_space((ui.available_height() - SETTINGS_RAIL_FOOTER_H).max(SETTINGS_GROUP_GAP));
        settings_rule(ui);
        ui.add_space(SETTINGS_RAIL_RULE_GAP);
        if settings_nav_item(
            ui,
            SettingsTab::About,
            self.settings_tab == SettingsTab::About,
        ) {
            self.select_settings_tab(SettingsTab::About, ui.ctx());
        }
    }

    fn settings_content(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let open = if anim::enabled() {
            anim::ease_out(self.settings_opened.elapsed().as_secs_f32() / 0.09)
        } else {
            1.0
        };
        let tab_fade = if anim::enabled() {
            let t = anim::ease_out(self.settings_tab_fade.elapsed().as_secs_f32() / 0.09);
            if t < 1.0 {
                ctx.request_repaint();
            }
            t
        } else {
            1.0
        };
        ui.set_opacity(open * tab_fade);

        let tab = self.settings_tab;
        settings_header(ui, tab);
        ui.add_space(SETTINGS_GROUP_GAP);

        egui::ScrollArea::vertical()
            .id_salt(("settings_scroll", tab.title()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.scope(|ui| {
                    ui.set_width(ui.available_width().min(SETTINGS_CONTENT_MAX_W));
                    match tab {
                        SettingsTab::Playback => self.settings_page_playback(ui),
                        SettingsTab::Appearance => self.settings_page_appearance(ctx, ui),
                        SettingsTab::Window => self.settings_page_window(ui),
                        SettingsTab::About => self.settings_page_about(ui),
                    }
                    // Every page that owns fields ends with its Reset row; the
                    // group list lives in `storage::Settings`, not per page.
                    if let Some(group) = tab.group() {
                        self.settings_reset_group(ui, ctx, group);
                    }
                    ui.add_space(SETTINGS_GROUP_GAP);
                });
            });
    }

    /// The trailing "Defaults" card on a settings page: restores that page's
    /// fields only. Libraries and sounds are not settings, so they are never
    /// part of it.
    fn settings_reset_group(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        group: storage::SettingsGroup,
    ) {
        settings_group(ui, "Defaults", |ui| {
            settings_value(ui, "Reset this page", SETTINGS_BUTTON_SIZE.x, |ui| {
                if ghost_button(ui, "Reset", SETTINGS_BUTTON_SIZE)
                    .on_hover_text(format!("Restore the {} defaults", group.title()))
                    .clicked()
                {
                    self.reset_settings_group(ctx, group);
                }
            });
            settings_detail(ui, "Libraries and sounds are not affected.");
        });
    }

    fn settings_page_playback(&mut self, ui: &mut Ui) {
        settings_group(ui, "Looping track", |ui| {
            let mut enabled = self.cfg.loop_enabled;
            if settings_toggle(
                ui,
                Id::new("settings_loop_enabled"),
                "Enabled",
                &mut enabled,
                false,
                true,
            ) {
                self.set_loop_enabled(enabled);
            }
            if self.cfg.loop_enabled {
                settings_divider(ui);
                self.loop_settings_body(ui);
            }
        });

        if self.cfg.loop_enabled {
            settings_group(ui, "Loop outputs", |ui| {
                let mut monitor = self.cfg.loop_on_monitor;
                if settings_toggle(
                    ui,
                    Id::new("settings_loop_monitor"),
                    "Monitor",
                    &mut monitor,
                    false,
                    true,
                ) {
                    self.cfg.loop_on_monitor = monitor;
                    self.engine
                        .set_loop_outputs(self.cfg.loop_on_monitor, self.cfg.loop_on_cable);
                    self.mark_dirty();
                }

                settings_divider(ui);
                let mut cable = self.cfg.loop_on_cable;
                if settings_toggle(
                    ui,
                    Id::new("settings_loop_cable"),
                    "Cable",
                    &mut cable,
                    false,
                    true,
                ) {
                    self.cfg.loop_on_cable = cable;
                    self.engine
                        .set_loop_outputs(self.cfg.loop_on_monitor, self.cfg.loop_on_cable);
                    self.mark_dirty();
                }
            });
        }

        settings_group(ui, "Hotkey sounds", |ui| {
            let ram = self.playback_stats();
            let enabled = ram.is_some();
            // A partly loaded board is neither on nor off: show the half-knob
            // instead of claiming "off". A click still loads everything.
            let mixed = matches!(ram, Some(RamState::Mixed));
            let mut on = matches!(ram, Some(RamState::On));
            if settings_toggle(
                ui,
                Id::new("settings_ram"),
                "Load into RAM",
                &mut on,
                mixed,
                enabled,
            ) {
                self.set_all_hotkey_ram(on);
            }
            settings_divider(ui);
            let mut show_state = self.cfg.show_ram_state;
            if settings_toggle(
                ui,
                Id::new("settings_show_ram"),
                "Show RAM state",
                &mut show_state,
                false,
                true,
            ) {
                self.cfg.show_ram_state = show_state;
                self.mark_dirty();
            }
            if self.cfg.show_ram_state {
                settings_detail(ui, self.playback_stats.label.as_str());
            }
        });

        settings_group(ui, "Output devices", |ui| {
            for (index, slot) in [Slot::Monitor, Slot::Cable].into_iter().enumerate() {
                let (text, color) = match self.device_note[slot.index()].as_ref() {
                    // A substitution or a failure outranks "ready": the device
                    // that plays is not the one that was chosen.
                    Some(note) => (
                        format!("{}: {}", slot.label(), note.text),
                        note.kind.color(),
                    ),
                    None if self.device_ready[slot.index()] => {
                        (format!("{}: ready", slot.label()), theme::accent())
                    }
                    None => (format!("{}: not connected", slot.label()), theme::muted()),
                };
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), SETTINGS_ROW_H),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        status_dot(ui, color);
                        ui.label(RichText::new(text).size(11.0).color(color));
                    },
                );
                if index == 0 {
                    settings_divider(ui);
                }
            }
        });

        settings_group(ui, "Stop everything", |ui| {
            let mut enabled = self.cfg.stop_all_hotkey_enabled;
            if settings_toggle(
                ui,
                Id::new("settings_stop_all"),
                "Stop-all hotkey",
                &mut enabled,
                false,
                true,
            ) {
                self.cfg.stop_all_hotkey_enabled = enabled;
                self.sync_active_library();
                self.mark_dirty();
            }
            if self.cfg.stop_all_hotkey_enabled {
                let detail = match self.stop_all {
                    Some(hotkey) => {
                        format!("Key: {}. Set or change it in the sidebar.", hotkey.label())
                    }
                    None => "Set the key in the sidebar widget.".to_string(),
                };
                settings_detail(ui, detail);
            }
        });
    }

    fn loop_settings_body(&mut self, ui: &mut Ui) {
        let path = self.cfg.loop_path.as_deref().map(model::resolve_path);
        let name = path
            .as_ref()
            .map(|path| display_name(path))
            .unwrap_or_else(|| "No loop file".to_string());
        let state = self.loop_state.clone();
        let error = state.error().map(str::to_string);
        let active = state.is_active();
        let missing = path.is_some() && self.loop_missing;
        let has_file = path.is_some() && !missing;

        ui.horizontal(|ui| {
            let label_width =
                (ui.available_width() - SETTINGS_BUTTON_SIZE.x - SETTINGS_ROW_LABEL_GAP)
                    .max(SETTINGS_ROW_LABEL_MIN_W);
            ui.allocate_ui_with_layout(
                Vec2::new(label_width, SETTINGS_ROW_H + SETTINGS_DETAIL_GAP),
                Layout::top_down(Align::Min),
                |ui| {
                    let name_color = if missing || error.is_some() {
                        theme::red()
                    } else if has_file {
                        theme::text()
                    } else {
                        theme::faint()
                    };
                    ui.label(RichText::new(&name).size(12.5).color(name_color));
                    ui.add_space(2.0);
                    let (status, color) = if missing {
                        ("Missing", theme::red())
                    } else if let Some(message) = &error {
                        (message.as_str(), theme::red())
                    } else if active {
                        ("Playing", theme::accent())
                    } else if path.is_some() {
                        ("Ready", theme::muted())
                    } else {
                        ("No file", theme::faint())
                    };
                    ui.label(RichText::new(status).size(10.5).color(color));
                },
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ghost_button(
                    ui,
                    if missing { "Locate" } else { "Choose file" },
                    SETTINGS_BUTTON_SIZE,
                )
                .clicked()
                {
                    self.pick_loop_file();
                }
            });
        });
    }

    fn settings_page_appearance(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        settings_group(ui, "Theme", |ui| {
            settings_value(ui, "Theme", SETTINGS_THEME_W, |ui| {
                self.theme_picker(ctx, ui, SETTINGS_THEME_W);
            });
            settings_divider(ui);
            settings_value(ui, "Reload themes", SETTINGS_BUTTON_SIZE.x, |ui| {
                if ghost_button(ui, "Reload", SETTINGS_BUTTON_SIZE)
                    .on_hover_text("Re-read config/themes/*.md")
                    .clicked()
                {
                    self.reload_themes(ctx);
                }
            });
        });

        settings_group(ui, "Motion", |ui| {
            let mut animate = self.cfg.animate_ui;
            if settings_toggle(
                ui,
                Id::new("settings_animate"),
                "Animate UI",
                &mut animate,
                false,
                true,
            ) {
                self.cfg.animate_ui = animate;
                anim::set_user(animate);
                self.mark_dirty();
            }
        });
    }

    fn settings_page_window(&mut self, ui: &mut Ui) {
        settings_group(ui, "Tray", |ui| {
            let mut minimize = self.cfg.minimize_to_tray;
            if settings_toggle(
                ui,
                Id::new("settings_minimize_to_tray"),
                "Minimize to tray",
                &mut minimize,
                false,
                self.tray_available,
            ) {
                self.cfg.minimize_to_tray = minimize;
                self.mark_dirty();
            }
        });

        settings_group(ui, "Sidebar", |ui| {
            let mut show_gain = self.cfg.show_gain_slider;
            if settings_toggle(
                ui,
                Id::new("settings_show_gain"),
                "Show gain",
                &mut show_gain,
                false,
                true,
            ) {
                self.cfg.show_gain_slider = show_gain;
                self.mark_dirty();
            }
        });
    }

    fn settings_page_about(&mut self, ui: &mut Ui) {
        // Narrow column: the page is short text, so a 780 px card reads as an
        // empty box.
        ui.scope(|ui| {
            ui.set_max_width(SETTINGS_ABOUT_MAX_W.min(ui.available_width()));

            // Application identity and links in one card: splitting them pushed the
            // credit off a 540 px window. Data follows as its own group because
            // export/import/reset act on the whole config, not on the app.
            settings_group(ui, "Application", |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(APP_NAME)
                            .font(theme::medium(15.5))
                            .color(theme::text()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .size(11.0)
                                .color(theme::faint()),
                        );
                    });
                });
                ui.add_space(1.0);
                ui.label(
                    RichText::new("Portable Windows soundboard")
                        .size(10.5)
                        .color(theme::faint()),
                );
                // The accessibility gap is stated where users look for app info,
                // not only in the README (SPEC NFR-8.6.3).
                settings_detail(
                    ui,
                    "No screen-reader support yet - README > Accessibility lists the keyboard coverage.",
                );
                settings_divider(ui);
                settings_value(ui, "Config folder", SETTINGS_BUTTON_SIZE.x, |ui| {
                    if ghost_button(ui, "Open", SETTINGS_BUTTON_SIZE).clicked() {
                        let _ = std::process::Command::new("explorer")
                            .arg(model::config_dir())
                            .spawn();
                    }
                });
                settings_divider(ui);
                settings_value(ui, "License", SETTINGS_TEXT_COL_W, |ui| {
                    ui.label(RichText::new(LICENSE_NAME).size(11.5).color(theme::muted()));
                });
                for (label, url) in ABOUT_LINKS.iter().filter(|(_, url)| !url.is_empty()) {
                    settings_divider(ui);
                    settings_value(ui, label, SETTINGS_BUTTON_SIZE.x, |ui| {
                        if ghost_button(ui, "Open", SETTINGS_BUTTON_SIZE).clicked() {
                            open_url(url);
                        }
                    });
                }
            });

            // Backup / move to another PC (A5), plus the settings-wide reset (A6).
            settings_group(ui, "Data", |ui| {
                settings_value(ui, "Export configuration", SETTINGS_BUTTON_SIZE.x, |ui| {
                    if ghost_button(ui, "Export", SETTINGS_BUTTON_SIZE)
                        .on_hover_text("Save settings and libraries as one JSON file")
                        .clicked()
                    {
                        self.export_config();
                    }
                });
                settings_divider(ui);
                settings_value(ui, "Import configuration", SETTINGS_BUTTON_SIZE.x, |ui| {
                    if ghost_button(ui, "Import", SETTINGS_BUTTON_SIZE)
                        .on_hover_text("Replace the board from an exported file")
                        .clicked()
                    {
                        self.import_config_dialog();
                    }
                });
                settings_divider(ui);
                settings_value(ui, "Reset all settings", SETTINGS_BUTTON_SIZE.x, |ui| {
                    if ghost_button_colored(ui, "Reset", SETTINGS_BUTTON_SIZE, theme::red())
                        .on_hover_text("Settings only - libraries and sounds stay")
                        .clicked()
                    {
                        self.confirm_reset = true;
                    }
                });
                settings_detail(
                    ui,
                    "A bundle holds settings and libraries (paths stay references, audio is never copied).",
                );
            });

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Made by {DEVELOPER_NAME}"))
                        .size(10.5)
                        .color(theme::faint()),
                );
                for (label, url) in DEVELOPER_LINKS.iter().filter(|(_, url)| !url.is_empty()) {
                    ui.label(RichText::new("·").size(10.5).color(theme::faint()));
                    if link_label(ui, label, url) {
                        open_url(url);
                    }
                }
            });
        });
    }

    fn set_loop_enabled(&mut self, enabled: bool) {
        if self.cfg.loop_enabled == enabled {
            return;
        }
        self.cfg.loop_enabled = enabled;
        if !enabled {
            self.engine.stop_loop();
            self.loop_state = LoopState::Idle;
            self.loop_missing = false;
        }
        self.loop_checked_at = since(10);
        self.refresh_loop_missing(true);
        self.mark_dirty();
    }

    fn theme_picker(&mut self, ctx: &egui::Context, ui: &mut Ui, width: f32) {
        let mut selected = self.cfg.theme.clone();
        // The list is read once per reload, not per frame.
        let current_name = self
            .theme_choices
            .iter()
            .find(|(id, _)| id == &self.cfg.theme)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| selected.clone());
        ComboBox::from_id_salt("settings_theme")
            .width(width)
            .selected_text(current_name)
            .show_ui(ui, |ui| {
                for (id, name) in &self.theme_choices {
                    if ui
                        .selectable_label(selected.as_str() == id.as_str(), name)
                        .clicked()
                    {
                        selected = id.clone();
                        ui.close_menu();
                    }
                }
            });
        if selected != self.cfg.theme {
            self.cfg.theme = selected;
            for warning in theme::set_theme(&self.cfg.theme) {
                self.log(StatusKind::Warning, warning);
            }
            theme::apply_style(ctx);
            self.mark_dirty();
            ctx.request_repaint();
        }
    }

    fn reload_themes(&mut self, ctx: &egui::Context) {
        for warning in theme::reload() {
            self.log(StatusKind::Warning, warning);
        }
        self.theme_choices = theme::available();
        // Re-resolve before the membership check: a colour edit must repaint from
        // the freshly parsed file, and a vanished id must say so before the app
        // quietly corrects it to Graphite.
        for warning in theme::set_theme(&self.cfg.theme) {
            self.log(StatusKind::Warning, warning);
        }
        if !self
            .theme_choices
            .iter()
            .any(|(id, _)| id == &self.cfg.theme)
        {
            self.cfg.theme = "graphite".to_string();
        }
        theme::apply_style(ctx);
        self.mark_dirty();
        ctx.request_repaint();
    }

    fn capture_modal(&mut self, ctx: &egui::Context) {
        let Some(mut capture) = self.capture.take() else {
            return;
        };
        let mut save = false;
        let mut clear = false;

        // Read the keyboard directly so the global hotkey wins over widgets. The
        // events are walked inside the closure: cloning the frame's event list
        // bought nothing when only the resulting chord matters.
        let (escape, mut picked) = ctx.input(|input| {
            let mut escape = false;
            let mut picked: Option<Option<Hotkey>> = None;
            for event in &input.events {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    continue;
                };
                if *key == Key::Escape {
                    escape = true;
                    continue;
                }
                if (*key == Key::Backspace || *key == Key::Delete)
                    && mods_from_egui(modifiers).is_empty()
                {
                    picked = Some(None);
                    continue;
                }
                if let Some(hotkey) = Hotkey::from_egui(*key, modifiers) {
                    picked = Some(Some(hotkey));
                }
            }
            (escape, picked)
        });
        // The shell consumes Win chords (`Win+X`) before this window sees a key
        // event, so egui reports nothing at all for them: fall back to the
        // physical keyboard, which is what the hook matches against (B8).
        if picked.is_none() {
            picked = held_chord().map(Some);
        }
        let mut close = escape;
        if let Some(picked) = picked {
            capture.current = picked;
        }

        let (title, subject) = match capture.target {
            CaptureTarget::Sound(id) => ("Set hotkey", format!("\"{}\"", self.sound_name(id))),
            CaptureTarget::StopAll => (
                "Set stop-all hotkey",
                "Stops every playing sound".to_string(),
            ),
        };
        // A taken chord cannot be saved (FR-5.7.5). `chord_owner` owns the rules,
        // so the dialog, the rebind and the stop-all path all agree.
        let conflict = capture.current.and_then(|hotkey| {
            let editing = match capture.target {
                CaptureTarget::Sound(id) => ChordOwner::Sound(id),
                CaptureTarget::StopAll => ChordOwner::StopAll,
            };
            chord_owner(
                self.chord_sounds(),
                self.stop_all_chord(),
                hotkey,
                Some(editing),
            )
            .map(|owner| self.owner_label(owner))
        });
        // A blocked chord is never allowed (F5): the dialog refuses it with the
        // one message, and the load path unbinds one a hand-edited config let in.
        let blocked = capture.current.and_then(blocked_reason);

        let content = self.content_rect;
        modal_shell(
            ctx,
            content,
            Id::new("capture_modal"),
            400.0,
            MODAL_MARGIN,
            &mut close,
            |ui, dismiss| {
                modal_header(ui, title, Id::new("capture_close"), dismiss);
                ui.label(RichText::new(subject).size(13.0).color(theme::muted()));
                ui.add_space(12.0);

                let box_rect =
                    Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), 96.0));
                let painter = ui.painter();
                painter.rect_filled(
                    box_rect,
                    CornerRadius::same(theme::RADIUS_MD),
                    theme::field(),
                );
                painter.rect_stroke(
                    box_rect,
                    CornerRadius::same(theme::RADIUS_MD),
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
                if let Some(blocked) = &blocked {
                    ui.label(RichText::new(blocked).size(12.5).color(theme::red()));
                } else if let Some(conflict) = &conflict {
                    ui.label(
                        RichText::new(format!("Already used by {conflict}"))
                            .size(12.5)
                            .color(theme::red()),
                    );
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let can_save = conflict.is_none() && blocked.is_none();
                    if accent_button_enabled(ui, "Save", Vec2::new(100.0, 32.0), can_save).clicked()
                        && can_save
                    {
                        save = true;
                    }
                    if ghost_button(ui, "Cancel", Vec2::new(100.0, 32.0))
                        .on_hover_text("Esc")
                        .clicked()
                    {
                        *dismiss = true;
                    }
                    if ghost_button(ui, "Clear", Vec2::new(84.0, 32.0)).clicked() {
                        clear = true;
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Press `Esc` to cancel")
                        .size(10.5)
                        .color(theme::faint()),
                );
                ui.add_space(2.0);
            },
        );

        if close {
            return;
        }
        if save {
            let mut log_line = None;
            match capture.target {
                CaptureTarget::Sound(id) => {
                    if let Some(hotkey) = capture.current {
                        // A fresh binding starts with the global RAM preference.
                        let preload = self.cfg.preload_hotkey_sounds;
                        if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                            if sound.hotkey.is_none() {
                                sound.ram = preload;
                            }
                            sound.hotkey = Some(hotkey);
                            sound.refresh_hotkey_label();
                            log_line = Some(format!(
                                "Hotkey {} assigned to \"{}\"",
                                hotkey.label(),
                                sound.name
                            ));
                        }
                    } else if let Some((name, _)) = self.clear_sound_hotkey(id) {
                        log_line = Some(format!("Hotkey cleared for \"{name}\""));
                    }
                }
                CaptureTarget::StopAll => {
                    self.stop_all = capture.current;
                    log_line = Some(match capture.current {
                        Some(hotkey) => format!("Stop-all hotkey set to {}", hotkey.label()),
                        None => "Stop-all hotkey cleared".to_string(),
                    });
                }
            }
            if let Some(line) = log_line {
                self.log(StatusKind::Info, line);
            }
            self.sync_active_library();
            self.mark_dirty();
            return;
        }
        if clear {
            match capture.target {
                CaptureTarget::Sound(id) => {
                    if let Some((name, _)) = self.clear_sound_hotkey(id) {
                        self.log(StatusKind::Info, format!("Hotkey cleared for \"{name}\""));
                    }
                }
                CaptureTarget::StopAll => {
                    self.stop_all = None;
                    self.log(StatusKind::Info, "Stop-all hotkey cleared");
                }
            }
            self.sync_active_library();
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
        let is_library = matches!(renaming.target, RenameTarget::Library(_));
        let title = if is_library {
            "Rename library"
        } else {
            "Rename sound"
        };

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            close = true;
        }

        let content = self.content_rect;
        modal_shell(
            ctx,
            content,
            Id::new("rename_modal"),
            380.0,
            MODAL_MARGIN,
            &mut close,
            |ui, dismiss| {
                modal_header(ui, title, Id::new("rename_close"), dismiss);
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
                        *dismiss = true;
                    }
                });
            },
        );

        if commit {
            let name = renaming.buffer.trim().to_string();
            if name.is_empty() {
                self.log(StatusKind::Warning, "Name cannot be empty");
                self.renaming = Some(renaming);
                return;
            }
            match renaming.target {
                RenameTarget::Sound(id) => {
                    let mut old_name = None;
                    if let Some(sound) = self.sounds.iter_mut().find(|s| s.id == id) {
                        old_name = Some(std::mem::replace(&mut sound.name, name));
                        sound.refresh_labels();
                    }
                    if let Some(old) = old_name {
                        self.log(StatusKind::Info, format!("Renamed \"{old}\""));
                    }
                }
                RenameTarget::Library(id) => {
                    let duplicate = self.libraries.iter().any(|library| {
                        library.id != id && library.name.eq_ignore_ascii_case(&name)
                    });
                    if duplicate {
                        self.log(
                            StatusKind::Warning,
                            format!("A library named \"{name}\" already exists"),
                        );
                        self.renaming = Some(renaming);
                        return;
                    }
                    let mut old_name = None;
                    if let Some(library) = self.libraries.iter_mut().find(|l| l.id == id) {
                        old_name = Some(std::mem::replace(&mut library.name, name));
                    }
                    if let Some(old) = old_name {
                        self.log(StatusKind::Info, format!("Renamed library \"{old}\""));
                    }
                }
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
        let mut close = false;
        // Paint straight from the log: copying every entry into a fresh list each
        // frame was the whole cost of this dialog.
        let statuses = &self.statuses;
        let content = self.content_rect;
        modal_shell(
            ctx,
            content,
            Id::new("log_modal"),
            520.0,
            MODAL_MARGIN_COMPACT,
            &mut close,
            |ui, dismiss| {
                modal_header(ui, "Activity log", Id::new("log_close"), dismiss);
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if statuses.is_empty() {
                            ui.label(RichText::new("No activity yet.").color(theme::faint()));
                        }
                        for entry in statuses.iter().rev() {
                            let secs = entry.at.elapsed().as_secs();
                            let when = if secs < 60 {
                                format!("{secs}s ago")
                            } else {
                                format!("{}m ago", secs / 60)
                            };
                            ui.horizontal_top(|ui| {
                                status_dot(ui, entry.kind.color());
                                ui.label(RichText::new(&entry.text).size(12.5).color(
                                    if entry.kind == StatusKind::Error {
                                        theme::red()
                                    } else {
                                        theme::text()
                                    },
                                ));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(RichText::new(when).size(10.0).color(theme::faint()));
                                });
                            });
                            ui.add_space(2.0);
                        }
                    });
            },
        );
        if close {
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
            CornerRadius::same(theme::RADIUS_LG),
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
        if self.show_settings {
            return;
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !hovering || self.drag.is_some() {
            return;
        }
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("drop_overlay")));
        painter.rect_filled(screen, CornerRadius::ZERO, theme::alpha(theme::bg(), 220));
        painter.rect_stroke(
            screen.shrink(18.0),
            CornerRadius::same(theme::RADIUS_XL),
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
        // Modal state owns the keyboard; each modal handles its own keys.
        if self.capture.is_some()
            || self.renaming.is_some()
            || self.deleting.is_some()
            || self.importing.is_some()
            || self.confirm_reset
        {
            return;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Comma)) {
            if self.show_settings {
                self.close_settings(ctx);
            } else {
                self.open_settings(ctx);
            }
            return;
        }

        // Whether a text field owns the keyboard. Everything below that changes
        // the board is suppressed while that is true; only quitting and the
        // settings toggle above stay available.
        let typing = ctx.memory(|m| m.focused().is_some());

        // Esc is a universal "back": close an open popup first, then the current
        // overlay, cancel a drag, clear a search, and only then stop playback.
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if ctx.memory(|memory| memory.any_popup_open()) {
                return;
            }
            if self.show_settings {
                self.close_settings(ctx);
            } else if self.show_log {
                self.show_log = false;
            } else if self.drag.is_some() {
                self.drag = None;
            } else if !self.search.is_empty() {
                self.search.clear();
            } else if !typing {
                self.engine.stop_all();
                self.log(StatusKind::Info, "Stopped all sounds");
            }
            ctx.request_repaint();
            return;
        }

        // Quitting is always available (except inside a modal, handled above).
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Q)) {
            self.quit = true;
            self.save_now();
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }

        if self.show_settings || typing {
            return;
        }

        let (open, find, toggle_layout, undo, delete, space, f2) = ctx.input(|i| {
            (
                i.modifiers.ctrl && i.key_pressed(Key::O),
                i.modifiers.ctrl && i.key_pressed(Key::F),
                i.modifiers.ctrl && i.key_pressed(Key::L),
                i.modifiers.ctrl && i.key_pressed(Key::Z),
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
                        target: RenameTarget::Sound(id),
                        buffer: sound.name.clone(),
                        focus: true,
                    });
                }
            }
        }
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        self.show_settings = true;
        self.settings_opened = Instant::now();
        self.settings_tab_fade = Instant::now();
        self.focus_search = false;
        ctx.memory_mut(|memory| memory.stop_text_input());
        ctx.request_repaint();
    }

    fn close_settings(&mut self, ctx: &egui::Context) {
        self.show_settings = false;
        self.focus_search = false;
        ctx.memory_mut(|memory| memory.stop_text_input());
        self.page_fade = Instant::now();
        ctx.request_repaint();
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.poll_loop_state();
        self.refresh_loop_missing(false);
        self.save_debounced();
        self.update_tray_tip();

        // Meter motion needs ~30 fps; idle settles to one frame per second.
        let levels = self.engine.take_levels();
        let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.2);
        let now = Instant::now();
        for slot in [Slot::Monitor, Slot::Cable] {
            let index = slot.index();
            let (Some(&level), Some(db), Some(peak_db), Some(peak_at)) = (
                levels.get(index),
                self.meter_db.get_mut(index),
                self.meter_peak_db.get_mut(index),
                self.meter_peak_at.get_mut(index),
            ) else {
                continue;
            };
            smooth_meter(db, peak_db, peak_at, level, dt, now);
        }

        let interval = self.repaint_interval();
        self.last_interval_ms = interval.as_millis() as u32;
        ctx.request_repaint_after(interval);
    }

    /// The per-frame work that must keep running with the window hidden:
    /// loop errors, the save debounce and the tray tooltip. Meter smoothing and
    /// painting are deliberately absent.
    fn hidden_tick(&mut self, ctx: &egui::Context) {
        self.poll_loop_state();
        self.refresh_loop_missing(false);
        self.save_debounced();
        self.update_tray_tip();
        // Drop whatever the device produced, so showing again never flashes a
        // stale peak.
        let _ = self.engine.take_levels();
        let interval = self.repaint_interval();
        self.last_interval_ms = interval.as_millis() as u32;
        ctx.request_repaint_after(interval);
    }

    /// One place decides when the next frame is due. `request_repaint()` from a
    /// real event always wins; this only covers idle motion and the hidden case.
    fn repaint_interval(&self) -> Duration {
        const ANIM: Duration = Duration::from_millis(33);
        const SETTINGS_AUDIO: Duration = Duration::from_millis(100);
        const IDLE: Duration = Duration::from_millis(1000);

        if self.hidden {
            return IDLE;
        }

        let now = Instant::now();
        let flashing = self
            .sounds
            .iter()
            .any(|sound| sound.flash_until.map(|until| until > now).unwrap_or(false));
        let fading = self.page_fade.elapsed() < Duration::from_millis(90)
            || self.settings_opened.elapsed() < Duration::from_millis(90)
            || self.settings_tab_fade.elapsed() < Duration::from_millis(90);
        let moving = self.engine.is_playing()
            || self.meter_db.iter().any(|db| *db > -59.5)
            || self.meter_peak_db.iter().any(|db| *db > -59.5);

        match (
            self.show_settings && !flashing,
            moving || flashing || fading,
        ) {
            (_, false) => IDLE,
            (true, true) => SETTINGS_AUDIO,
            (false, true) => ANIM,
        }
    }

    fn poll_loop_state(&mut self) {
        let previous_loop = self.loop_state.clone();
        self.loop_state = self.engine.loop_state();
        stats::set_loop(match self.loop_state {
            LoopState::Idle => 0,
            LoopState::Playing => 1,
            LoopState::Error(_) => 2,
        });
        if let LoopState::Error(message) = &self.loop_state {
            if !matches!(previous_loop, LoopState::Error(_)) {
                let message = message.clone();
                self.log(StatusKind::Error, format!("Loop track: {message}"));
            }
        }
    }

    fn save_debounced(&mut self) {
        if self.dirty && self.last_save.elapsed() > Duration::from_millis(700) {
            self.save_now();
        }
    }

    fn update_tray_tip(&mut self) {
        let now = Instant::now();
        if now < self.next_tip {
            return;
        }
        self.next_tip = now + Duration::from_secs(3);
        // The voice list is tiny and can briefly hold one id twice (a retrigger
        // before the fade-out finishes), so each id counts once. Counting the
        // ids directly beats building a set per call.
        let active = self.engine.active();
        let mut playing = 0usize;
        for (index, voice) in active.iter().enumerate() {
            if !active[..index]
                .iter()
                .any(|earlier| earlier.sound_id == voice.sound_id)
            {
                playing += 1;
            }
        }
        let total = self.sounds.len();
        let state = (playing, total);
        if self.last_tip == Some(state) {
            return;
        }
        self.last_tip = Some(state);
        let tip = if playing > 0 {
            format!("{APP_NAME} - {playing} playing")
        } else {
            format!("{APP_NAME} - {total} sounds")
        };
        self.hotkeys.set_tooltip(tip);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        // Detect the settings screen opening so its fade starts from zero.
        if self.show_settings && !self.settings_was_open {
            self.settings_opened = Instant::now();
            self.settings_tab_fade = Instant::now();
            self.focus_search = false;
            self.invalidate_playback_stats();
            ctx.memory_mut(|memory| memory.stop_text_input());
        }
        self.settings_was_open = self.show_settings;

        // Debounce filtering: typing must never re-scan the whole library per
        // keystroke, and a huge library must never block the frame.
        let typed = self.search.trim().to_lowercase();
        if typed.is_empty() {
            self.filter_needle.clear();
            self.search_pending = None;
        } else if typed != self.filter_needle {
            match self.search_pending {
                Some(started) if started.elapsed() >= Duration::from_millis(120) => {
                    self.filter_needle = typed;
                    self.search_pending = None;
                }
                Some(_) => ctx.request_repaint_after(Duration::from_millis(60)),
                None => {
                    self.search_pending = Some(Instant::now());
                    ctx.request_repaint_after(Duration::from_millis(120));
                }
            }
        } else {
            self.search_pending = None;
        }
        self.poll_events(ctx);
        self.handle_close_request(ctx);
        if self.quit {
            return;
        }
        // The custom title bar hides decorations, so honor an OS minimize as
        // well (taskbar button, Win+D) when the user asked to minimize to tray.
        self.handle_os_minimize(ctx);

        // Nothing on screen: skip the whole paint pass. The idle timer alone
        // still pays tessellation and a present, which is what kept a hidden
        // window busier than a visible one.
        if self.hidden {
            self.hidden_tick(ctx);
            stats::record_frame(frame_start.elapsed(), true, self.last_interval_ms);
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

        let side = if self.show_settings {
            egui::SidePanel::left("settings_rail")
                .resizable(false)
                .exact_width(200.0)
                .frame(Frame::NONE.fill(theme::panel_deep()).inner_margin(Margin {
                    left: 14,
                    right: 14,
                    top: 14,
                    bottom: 16,
                }))
                .show(ctx, |ui| self.settings_rail(ui))
        } else {
            egui::SidePanel::right("sidebar")
                .resizable(false)
                .exact_width(SIDEBAR_WIDTH)
                .frame(
                    Frame::NONE
                        .fill(theme::panel())
                        // Extra bottom padding so the Settings button never sits
                        // flush against the status bar.
                        .inner_margin(Margin {
                            left: 14,
                            right: 14,
                            top: 12,
                            bottom: 22,
                        }),
                )
                .show(ctx, |ui| self.side_panel(ui))
        };

        // Separator between the rail and the content. Clamp it to the area
        // between the title bar and the status bar so it never bleeds into the
        // status strip (which is drawn on top of the panel's layer).
        let divider = egui::Rangef::new(titlebar_rect.bottom() + 1.0, status_rect.top());
        let edge = if self.show_settings {
            side.response.rect.right()
        } else {
            side.response.rect.left()
        };

        let content_rect = if self.show_settings {
            egui::CentralPanel::default()
                .frame(Frame::NONE.fill(theme::bg()).inner_margin(Margin {
                    left: 28,
                    right: 28,
                    top: 20,
                    bottom: 14,
                }))
                .show(ctx, |ui| {
                    ui.painter().with_clip_rect(screen).vline(
                        edge,
                        divider,
                        Stroke::new(1.0_f32, theme::border()),
                    );
                    self.settings_content(ctx, ui);
                })
                .response
                .rect
        } else {
            egui::CentralPanel::default()
                .frame(
                    Frame::NONE
                        .fill(theme::bg())
                        .inner_margin(Margin::symmetric(14, 10)),
                )
                .show(ctx, |ui| {
                    ui.painter().with_clip_rect(screen).vline(
                        edge,
                        divider,
                        Stroke::new(1.0_f32, theme::border()),
                    );
                    self.main_page(ui);
                })
                .response
                .rect
        };
        // Modals centre on this rect rather than on the window (B13).
        self.content_rect = content_rect;

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

        stats::record_frame(frame_start.elapsed(), false, self.last_interval_ms);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // The loop owns a decoder thread; stop it before the engine is dropped.
        self.engine.stop_loop();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.engine.stop_loop();
        self.cfg.active_library = self.active_library;
        let _ = self.persist();
    }
}

// ---------------------------------------------------------------------------
// Card / list drawing
// ---------------------------------------------------------------------------

/// `now - seconds`, or `now` when the machine has not been up that long.
///
/// `Instant::sub` panics if the result would be before the clock's origin, which
/// is reachable on a freshly booted Windows machine.
fn since(seconds: u64) -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(seconds))
        .unwrap_or_else(Instant::now)
}

/// Lower-cased haystack for search: display name plus file name.
fn search_key(name: &str, path: &Path) -> String {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    format!("{} {}", name.to_lowercase(), file)
}

impl Sound {
    /// Rebuild the cached strings the board paints from. Call after the name or
    /// path changes — this is the only place they are derived.
    fn refresh_labels(&mut self) {
        self.search_key = search_key(&self.name, &self.path);
        self.path_label = self.path.display().to_string();
        self.refresh_hotkey_label();
    }

    /// The pill text only; a rebind changes nothing else, so it does not pay for
    /// the name/path rebuild.
    fn refresh_hotkey_label(&mut self) {
        self.hotkey_label = self.hotkey.map(|hotkey| hotkey.label());
    }
}

/// Duplicate-detection key for a sound file: the canonical path when it
/// resolves, the path as given otherwise, lower-cased because Windows paths
/// compare case-insensitively.
fn canonical_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
}

/// Claim `path` for this import batch; false when an earlier path already
/// claimed the same file.
fn claim_unique(claimed: &mut HashSet<String>, path: &Path) -> bool {
    claimed.insert(canonical_key(path))
}

/// First id free for a batch of new sounds: past everything on the board, and
/// never below the counter that keeps a removed sound's id from coming back.
fn free_sound_id(sounds: &[Sound], next_id: u64) -> u64 {
    next_id.max(sounds.iter().map(|sound| sound.id).max().unwrap_or(0) + 1)
}

/// A sound's only RAM-eligibility rule (FR-5.3.5 + FR-5.2.5): it needs a hotkey
/// and it has to be in the library whose hotkeys are bound. Everything else is
/// streamed from disk, so retention, preloading and the RAM controls all read
/// this one predicate.
fn ram_eligible(sound: &Sound, active_library: u64) -> bool {
    sound.hotkey.is_some() && sound.library_id == active_library
}

/// Re-insert an undone delete at its old index and re-register it with playback.
///
/// The missing flag is judged from the restored path, so a healthy file comes back
/// healthy (the old code asked the board instead, which no longer held the sound,
/// and marked every restored sound missing).
fn restore_sound(sounds: &mut Vec<Sound>, playback: &Playback, undo: UndoDelete) -> u64 {
    let index = undo.index.min(sounds.len());
    let sound = undo.sound;
    let id = sound.id;
    playback.register(id, sound.path.clone(), sound.volume, sound.gain_db);
    // `Playback` owns the "is the file missing" decision. Re-registering already set
    // it from the restored path, so the message is refreshed from that; the old code
    // asked the board, which no longer held the sound, and flagged every restore.
    let missing = playback.info(id).missing;
    playback.mark_missing(id, missing, None);
    sounds.insert(index, sound);
    id
}

/// The sound id under the pointer, if any. The drag rects carry ids so a list
/// that shifted between paint and drop cannot move the wrong sound.
fn drop_target(rects: &[(u64, Rect)], pointer: Pos2) -> Option<u64> {
    rects
        .iter()
        .find(|(_, rect)| rect.contains(pointer))
        .map(|(id, _)| *id)
}

/// Move `id` to `target`'s position, keeping every other sound in order. Returns
/// whether anything moved; a source or target that left the board is skipped.
fn move_sound_to_id(sounds: &mut Vec<Sound>, id: u64, target: u64) -> bool {
    let (Some(from), Some(to)) = (
        sounds.iter().position(|sound| sound.id == id),
        sounds.iter().position(|sound| sound.id == target),
    ) else {
        return false;
    };
    if from == to {
        return false;
    }
    let sound = sounds.remove(from);
    sounds.insert(to, sound);
    true
}

/// One meter slot's motion: instant attack, linear release, peak hold then fall.
/// Takes the slot's values directly so a length mismatch in the slot arrays can
/// never index out of bounds.
fn smooth_meter(
    db: &mut f32,
    peak_db: &mut f32,
    peak_at: &mut Instant,
    level: f32,
    dt: f32,
    now: Instant,
) {
    let target = if level > 0.000_01 {
        (20.0 * level.log10()).clamp(METER_FLOOR_DB, 0.0)
    } else {
        METER_FLOOR_DB
    };
    if target >= *db {
        *db = target;
    } else {
        *db = (*db - METER_RELEASE_DB_PER_SEC * dt)
            .max(target)
            .max(METER_FLOOR_DB);
    }
    if target >= *peak_db {
        *peak_db = target;
        *peak_at = now;
    } else if now.duration_since(*peak_at) > METER_PEAK_HOLD {
        *peak_db = (*peak_db - METER_PEAK_RELEASE_DB_PER_SEC * dt)
            .max(target)
            .max(METER_FLOOR_DB);
    }
}

fn settings_header(ui: &mut Ui, tab: SettingsTab) {
    ui.label(
        RichText::new(tab.title())
            .font(theme::medium(20.0))
            .color(theme::text()),
    );
}

fn settings_group(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui)) {
    ui.label(
        RichText::new(title)
            .font(theme::medium(13.0))
            .color(theme::text()),
    );
    ui.add_space(SETTINGS_GROUP_TITLE_GAP);
    Frame::NONE
        .fill(theme::card())
        .stroke(Stroke::new(1.0_f32, theme::border_soft()))
        .corner_radius(theme::RADIUS_LG)
        .inner_margin(Margin::same(SETTINGS_CARD_PADDING))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
    ui.add_space(SETTINGS_GROUP_GAP);
}

/// Whether a settings row stacks its label above its control instead of sitting
/// beside it (narrow content column).
fn row_stacks(content_width: f32) -> bool {
    content_width < SETTINGS_ROW_STACK_W
}

/// One label/control row: the label on the left, the control flush with the
/// right edge whatever its own width. `control_width` reserves the space the
/// control needs; rows stack on a narrow content column.
fn settings_value(ui: &mut Ui, title: &str, control_width: f32, add_control: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    if row_stacks(width) {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(12.5).color(theme::text()));
            ui.add_space(SETTINGS_STACK_LABEL_GAP);
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), SETTINGS_ROW_H),
                Layout::top_down(Align::Min),
                |ui| add_control(ui),
            );
        });
    } else {
        ui.horizontal_top(|ui| {
            ui.set_min_height(SETTINGS_ROW_H);
            let label_width =
                (width - control_width - SETTINGS_ROW_LABEL_GAP).max(SETTINGS_ROW_LABEL_MIN_W);
            ui.vertical(|ui| {
                ui.set_max_width(label_width);
                ui.label(RichText::new(title).size(12.5).color(theme::text()));
            });
            // Right-to-left, so the control ends flush with the row's edge
            // whether it is a switch, a dropdown or a text button.
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                add_control(ui);
            });
        });
    }
}

/// One boolean setting: a [`settings_value`] row holding a switch. The value is
/// flipped in place; the return value says whether the user changed it, which is
/// when the caller applies the side effects and saves.
///
/// `mixed` paints the half-knob "some but not all" state (a value that is not
/// `true`, yet is not a plain `false` either); it never changes what a click does.
fn settings_toggle(
    ui: &mut Ui,
    id: Id,
    title: &str,
    value: &mut bool,
    mixed: bool,
    enabled: bool,
) -> bool {
    let mut changed = false;
    settings_value(ui, title, SETTINGS_SWITCH_SIZE.x, |ui| {
        changed = settings_switch(ui, id, value, mixed, enabled);
    });
    changed
}

/// The muted one-line explanation under a row.
fn settings_detail(ui: &mut Ui, text: impl Into<String>) {
    ui.add_space(SETTINGS_DETAIL_GAP);
    ui.label(RichText::new(text).size(10.5).color(theme::faint()));
}

fn settings_rule(ui: &mut Ui) {
    let rect = Rect::from_min_size(
        ui.cursor().min,
        Vec2::new(ui.available_width(), SETTINGS_RULE_H),
    );
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::border_soft());
    ui.allocate_space(rect.size());
}

fn settings_divider(ui: &mut Ui) {
    ui.add_space(SETTINGS_DIVIDER_GAP);
    settings_rule(ui);
    ui.add_space(SETTINGS_DIVIDER_GAP);
}

/// A muted text link. Returns true when clicked.
fn link_label(ui: &mut Ui, text: &str, url: &str) -> bool {
    let response = ui.add(
        egui::Label::new(RichText::new(text).size(10.5).color(theme::muted()))
            .sense(Sense::click()),
    );
    let response = response
        .on_hover_text(url)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.clicked()
}

/// Opens an http(s) URL in the default browser. Anything else is ignored so a
/// mistyped constant in `ABOUT_LINKS` cannot hand the shell an arbitrary verb.
fn open_url(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let target = HSTRING::from(url);
    unsafe {
        ShellExecuteW(
            None,
            windows::core::w!("open"),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn settings_switch(ui: &mut Ui, id: Id, value: &mut bool, mixed: bool, enabled: bool) -> bool {
    let inner = ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), SETTINGS_SWITCH_SIZE.y),
        Layout::right_to_left(Align::Min),
        |ui| {
            ui.allocate_exact_size(
                SETTINGS_SWITCH_SIZE,
                if enabled {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            )
        },
    );
    let (rect, response) = inner.inner;
    let hover = anim::hover(ui, id.with("hover"), response.hovered() && enabled);
    let painter = ui.painter();
    if enabled && hover > 0.01 {
        painter.rect_filled(
            rect.expand(3.0),
            CornerRadius::same(theme::RADIUS_MD),
            theme::alpha(theme::accent(), (14.0 * hover) as u8),
        );
    }
    paint_switch(painter, rect, *value, mixed, enabled);
    pressable(ui, id.with("press"), &response, theme::RADIUS_MD, 0.0);

    let clicked = enabled && response.clicked();
    if clicked {
        *value = !*value;
    }
    clicked
}

fn settings_nav_item(ui: &mut Ui, tab: SettingsTab, active: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), SETTINGS_RAIL_ITEM_H),
        Sense::click(),
    );
    let hover = anim::hover(
        ui,
        Id::new(("settings_nav", tab.title())),
        response.hovered(),
    );
    let painter = ui.painter();
    if active {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            theme::alpha(theme::accent(), 34),
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            Stroke::new(1.0_f32, theme::alpha(theme::accent(), 90)),
            StrokeKind::Inside,
        );
    } else if hover > 0.01 {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            theme::alpha(theme::text(), (8.0 * hover) as u8),
        );
    }

    let color = if active {
        theme::accent()
    } else if response.hovered() {
        theme::text()
    } else {
        theme::muted()
    };
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.left() + SETTINGS_RAIL_GLYPH_X, rect.center().y),
        Vec2::splat(15.0),
    );
    paint_settings_icon(painter, icon_rect, tab, color);
    painter.text(
        Pos2::new(rect.left() + SETTINGS_RAIL_TEXT_X, rect.center().y),
        Align2::LEFT_CENTER,
        tab.title(),
        theme::ui_font(12.5),
        color,
    );
    response.clicked()
}

fn settings_back(ui: &mut Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), SETTINGS_RAIL_ITEM_H),
        Sense::click(),
    );
    let hover = anim::hover(ui, Id::new("settings_back"), response.hovered());
    let painter = ui.painter();
    if hover > 0.01 {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            theme::alpha(theme::text(), (8.0 * hover) as u8),
        );
    }
    let color = if response.hovered() {
        theme::text()
    } else {
        theme::muted()
    };
    let stroke = Stroke::new(1.4_f32, color);
    // Same glyph centre and text origin as the nav items, so the whole rail
    // lines up down both edges.
    let center = rect.left_center() + Vec2::new(SETTINGS_RAIL_GLYPH_X, 0.0);
    painter.line_segment([center + Vec2::new(4.0, -4.0), center], stroke);
    painter.line_segment([center, center + Vec2::new(4.0, 4.0)], stroke);
    painter.text(
        rect.left_center() + Vec2::new(SETTINGS_RAIL_TEXT_X, 0.0),
        Align2::LEFT_CENTER,
        "Back",
        theme::ui_font(11.5),
        color,
    );
    response
        .on_hover_text("Back to the soundboard (Esc)")
        .clicked()
}

fn paint_settings_icon(painter: &egui::Painter, rect: Rect, tab: SettingsTab, color: Color32) {
    let center = rect.center();
    match tab {
        SettingsTab::Playback => paint_play_icon(painter, rect, color),
        SettingsTab::Appearance => {
            painter.circle_stroke(center, 6.0, Stroke::new(1.2_f32, color));
            for offset in [
                Vec2::new(-2.2, -2.0),
                Vec2::new(2.2, -1.0),
                Vec2::new(-1.0, 2.4),
            ] {
                painter.circle_filled(center + offset, 1.2, color);
            }
        }
        SettingsTab::Window => {
            let body = Rect::from_center_size(center, Vec2::new(12.0, 10.0));
            painter.rect_stroke(
                body,
                CornerRadius::same(2),
                Stroke::new(1.2_f32, color),
                StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    Pos2::new(body.left(), body.top() + 3.0),
                    Pos2::new(body.right(), body.top() + 3.0),
                ],
                Stroke::new(1.0_f32, color),
            );
        }
        SettingsTab::About => {
            painter.circle_stroke(center, 6.0, Stroke::new(1.2_f32, color));
            painter.circle_filled(center + Vec2::new(0.0, -2.6), 1.0, color);
            painter.line_segment(
                [center + Vec2::new(0.0, -0.6), center + Vec2::new(0.0, 3.8)],
                Stroke::new(1.4_f32, color),
            );
        }
    }
}

/// Width the board's content may use: everything the panel gives us, minus the
/// strip a floating scroll bar draws over. The toolbar and the grid both size
/// themselves from this one number, which is what makes the RAM pill and the
/// last column share a right edge (B4).
fn board_content_width(ui: &Ui) -> f32 {
    let scroll = ui.spacing().scroll;
    // A solid bar already shrinks `available_width`; a floating one does not.
    let gutter = if scroll.floating {
        scroll.bar_width + scroll.bar_outer_margin
    } else {
        0.0
    };
    (ui.available_width() - gutter).max(BOARD_CONTENT_MIN_W)
}

/// Column count and card width for one content width. The cards plus the gaps
/// add back up to exactly `width`, so the last column ends on the content's
/// right edge.
fn grid_geometry(width: f32) -> (usize, f32) {
    let columns =
        (((width + GRID_GAP) / GRID_COLUMN_PITCH).floor() as usize).clamp(1, GRID_MAX_COLUMNS);
    let card = ((width - GRID_GAP * columns.saturating_sub(1) as f32) / columns as f32)
        .max(GRID_MIN_CARD_W);
    (columns, card)
}

/// Search field width: one content width, less the pill and the gap before it.
fn search_field_width(content: f32, pill: f32, gap: f32) -> f32 {
    (content - pill - gap).max(SEARCH_MIN_W)
}

#[allow(clippy::too_many_arguments)]
fn draw_grid(
    ui: &mut Ui,
    sounds: &[Sound],
    libraries: &[Library],
    filtered: &[usize],
    infos: &HashMap<u64, SoundInfo>,
    playing: &HashMap<u64, Option<f32>>,
    drag_enabled: bool,
    capture_id: Option<u64>,
    selected: Option<u64>,
    show_ram: bool,
    rects: &mut Vec<(u64, Rect)>,
    actions: &mut Vec<Action>,
) {
    let spacing = GRID_GAP;
    let width = board_content_width(ui);
    let (columns, card_width) = grid_geometry(width);
    // One stand-in for sounds the registry has no entry for, so neither the map
    // lookup nor its error strings are cloned per item.
    let no_info = SoundInfo::default();

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        for chunk in filtered.chunks(columns) {
            ui.horizontal_top(|ui| {
                for (position, &index) in chunk.iter().enumerate() {
                    if position > 0 {
                        ui.add_space(spacing);
                    }
                    let sound = &sounds[index];
                    let info = infos.get(&sound.id).unwrap_or(&no_info);
                    let state = playing.get(&sound.id).copied();
                    let rect = sound_item(
                        ui,
                        Vec2::new(card_width, CARD_HEIGHT),
                        sound,
                        info,
                        state.is_some(),
                        state.flatten(),
                        drag_enabled,
                        capture_id == Some(sound.id),
                        selected == Some(sound.id),
                        show_ram,
                        libraries,
                        ItemLayout::Grid,
                        actions,
                    );
                    rects.push((sound.id, rect));
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
    libraries: &[Library],
    filtered: &[usize],
    infos: &HashMap<u64, SoundInfo>,
    playing: &HashMap<u64, Option<f32>>,
    drag_enabled: bool,
    capture_id: Option<u64>,
    selected: Option<u64>,
    show_ram: bool,
    rects: &mut Vec<(u64, Rect)>,
    actions: &mut Vec<Action>,
) {
    // One stand-in for sounds the registry has no entry for (see `draw_grid`).
    let no_info = SoundInfo::default();
    ui.horizontal_top(|ui| {
        let available = board_content_width(ui);
        let content_width = available.min(1100.0);
        let pad = ((available - content_width) * 0.5).max(0.0);
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(content_width);
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for &index in filtered {
                let sound = &sounds[index];
                let info = infos.get(&sound.id).unwrap_or(&no_info);
                let state = playing.get(&sound.id).copied();
                let rect = sound_item(
                    ui,
                    Vec2::new(ui.available_width(), ROW_HEIGHT),
                    sound,
                    info,
                    state.is_some(),
                    state.flatten(),
                    drag_enabled,
                    capture_id == Some(sound.id),
                    selected == Some(sound.id),
                    show_ram,
                    libraries,
                    ItemLayout::List,
                    actions,
                );
                rects.push((sound.id, rect));
                ui.add_space(6.0);
            }
        });
    });
}

/// How far a window-centred modal must move so it lands on `content`'s centre.
/// Anchoring is relative to the window, so this is the correction, not a
/// hardcoded offset: half the sidebar left for the board, half the rail right
/// for Settings (B13).
fn content_centre_offset(content: Rect, screen: Rect) -> Vec2 {
    if content.is_positive() {
        content.center() - screen.center()
    } else {
        Vec2::ZERO
    }
}

/// Centre modals over the content area (the board, or the settings body) rather
/// than over the window: an area anchored to the window is pulled left by the
/// 300 px sidebar, which is what put Settings-opened dialogs off-centre (B13).
/// `content` comes from the panel the body was painted into; an unset rect
/// (first frame) falls back to the window centre.
fn centered_modal_area(id: Id, content: Rect, screen: Rect) -> egui::Area {
    egui::Modal::default_area(id).anchor(
        Align2::CENTER_CENTER,
        content_centre_offset(content, screen),
    )
}

/// Shared modal chrome: centred over the content area, transparent backdrop,
/// panel frame, fixed body width, and backdrop-click close. `body` gets the
/// modal's `close` flag so a header X can cancel it.
fn modal_shell(
    ctx: &egui::Context,
    content: Rect,
    id: Id,
    width: f32,
    margin: i8,
    close: &mut bool,
    body: impl FnOnce(&mut Ui, &mut bool),
) {
    let modal = egui::Modal::new(id)
        .area(centered_modal_area(id, content, ctx.screen_rect()))
        .backdrop_color(Color32::TRANSPARENT)
        .frame(
            Frame::NONE
                .fill(theme::panel())
                .stroke(Stroke::new(1.0_f32, theme::border()))
                .corner_radius(theme::RADIUS_XL)
                .inner_margin(Margin::same(margin)),
        )
        .show(ctx, |ui| {
            ui.set_width(width);
            body(ui, close);
        });
    if modal.backdrop_response.clicked() {
        *close = true;
    }
}

fn modal_header(ui: &mut Ui, title: &str, id: Id, close: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .font(theme::medium(20.0))
                .color(theme::text()),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
            let painter = ui.painter().clone();
            let response = close_button(ui, &painter, rect, id);
            if response.clicked() {
                *close = true;
            }
            response.on_hover_text("Close");
        });
    });
}

fn close_button(ui: &mut Ui, painter: &egui::Painter, rect: Rect, id: Id) -> egui::Response {
    let response = ui.interact(rect, id, Sense::click());
    let hovered = response.hovered();
    if hovered {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            theme::alpha(theme::red(), 26),
        );
    }
    let press = anim::press(ui, id, response.is_pointer_button_down_on());
    press_overlay(painter, rect, theme::RADIUS_MD, press);
    let stroke = Stroke::new(
        1.4_f32,
        if hovered {
            theme::red()
        } else {
            theme::muted()
        },
    );
    let center = rect.center();
    // Both diagonals; the press squeezes them slightly toward the centre.
    let arm = 4.0 - press * 0.6;
    painter.line_segment(
        [center + Vec2::new(-arm, -arm), center + Vec2::new(arm, arm)],
        stroke,
    );
    painter.line_segment(
        [center + Vec2::new(arm, -arm), center + Vec2::new(-arm, arm)],
        stroke,
    );
    response
}

/// Removes a sound's binding and the state that depends on it (FR-5.3.5): the
/// hotkey is gone and RAM preloading is off. Returns
/// the dropped hotkey so the caller can name it in the log.
fn unbind(sound: &mut Sound) -> Option<Hotkey> {
    let hotkey = sound.hotkey.take()?;
    sound.ram = false;
    sound.refresh_hotkey_label();
    Some(hotkey)
}

/// Hotkey pill shared by cards and rows. Returns true when clicked. While the
/// sound's chord is being captured the pill turns red, which is what tells the
/// user the dialog belongs to this item.
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
    let hover = anim::hover(ui, id, pill.hovered());
    let border = if capture_active {
        theme::red()
    } else if pill.hovered() {
        theme::accent()
    } else {
        theme::border()
    };
    painter.rect(
        rect,
        CornerRadius::same(theme::RADIUS_MD),
        theme::mix(theme::field(), theme::alpha(theme::accent(), 20), hover),
        Stroke::new(1.0_f32, border),
        StrokeKind::Inside,
    );
    let press = anim::press(ui, id, pill.is_pointer_button_down_on());
    press_overlay(painter, rect, theme::RADIUS_MD, press);
    // The label is cached on the sound; only the colour depends on the modal.
    let (label, color) = match &sound.hotkey_label {
        Some(label) => (
            label.as_str(),
            if capture_active {
                theme::red()
            } else {
                theme::text()
            },
        ),
        None => (HOTKEY_UNBOUND_LABEL, theme::faint()),
    };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::ui_font(font_size),
        color,
    );
    let clicked = pill.clicked();
    // A tooltip cannot show while the item is not hovered, and building it was a
    // string allocation per item per frame.
    if pill.hovered() {
        pill.on_hover_text("Set a global hotkey (works while hidden)");
    }
    clicked
}

/// RAM / disk / missing chip. Returns the action for a click, if any.
#[allow(clippy::too_many_arguments)]
fn ram_chip(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    id: Id,
    sound: &Sound,
    info: &SoundInfo,
    font_size: f32,
    show_ram: bool,
) -> Option<Action> {
    let missing = is_missing(info);
    let has_hotkey = sound.hotkey.is_some();
    if !missing && (!has_hotkey || !show_ram) {
        return None;
    }
    let ram_active = has_hotkey && info.loaded;
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
    let hover = anim::hover(ui, id, chip.hovered());
    painter.rect(
        rect,
        CornerRadius::same(theme::RADIUS_MD),
        theme::alpha(color, (10.0 + 10.0 * hover) as u8),
        Stroke::new(1.0_f32, theme::alpha(color, 60)),
        StrokeKind::Inside,
    );
    let press = anim::press(ui, id, chip.is_pointer_button_down_on());
    press_overlay(painter, rect, theme::RADIUS_MD, press);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::ui_font(font_size),
        color,
    );
    let clicked = chip.clicked();
    if chip.hovered() {
        chip.on_hover_text(if missing {
            "File is missing - click to locate"
        } else if ram_active {
            "In RAM - click to play from disk"
        } else if info.refused {
            "Too long or over the preload budget - always plays from disk"
        } else {
            "Playing from disk - click to keep in RAM"
        });
    }
    if !clicked {
        return None;
    }
    Some(if missing {
        Action::Locate(sound.id)
    } else {
        Action::ToggleRam(sound.id)
    })
}

/// Accent wash for icon-only buttons, driven by an already-tweened hover value.
fn icon_hover(painter: &egui::Painter, rect: Rect, t: f32) {
    if t > 0.01 {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            theme::alpha(theme::accent(), (16.0 * t) as u8),
        );
    }
}

/// A sound is missing when the file could not be loaded or the path lookup
/// failed. Kept in one place so cards, rows and chips agree.
fn is_missing(info: &SoundInfo) -> bool {
    (info.error.is_some() && !info.loading) || info.missing
}
/// Press feedback for a widget that has already been added: the tween and its
/// darkening overlay in one place, so every button presses identically.
fn pressable(ui: &Ui, id: Id, response: &egui::Response, radius: u8, inset: f32) {
    let press = anim::press(ui, id, response.is_pointer_button_down_on());
    press_overlay(ui.painter(), response.rect.shrink(inset), radius, press);
}

/// Darken a control while it is held. One alpha for every button so press
/// feedback feels identical across the app.
fn press_overlay(painter: &egui::Painter, rect: Rect, radius: u8, press: f32) {
    if press <= 0.01 {
        return;
    }
    painter.rect_filled(
        rect,
        CornerRadius::same(radius),
        theme::alpha(theme::bg(), (press * 40.0) as u8),
    );
}

/// Shared fill/border for a sound card or row.
fn sound_surface(
    hover: f32,
    ram_active: bool,
    playing: bool,
    missing: bool,
    selected: bool,
    flash: bool,
) -> (Color32, Color32, f32) {
    let mut fill = theme::card();
    if ram_active {
        fill = theme::mix(fill, theme::purple(), 0.06);
    }
    if playing {
        fill = theme::mix(fill, theme::accent(), 0.10);
    }
    if missing {
        fill = theme::mix(fill, theme::red(), 0.09);
    }
    if hover > 0.0 {
        fill = theme::mix(fill, theme::text(), 0.03 * hover);
    }
    let border = if flash || playing || selected {
        theme::accent()
    } else if missing {
        theme::red()
    } else if ram_active {
        theme::alpha(theme::purple(), 105)
    } else {
        theme::border()
    };
    let width = if selected || flash { 1.5 } else { 1.0 };
    (fill, border, width)
}

fn play_progress(painter: &egui::Painter, track: Rect, progress: f32, alpha: u8) {
    painter.rect_filled(
        track,
        CornerRadius::same(2),
        theme::alpha(theme::accent(), alpha),
    );
    let filled = Rect::from_min_max(
        track.left_top(),
        Pos2::new(
            track.left() + track.width() * progress.clamp(0.0, 1.0),
            track.bottom(),
        ),
    );
    painter.rect_filled(filled, CornerRadius::same(2), theme::accent());
}

/// Which presentation a sound item uses. Both layouts share one implementation;
/// only the geometry differs (see [`item_geometry`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemLayout {
    Grid,
    List,
}

/// Rects, fonts and widget-id tags for one sound item. Kept pure so the layouts
/// can be exercised without an egui context.
///
/// Widget ids stay per-layout (`card_*` vs `row_*`) on purpose: a widget's
/// animation state is stored under its id, so sharing one id across layouts
/// would carry a stale hover/press tween into the other view.
struct ItemGeometry {
    radius: u8,
    /// Grid cards wash the whole surface while a trigger flashes; rows rely on
    /// the accent border alone.
    flash_overlay: bool,
    hover_tag: &'static str,
    name: Rect,
    name_font: FontId,
    pill: Rect,
    pill_tag: &'static str,
    pill_font: f32,
    /// `None` when the layout reserves no room for the chip (nothing to show).
    chip: Option<Rect>,
    chip_tag: &'static str,
    chip_font: f32,
    close: Rect,
    close_tag: &'static str,
    /// `None` when the item is too narrow for a usable track.
    volume: Option<Rect>,
    volume_tag: &'static str,
    /// Rows hide the speaker glyph and percentage to save horizontal space.
    volume_compact: bool,
    progress: Rect,
    progress_alpha: u8,
}

fn item_geometry(
    layout: ItemLayout,
    rect: Rect,
    has_hotkey: bool,
    show_ram: bool,
    missing: bool,
) -> ItemGeometry {
    match layout {
        ItemLayout::Grid => {
            let pill_width = if has_hotkey { 78.0 } else { 88.0 };
            let pill = Rect::from_min_size(
                rect.left_bottom() + Vec2::new(12.0, -34.0),
                Vec2::new(pill_width, 24.0),
            );
            ItemGeometry {
                radius: theme::RADIUS_LG,
                flash_overlay: true,
                hover_tag: "card_hover",
                name: Rect::from_min_max(
                    rect.left_top() + Vec2::new(13.0, 30.0),
                    Pos2::new(rect.right() - 38.0, rect.top() + 68.0),
                ),
                name_font: theme::medium(17.0),
                pill,
                pill_tag: "hotkey",
                pill_font: 12.5,
                // Compact RAM / disk / missing chip at the top-left.
                chip: Some(Rect::from_min_size(
                    rect.left_top() + Vec2::new(10.0, 8.0),
                    Vec2::new(54.0, 20.0),
                )),
                chip_tag: "ram",
                chip_font: 10.5,
                // Close button: clean vector X, exactly centered.
                close: Rect::from_min_size(
                    rect.right_top() + Vec2::new(-30.0, 7.0),
                    Vec2::new(22.0, 22.0),
                ),
                close_tag: "close",
                volume: Some(Rect::from_min_max(
                    Pos2::new(pill.right() + 10.0, rect.bottom() - 34.0),
                    Pos2::new(rect.right() - 12.0, rect.bottom() - 10.0),
                )),
                volume_tag: "cardvol",
                volume_compact: false,
                progress: Rect::from_min_max(
                    Pos2::new(rect.left() + 10.0, rect.bottom() - 4.0),
                    Pos2::new(rect.right() - 10.0, rect.bottom() - 2.0),
                ),
                progress_alpha: 45,
            }
        }
        ItemLayout::List => {
            // Controls on the right: chip, hotkey pill and close.
            let pill_width = if has_hotkey { 110.0 } else { 118.0 };
            let indicator_width = if (has_hotkey && show_ram) || missing {
                70.0
            } else {
                0.0
            };
            let controls_left = rect.right() - 42.0 - pill_width - indicator_width;

            // Name column; the volume track uses the remaining space up to the
            // controls instead of floating as a short bar in a maximized window.
            let name = Rect::from_min_max(
                rect.left_center() + Vec2::new(16.0, -14.0),
                Pos2::new(rect.left() + rect.width() * 0.38, rect.center().y + 14.0),
            );
            let pill = Rect::from_center_size(
                Pos2::new(rect.right() - 42.0 - pill_width * 0.5, rect.center().y),
                Vec2::new(pill_width, 30.0),
            );
            let chip = (indicator_width > 0.0).then(|| {
                Rect::from_center_size(
                    Pos2::new(
                        pill.left() - 8.0 - (indicator_width - 8.0) * 0.5,
                        rect.center().y,
                    ),
                    Vec2::new(indicator_width - 8.0, 28.0),
                )
            });
            let volume_left = name.right() + 16.0;
            let volume_right = controls_left - 16.0;
            let slider_width = (volume_right - volume_left).max(0.0);
            ItemGeometry {
                radius: theme::RADIUS_MD,
                flash_overlay: false,
                hover_tag: "row_hover",
                name,
                name_font: theme::medium(15.0),
                pill,
                pill_tag: "rowhotkey",
                pill_font: 13.5,
                chip,
                chip_tag: "rowram",
                chip_font: 11.0,
                close: Rect::from_center_size(
                    Pos2::new(rect.right() - 18.0, rect.center().y),
                    Vec2::new(26.0, 26.0),
                ),
                close_tag: "rowclose",
                volume: (slider_width >= MIN_VOLUME_WIDTH).then(|| {
                    Rect::from_min_size(
                        Pos2::new(volume_left, rect.center().y - 11.0),
                        Vec2::new(slider_width, 22.0),
                    )
                }),
                volume_tag: "rowvol",
                volume_compact: true,
                progress: Rect::from_min_max(
                    Pos2::new(rect.left() + 8.0, rect.bottom() - 3.0),
                    Pos2::new(rect.right() - 8.0, rect.bottom() - 1.0),
                ),
                progress_alpha: 40,
            }
        }
    }
}

/// One sound item for both the grid and the list. Painting follows the list's
/// order (name, pill, chip, close, volume, progress); the layouts never overlap
/// those rects, so the order is invisible. Actions are appended to the caller's
/// buffer, so a frame of cards allocates nothing per item.
#[allow(clippy::too_many_arguments)]
fn sound_item(
    ui: &mut Ui,
    size: Vec2,
    sound: &Sound,
    info: &SoundInfo,
    playing: bool,
    progress: Option<f32>,
    drag_enabled: bool,
    capture_active: bool,
    selected: bool,
    show_ram: bool,
    libraries: &[Library],
    layout: ItemLayout,
    actions: &mut Vec<Action>,
) -> Rect {
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter().clone();
    let has_hotkey = sound.hotkey.is_some();
    let ram_active = has_hotkey && info.loaded;
    let missing = is_missing(info);
    let flash = sound
        .flash_until
        .map(|t| t > Instant::now())
        .unwrap_or(false);
    let geometry = item_geometry(layout, rect, has_hotkey, show_ram, missing);

    let hover = anim::hover(
        ui,
        Id::new((geometry.hover_tag, sound.id)),
        response.hovered(),
    );
    let (fill, border, width) = sound_surface(hover, ram_active, playing, missing, selected, flash);
    let radius = CornerRadius::same(geometry.radius);
    painter.rect(
        rect,
        radius,
        fill,
        Stroke::new(width, border),
        StrokeKind::Inside,
    );
    if geometry.flash_overlay && flash {
        painter.rect(
            rect,
            radius,
            theme::alpha(theme::accent(), 18),
            Stroke::NONE,
            StrokeKind::Inside,
        );
    }

    if response.clicked() {
        actions.push(Action::Play(sound.id));
    }
    if drag_enabled && response.drag_started() {
        let drag_size = match layout {
            ItemLayout::Grid => size,
            ItemLayout::List => Vec2::new(ROW_DRAG_WIDTH, ROW_HEIGHT),
        };
        actions.push(Action::DragStart {
            id: sound.id,
            size: drag_size,
        });
    }
    if response.drag_stopped() {
        actions.push(Action::DragStop);
    }
    response.context_menu(|ui| {
        sound_context_menu(ui, sound, info, libraries, &mut *actions);
    });

    // Name.
    let name = theme::truncate(
        &painter,
        &sound.name,
        geometry.name_font.clone(),
        geometry.name.width(),
    );
    painter.text(
        geometry.name.left_center(),
        Align2::LEFT_CENTER,
        name,
        geometry.name_font,
        if missing {
            theme::mix(theme::text(), theme::red(), 0.4)
        } else {
            theme::text()
        },
    );

    // Hotkey pill.
    if hotkey_pill(
        ui,
        &painter,
        geometry.pill,
        ui.id().with((geometry.pill_tag, sound.id)),
        sound,
        capture_active,
        geometry.pill_font,
    ) {
        actions.push(Action::SetHotkey(sound.id));
    }

    // RAM / disk / missing chip. Nothing is emitted or stored when `None`.
    if let Some(chip) = geometry.chip {
        if let Some(action) = ram_chip(
            ui,
            &painter,
            chip,
            ui.id().with((geometry.chip_tag, sound.id)),
            sound,
            info,
            geometry.chip_font,
            show_ram,
        ) {
            actions.push(action);
        }
    }

    let close = close_button(
        ui,
        &painter,
        geometry.close,
        ui.id().with((geometry.close_tag, sound.id)),
    );
    if close.clicked() {
        actions.push(Action::Remove(sound.id));
    }
    if close.hovered() {
        close.on_hover_text("Remove sound");
    }

    if let Some(volume_rect) = geometry.volume {
        volume::bar(
            ui,
            volume_rect,
            ui.id().with((geometry.volume_tag, sound.id)),
            sound.volume,
            |v| actions.push(Action::Volume(sound.id, v)),
            geometry.volume_compact,
        );
    }

    // Playing progress line. Absent until the total duration is known, so a
    // streamed sound never shows a bar pegged at 100%.
    if let Some(progress) = progress {
        play_progress(
            &painter,
            geometry.progress,
            progress,
            geometry.progress_alpha,
        );
    }

    if response.double_clicked() {
        actions.push(Action::Rename(sound.id));
    }
    // Rows name the decode error instead of the path; cards always show the path.
    // Both are stable strings and only needed while hovered — building them per
    // item per frame was pure waste.
    if response.hovered() {
        let tooltip = match (layout, &info.error) {
            (ItemLayout::List, Some(error)) => error.as_str(),
            _ => sound.path_label.as_str(),
        };
        response = response.on_hover_text(tooltip);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand);

    rect
}

/// The per-sound trim row: −, the signed value, +, and a reset once it is not
/// unity. Clicks step by [`GAIN_STEP_DB`] and keep the menu open, so trimming a
/// loud file takes one visit. Cards and rows share it via `sound_context_menu`.
fn gain_row(ui: &mut Ui, sound: &Sound, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Gain").size(11.5).color(theme::muted()));
        if tiny_button_with_id(ui, Id::new(("gain-down", sound.id)), "\u{2212}").clicked() {
            actions.push(Action::Gain(sound.id, sound.gain_db - GAIN_STEP_DB));
        }
        ui.label(
            RichText::new(format_gain_db(sound.gain_db))
                .size(11.5)
                .color(theme::text())
                .monospace(),
        );
        if tiny_button_with_id(ui, Id::new(("gain-up", sound.id)), "+").clicked() {
            actions.push(Action::Gain(sound.id, sound.gain_db + GAIN_STEP_DB));
        }
        // The reset appears only when it would do something.
        if sound.gain_db.abs() > 0.001
            && tiny_button_with_id(ui, Id::new(("gain-reset", sound.id)), "reset")
                .on_hover_text("Back to 0.0 dB")
                .clicked()
        {
            actions.push(Action::Gain(sound.id, 0.0));
        }
    });
}

fn sound_context_menu(
    ui: &mut Ui,
    sound: &Sound,
    info: &SoundInfo,
    libraries: &[Library],
    actions: &mut Vec<Action>,
) {
    if ui.button("Play").clicked() {
        actions.push(Action::Play(sound.id));
        ui.close_menu();
    }
    if ui.button("Stop").clicked() {
        actions.push(Action::Stop(sound.id));
        ui.close_menu();
    }
    ui.separator();
    gain_row(ui, sound, actions);
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
    if libraries.len() > 1 {
        ui.menu_button("Move to", |ui| {
            for library in libraries {
                if library.id == sound.library_id {
                    continue;
                }
                if ui.button(&library.name).clicked() {
                    actions.push(Action::MoveSound(sound.id, library.id));
                    ui.close_menu();
                }
            }
        });
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
    if (info.error.is_some() || info.missing) && ui.button("Locate file...").clicked() {
        actions.push(Action::Locate(sound.id));
        ui.close_menu();
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

// ---------------------------------------------------------------------------
// Small widgets
// ---------------------------------------------------------------------------

fn library_tab_width(ui: &Ui, name: &str, active: bool) -> f32 {
    let font = if active {
        theme::medium(13.0)
    } else {
        theme::ui_font(13.0)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font, theme::text());
    (galley.size().x + 24.0).clamp(58.0, 168.0)
}

fn paint_library_tab(painter: &egui::Painter, rect: Rect, name: &str, active: bool, hover: f32) {
    if active {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            theme::alpha(theme::accent(), 14),
        );
    } else if hover > 0.01 {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_MD),
            theme::alpha(theme::text(), (10.0 * hover) as u8),
        );
    }
    let font = if active {
        theme::medium(13.0)
    } else {
        theme::ui_font(13.0)
    };
    let color = if active {
        theme::text()
    } else {
        theme::mix(theme::muted(), theme::text(), hover * 0.4)
    };
    let label = theme::truncate(painter, name, font.clone(), rect.width() - 16.0);
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font, color);
    if active {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.left() + 8.0, rect.bottom() - 2.0),
                Pos2::new(rect.right() - 8.0, rect.bottom()),
            ),
            CornerRadius::same(2),
            theme::accent(),
        );
    }
}

fn paint_box_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let gap = 2.0;
    let size = (rect.width() - gap) * 0.5;
    for row in 0..2 {
        for col in 0..2 {
            let cell = Rect::from_min_size(
                Pos2::new(
                    rect.left() + col as f32 * (size + gap),
                    rect.top() + row as f32 * (size + gap),
                ),
                Vec2::splat(size),
            );
            painter.rect_filled(cell, CornerRadius::same(2), color);
        }
    }
}

fn paint_list_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.6_f32, color);
    for row in 0..3 {
        let y = rect.top() + 1.5 + row as f32 * 3.5;
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            stroke,
        );
    }
}

fn plus_button(ui: &mut Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    let painter = ui.painter();
    let hover = anim::hover(ui, Id::new("plus_hover"), response.hovered());
    icon_hover(painter, rect, hover);
    let press = anim::press(ui, Id::new("plus"), response.is_pointer_button_down_on());
    press_overlay(painter, rect, theme::RADIUS_MD, press);
    let color = theme::mix(theme::muted(), theme::accent(), hover);
    let stroke = Stroke::new(1.4_f32, color);
    let center = rect.center();
    painter.line_segment(
        [center - Vec2::new(4.5, 0.0), center + Vec2::new(4.5, 0.0)],
        stroke,
    );
    painter.line_segment(
        [center - Vec2::new(0.0, 4.5), center + Vec2::new(0.0, 4.5)],
        stroke,
    );
    let clicked = response.clicked();
    response.on_hover_text("New library");
    clicked
}

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
    .stroke(Stroke::new(
        1.0_f32,
        if enabled {
            theme::accent()
        } else {
            theme::border()
        },
    ))
    .corner_radius(theme::RADIUS_MD);
    let mut response = ui.add_sized(size, button);
    if !enabled {
        response = response.on_disabled_hover_text("Not available");
    }
    pressable(
        ui,
        Id::new(("accent", text, size.x as i32, size.y as i32)),
        &response,
        theme::RADIUS_MD,
        2.0,
    );
    response
}

fn ghost_button(ui: &mut Ui, text: &str, size: Vec2) -> egui::Response {
    ghost_button_colored(ui, text, size, theme::text())
}

/// Ghost button with a caller-chosen label colour, for destructive actions that
/// must not look like the neutral buttons beside them.
fn ghost_button_colored(ui: &mut Ui, text: &str, size: Vec2, color: Color32) -> egui::Response {
    let response = ui.add_sized(
        size,
        Button::new(RichText::new(text).size(13.5).color(color))
            .fill(theme::card())
            .stroke(Stroke::new(1.0_f32, theme::border()))
            .corner_radius(theme::RADIUS_MD),
    );
    pressable(
        ui,
        Id::new(("ghost", text, size.x as i32, size.y as i32)),
        &response,
        theme::RADIUS_MD,
        2.0,
    );
    response
}

fn tiny_button(ui: &mut Ui, text: &str) -> egui::Response {
    tiny_button_with_id(ui, Id::new(("tiny", text)), text)
}

/// [`tiny_button`] with an explicit id, for a row whose label is shared by more
/// than one button in the frame (the per-sound gain stepper).
fn tiny_button_with_id(ui: &mut Ui, id: Id, text: &str) -> egui::Response {
    let response =
        ui.add(Button::new(RichText::new(text).size(11.5).color(theme::muted())).frame(false));
    pressable(ui, id, &response, theme::RADIUS_SM, 0.0);
    response
}

fn section_line(ui: &mut Ui) {
    let rect = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), 1.0));
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::border_soft());
    ui.allocate_space(rect.size());
}

fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}

fn paint_logo(painter: &egui::Painter, rect: Rect) {
    painter.rect(
        rect,
        CornerRadius::same(theme::RADIUS_MD),
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
            if i == 1 {
                theme::accent()
            } else {
                theme::text()
            },
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
        CornerRadius::same(theme::RADIUS_MD),
        if linked {
            theme::alpha(theme::accent(), 24)
        } else {
            theme::field()
        },
        Stroke::new(
            1.0_f32,
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
        if linked {
            theme::accent()
        } else {
            theme::muted()
        },
        linked,
    );
}

#[derive(Clone, Copy)]
enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

fn window_button(ui: &mut Ui, kind: WindowButton, tray_available: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(36.0, 28.0), Sense::click());
    let hovered = response.hovered();
    let close = matches!(kind, WindowButton::Close);
    if hovered {
        ui.painter().rect_filled(
            rect.shrink2(Vec2::new(2.0, 2.0)),
            CornerRadius::same(theme::RADIUS_MD),
            if close {
                theme::alpha(theme::red(), 50)
            } else {
                theme::card_hover()
            },
        );
    }
    let press = anim::press(
        ui,
        Id::new(("window_button", kind as i32)),
        response.is_pointer_button_down_on(),
    );
    press_overlay(
        ui.painter(),
        rect.shrink2(Vec2::new(2.0, 2.0)),
        theme::RADIUS_MD,
        press,
    );
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
            ui.painter().line_segment(
                [
                    Pos2::new(c.x - 5.0, c.y + 4.0),
                    Pos2::new(c.x + 5.0, c.y + 4.0),
                ],
                stroke,
            );
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
            ui.painter()
                .line_segment([c + Vec2::new(-4.5, -4.5), c + Vec2::new(4.5, 4.5)], stroke);
            ui.painter()
                .line_segment([c + Vec2::new(4.5, -4.5), c + Vec2::new(-4.5, 4.5)], stroke);
        }
    }
    let tooltip = match kind {
        WindowButton::Minimize => {
            if tray_available {
                "Hide to tray (hotkeys keep working)"
            } else {
                "Minimize"
            }
        }
        WindowButton::Maximize => "Maximize",
        WindowButton::Close => "Quit SoundDeck",
    };
    let response = response.on_hover_text(tooltip);
    response.clicked()
}

fn format_gain_db(db: f32) -> String {
    if db.abs() < 0.05 {
        "0.0 dB".to_string()
    } else {
        // Signed on purpose: the sign carries the meaning (boost or trim).
        format!("{db:+.1} dB")
    }
}

fn level_x(bar: Rect, db: f32) -> f32 {
    let t = (db.clamp(-60.0, 0.0) + 60.0) / 60.0;
    bar.left() + bar.width() * t
}

fn draw_level_bar(painter: &egui::Painter, bar: Rect, db: f32, peak_db: f32) {
    painter.rect_filled(bar, CornerRadius::same(2), theme::field());
    let zones = [
        (-60.0f32, -18.0f32, theme::meter_low()),
        (-18.0, -6.0, theme::meter_mid()),
        (-6.0, 0.0, theme::meter_high()),
    ];
    let level = level_x(bar, db);
    for (low, high, color) in zones {
        let left = level_x(bar, low);
        let right = level_x(bar, high);
        let zone = Rect::from_min_max(Pos2::new(left, bar.top()), Pos2::new(right, bar.bottom()));
        painter.rect_filled(zone, CornerRadius::ZERO, theme::alpha(color, 34));
        if level > left {
            let fill =
                Rect::from_min_max(zone.left_top(), Pos2::new(level.min(right), bar.bottom()));
            painter.rect_filled(fill, CornerRadius::ZERO, color);
        }
    }
    if peak_db > -59.5 {
        let x = level_x(bar, peak_db);
        painter.line_segment(
            [
                Pos2::new(x, bar.top() - 1.0),
                Pos2::new(x, bar.bottom() + 1.0),
            ],
            Stroke::new(1.0_f32, theme::text()),
        );
    }
}

fn paint_play_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let center = rect.center();
    let size = rect.width().min(rect.height()) * 0.34;
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(center.x - size * 0.55, center.y - size),
            Pos2::new(center.x - size * 0.55, center.y + size),
            Pos2::new(center.x + size, center.y),
        ],
        color,
        Stroke::NONE,
    ));
}

fn paint_stop_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let side = rect.width().min(rect.height()) * 0.52;
    painter.rect_filled(
        Rect::from_center_size(rect.center(), Vec2::splat(side)),
        CornerRadius::same(2),
        color,
    );
}

fn paint_folder_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let width = rect.width();
    let height = rect.height();
    let stroke = Stroke::new(1.2_f32, color);
    let body = Rect::from_min_max(
        Pos2::new(rect.left() + width * 0.1, rect.top() + height * 0.32),
        Pos2::new(rect.right() - width * 0.1, rect.bottom() - height * 0.12),
    );
    painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
    painter.line_segment(
        [
            Pos2::new(rect.left() + width * 0.22, rect.top() + height * 0.32),
            Pos2::new(rect.left() + width * 0.22, rect.top() + height * 0.2),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(rect.left() + width * 0.22, rect.top() + height * 0.2),
            Pos2::new(rect.left() + width * 0.48, rect.top() + height * 0.2),
        ],
        stroke,
    );
}

/// Shared switch styling for the sidebar and Settings.
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
        if enabled {
            theme::text()
        } else {
            theme::faint()
        },
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

/// The stored label for "this slot is deliberately silent" — the device
/// selector's None row (FR-5.5.9).
///
/// Silence is stored as the slot's *name* with no id, because a settings field
/// cannot express "chosen to be silent" and "never configured" at once: an empty
/// pair is the first-run state that startup resolution fills in, while this label
/// is a choice that survives a restart. It is also what the selector shows, so the
/// board names the state the same way the user picked it.
const SILENT_DEVICE_LABEL: &str = "None (silent)";

/// Whether the user chose "None (silent)" for this slot.
fn slot_is_silent(cfg: &storage::Settings, slot: Slot) -> bool {
    let (id, name) = slot_endpoint(cfg, slot);
    id.is_none() && name.as_deref() == Some(SILENT_DEVICE_LABEL)
}

/// The stored endpoint (id, friendly name) for one slot. One mapping, read by
/// routing, the device refresh and the device selector.
fn slot_endpoint(cfg: &storage::Settings, slot: Slot) -> (&Option<String>, &Option<String>) {
    match slot {
        Slot::Monitor => (&cfg.monitor_device, &cfg.monitor_name),
        Slot::Cable => (&cfg.cable_device, &cfg.cable_name),
    }
}

/// Write both halves of a slot's endpoint at once, so an id and the label that
/// describes it can never disagree.
fn set_slot_endpoint(
    cfg: &mut storage::Settings,
    slot: Slot,
    id: Option<String>,
    name: Option<String>,
) {
    match slot {
        Slot::Monitor => {
            cfg.monitor_device = id;
            cfg.monitor_name = name;
        }
        Slot::Cable => {
            cfg.cable_device = id;
            cfg.cable_name = name;
        }
    }
}

/// A device whose name says it is a virtual cable. The one predicate for "this
/// is probably the streaming endpoint": startup resolution and the routing
/// fallback both read it.
fn looks_virtual(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("cable") || name.contains("virtual")
}

/// Where one slot should play now, and what had to be substituted to get there.
struct SlotRoute {
    /// A real device from the enumeration — never a configured id dressed up as
    /// one, so the engine always talks to something that exists.
    device: Option<DeviceInfo>,
    /// Set when the configured device is gone: `"Headphones" not found - using
    /// "Speakers"`, for the log and the Settings status line (FR-5.5.8).
    substituted: Option<String>,
}

/// Resolve one slot against the live device list, keeping the stored endpoint
/// untouched so a re-plugged device comes back on the next refresh (FR-5.5.7).
///
/// Order: the stored id, then the stored friendly name, then a substitute —
/// the Windows default, then the first device; the Cable prefers a device whose
/// name says "cable"/"virtual" and never the one the Monitor already uses.
///
/// A slot the user set to "None (silent)" resolves to no device at all: it is
/// not "unconfigured", and substituting a device here is exactly the bug where
/// the previous output kept playing after the user switched the slot off
/// (FR-5.5.9).
fn route_slot(
    devices: &[DeviceInfo],
    default_id: Option<&str>,
    slot: Slot,
    cfg: &storage::Settings,
    monitor: Option<&str>,
) -> SlotRoute {
    if slot_is_silent(cfg, slot) {
        return SlotRoute {
            device: None,
            substituted: None,
        };
    }
    let (id, name) = slot_endpoint(cfg, slot);
    if let Some(device) = resolve_device(devices, id.as_deref(), name.as_deref()) {
        return SlotRoute {
            device: Some(device),
            substituted: None,
        };
    }

    let free = |device: &&DeviceInfo| monitor.map(|id| device.id != id).unwrap_or(true);
    let by_default = || {
        default_id
            .and_then(|id| {
                devices
                    .iter()
                    .find(|device| device.id == id && free(device))
            })
            .cloned()
    };
    let first = || devices.iter().find(|device| free(device)).cloned();
    let fallback = match slot {
        Slot::Monitor => by_default().or_else(first),
        Slot::Cable => devices
            .iter()
            .find(|device| looks_virtual(&device.name) && free(device))
            .cloned()
            .or_else(by_default)
            .or_else(first),
    };

    // Nothing configured and nothing to fall back to is the first-run state, not
    // a failure; only a configured-but-gone device is worth reporting.
    let configured = name.clone().or_else(|| id.clone());
    let substituted = fallback.as_ref().and_then(|device| {
        configured
            .map(|configured| format!("\"{configured}\" not found - using \"{}\"", device.name))
    });

    SlotRoute {
        device: fallback,
        substituted,
    }
}

/// What the configured endpoints resolved to.
struct OutputSelection {
    monitor: Option<DeviceInfo>,
    cable: Option<DeviceInfo>,
    /// Log line when a virtual cable had to be guessed ("Auto-selected …").
    note: Option<String>,
}

/// The one place Monitor/Cable are resolved from settings at startup: the stored
/// id, then the friendly name, then (Monitor) the Windows default and the first
/// device; the Cable falls back to a device whose name says "cable"/"virtual".
/// Both results are written back into `cfg` so the next launch resolves the same
/// endpoints.
///
/// A slot the user set to "None (silent)" is left empty and stays silent: the
/// stored choice is not overwritten by a fresh guess (FR-5.5.9).
///
/// Startup, config import and "Reset all settings" all go through here. Runtime
/// re-routing deliberately does not: it keeps the stored endpoint and reports a
/// substitution instead (`route_slot`).
fn resolve_outputs(
    devices: &[DeviceInfo],
    default_id: Option<&str>,
    cfg: &mut storage::Settings,
) -> OutputSelection {
    let monitor_silent = slot_is_silent(cfg, Slot::Monitor);
    let monitor = if monitor_silent {
        None
    } else {
        resolve_device(
            devices,
            cfg.monitor_device.as_deref(),
            cfg.monitor_name.as_deref(),
        )
        .or_else(|| {
            default_id.and_then(|id| devices.iter().find(|device| device.id == id).cloned())
        })
        .or_else(|| devices.first().cloned())
    };

    let mut note = None;
    let cable_silent = slot_is_silent(cfg, Slot::Cable);
    let mut cable = if cable_silent {
        None
    } else {
        resolve_device(
            devices,
            cfg.cable_device.as_deref(),
            cfg.cable_name.as_deref(),
        )
    };
    if cable.is_none() && !cable_silent {
        cable = devices
            .iter()
            .find(|device| {
                looks_virtual(&device.name)
                    && monitor
                        .as_ref()
                        .map(|monitor| monitor.id != device.id)
                        .unwrap_or(true)
            })
            .cloned();
        if let Some(found) = &cable {
            note = Some(format!("Auto-selected virtual cable: {}", found.name));
        }
    }

    if let Some(device) = &monitor {
        set_slot_endpoint(
            cfg,
            Slot::Monitor,
            Some(device.id.clone()),
            Some(device.name.clone()),
        );
    }
    if let Some(device) = &cable {
        set_slot_endpoint(
            cfg,
            Slot::Cable,
            Some(device.id.clone()),
            Some(device.name.clone()),
        );
    }

    OutputSelection {
        monitor,
        cable,
        note,
    }
}

/// Linked outputs are one level: the cable follows the monitor.
fn link_output_volumes(cfg: &mut storage::Settings) {
    if cfg.link_volumes {
        cfg.cable_volume = cfg.monitor_volume;
    }
}

/// Register every board sound with the playback registry. Startup and config
/// import both use it, so an imported board is registered exactly like a loaded
/// one.
fn register_sounds(playback: &Playback, sounds: &[Sound]) {
    for sound in sounds {
        playback.register(sound.id, sound.path.clone(), sound.volume, sound.gain_db);
    }
}

/// Build the runtime tabs/sounds from their storage shape. Returns the warnings
/// the board produced (invalid or blocked hotkeys) for the caller to log.
///
/// Startup and config import both go through this, so a bundle that imports
/// builds the same board a restart would. A chord capture refuses is unbindable
/// here too (F5): the sound is kept, only its binding is dropped.
fn build_board(
    stored: &[storage::LibraryConfig],
    preload_hotkey_sounds: bool,
) -> (Vec<Library>, Vec<Sound>, Vec<String>) {
    let mut libraries = Vec::with_capacity(stored.len());
    let mut sounds = Vec::new();
    let mut warnings = Vec::new();

    for library in stored {
        libraries.push(Library {
            id: library.id,
            name: library.name.clone(),
        });
        for sound in &library.sounds {
            let hotkey = match sound.hotkey.as_deref().map(parse_bindable) {
                Some(Ok(hotkey)) => Some(hotkey),
                Some(Err(err)) => {
                    warnings.push(format!("Hotkey for \"{}\" is invalid: {err}", sound.name));
                    None
                }
                None => None,
            };
            let mut entry = Sound {
                id: sound.id,
                name: sound.name.clone(),
                path: model::resolve_path(&sound.path),
                hotkey,
                volume: sound.volume.clamp(0.0, 1.0),
                gain_db: sound.gain_db.clamp(SOUND_GAIN_MIN_DB, SOUND_GAIN_MAX_DB),
                // Only a bound sound can be retained (FR-5.3.5).
                ram: hotkey.is_some() && sound.ram.unwrap_or(preload_hotkey_sounds),
                library_id: library.id,
                search_key: String::new(),
                hotkey_label: None,
                path_label: String::new(),
                flash_until: None,
            };
            entry.refresh_labels();
            sounds.push(entry);
        }
    }

    (libraries, sounds, warnings)
}

#[cfg(test)]
mod tests {
    // Volume curve coverage lives in `ui::volume::tests`.

    use super::*;

    fn bound_sound() -> Sound {
        let mut sound = Sound {
            id: 7,
            name: "Airhorn".to_string(),
            path: PathBuf::from("airhorn.wav"),
            hotkey: Some(Hotkey::parse("Ctrl+1").expect("test hotkey parses")),
            volume: 1.0,
            gain_db: 0.0,
            ram: true,
            library_id: 1,
            search_key: String::new(),
            hotkey_label: None,
            path_label: String::new(),
            flash_until: None,
        };
        sound.refresh_labels();
        sound
    }

    fn sound_with_id(id: u64) -> Sound {
        let mut sound = bound_sound();
        sound.id = id;
        sound
    }

    /// A real file on disk, so `Playable` can stat and lock it. Content is never
    /// decoded in these tests.
    fn temp_audio_path(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "sounddeck-test-{tag}-{}-{}.wav",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::write(&path, [0u8; 16]).expect("write temp audio");
        path
    }

    #[test]
    fn undo_restores_a_present_file_without_flagging_it_missing() {
        let path = temp_audio_path("undo");
        let playback = Playback::new(Arc::new(Engine::new(1.0, 1.0, 0.0)));

        let mut present = sound_with_id(11);
        present.path = path.clone();
        let mut sounds = Vec::new();
        assert_eq!(
            restore_sound(
                &mut sounds,
                &playback,
                UndoDelete {
                    sound: present,
                    index: 0,
                }
            ),
            11
        );
        let info = playback.info(11);
        assert!(!info.missing, "an existing file must not come back missing");
        assert!(info.error.is_none());

        // The old bug judged from the board, which no longer held the sound, so
        // this case came back flagged too. A genuinely absent file still is.
        let gone = std::env::temp_dir().join("sounddeck-test-undo-gone.wav");
        let mut absent = sound_with_id(12);
        absent.path = gone;
        let id = restore_sound(
            &mut sounds,
            &playback,
            UndoDelete {
                sound: absent,
                index: 1,
            },
        );
        assert_eq!(id, 12);
        assert!(playback.info(12).missing);
        assert_eq!(sounds.len(), 2, "both sounds are on the board again");
        assert_eq!(sounds[1].id, 12, "restored at the recorded index");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn only_hotkey_sounds_in_the_active_library_are_ram_eligible() {
        let sound = bound_sound();
        assert!(ram_eligible(&sound, 1));
        assert!(
            !ram_eligible(&sound, 2),
            "another tab's sounds are never preloaded"
        );
        let mut unbound = bound_sound();
        unbound.hotkey = None;
        assert!(!ram_eligible(&unbound, 1), "no hotkey, no retention");
    }

    #[test]
    fn relinking_rebuilds_the_search_key_and_the_tooltip() {
        let mut sound = bound_sound();
        sound.path = PathBuf::from("old").join("Clown_Horn.wav");
        sound.refresh_labels();
        assert!(sound.search_key.contains("airhorn"));
        assert!(sound.search_key.contains("clown_horn"));
        assert!(sound.path_label.ends_with("Clown_Horn.wav"));

        sound.path = PathBuf::from("new").join("Party_Horn.wav");
        sound.refresh_labels();
        assert!(
            !sound.search_key.contains("clown_horn"),
            "the old file name must not linger"
        );
        assert!(sound.search_key.contains("party_horn"));
        assert!(
            sound.path_label.ends_with("Party_Horn.wav"),
            "the card tooltip follows the new path"
        );

        // The pill text is cached beside them and follows the binding.
        assert_eq!(sound.hotkey_label.as_deref(), Some("Ctrl+1"));
        sound.hotkey = None;
        sound.refresh_hotkey_label();
        assert_eq!(sound.hotkey_label, None);
    }

    #[test]
    fn a_drop_resolves_its_target_by_id() {
        let rects = vec![(7u64, Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)))];
        assert_eq!(drop_target(&rects, Pos2::new(5.0, 5.0)), Some(7));
        assert_eq!(drop_target(&rects, Pos2::new(50.0, 5.0)), None);
        assert_eq!(drop_target(&[], Pos2::ZERO), None);
    }

    #[test]
    fn moving_a_sound_keeps_the_rest_in_order() {
        let mut sounds = vec![sound_with_id(1), sound_with_id(2), sound_with_id(3)];
        // Dropping onto id 3 inserts at its pre-removal index, exactly as the old
        // index-based code did.
        assert!(move_sound_to_id(&mut sounds, 1, 3));
        let ids: Vec<u64> = sounds.iter().map(|sound| sound.id).collect();
        assert_eq!(ids, vec![2, 3, 1]);

        assert!(
            !move_sound_to_id(&mut sounds, 1, 1),
            "a self-drop is a no-op"
        );
        assert!(
            !move_sound_to_id(&mut sounds, 99, 2),
            "a stale source is skipped"
        );
        assert!(
            !move_sound_to_id(&mut sounds, 2, 99),
            "a stale target is skipped"
        );
        let ids: Vec<u64> = sounds.iter().map(|sound| sound.id).collect();
        assert_eq!(ids, vec![2, 3, 1], "skipped drops never reorder");
    }

    #[test]
    fn the_meter_attacks_instantly_and_decays_to_the_floor() {
        let mut db = METER_FLOOR_DB;
        let mut peak = METER_FLOOR_DB;
        let mut peak_at = Instant::now();
        let now = Instant::now();

        // -20 dBFS is shown in the same frame it arrives.
        smooth_meter(&mut db, &mut peak, &mut peak_at, 0.1, 0.033, now);
        assert!((db + 20.0).abs() < 0.01);
        assert!((peak + 20.0).abs() < 0.01);

        // Silence releases by the configured rate, never below the floor.
        smooth_meter(&mut db, &mut peak, &mut peak_at, 0.0, 0.1, now);
        assert!(db < -20.0);
        let later = now + METER_PEAK_HOLD + Duration::from_millis(100);
        for _ in 0..200 {
            smooth_meter(&mut db, &mut peak, &mut peak_at, 0.0, 0.1, later);
        }
        assert_eq!(db, METER_FLOOR_DB);
        assert_eq!(peak, METER_FLOOR_DB);
    }

    #[test]
    fn unbinding_clears_the_hotkey_and_ram_preload() {
        let mut sound = bound_sound();
        let dropped = unbind(&mut sound).map(|hotkey| hotkey.label());
        assert_eq!(dropped, Some("Ctrl+1".to_string()));
        assert_eq!(sound.hotkey, None);
        assert!(!sound.ram);
        assert_eq!(
            sound.hotkey_label, None,
            "the pill text follows the binding"
        );
        // Unbinding a sound that has no hotkey is a no-op, never a panic.
        assert_eq!(unbind(&mut sound), None);
        assert_eq!(sound.hotkey, None);
    }

    /// One bulk import must claim each file once and hand out fresh ids, without
    /// re-canonicalizing the library for every candidate.
    #[test]
    fn a_bulk_import_claims_each_file_once() {
        let path = temp_audio_path("claim");
        let mut claimed = HashSet::new();
        assert!(claim_unique(&mut claimed, &path));
        assert!(
            !claim_unique(&mut claimed, &path),
            "the same file is a duplicate"
        );
        // Windows resolves the same file through a differently-cased path.
        let shouty = PathBuf::from(path.to_string_lossy().to_uppercase());
        assert!(
            !claim_unique(&mut claimed, &shouty),
            "case is not a distinct file"
        );

        // A path that does not resolve still has a stable key of its own.
        let missing = std::env::temp_dir().join("sounddeck-test-claim-missing.wav");
        assert!(claim_unique(&mut claimed, &missing));
        assert!(!claim_unique(&mut claimed, &missing));

        let _ = std::fs::remove_file(&path);
    }

    /// An imported bundle builds the same board a restart would: paths resolve
    /// against the app folder, RAM follows a binding only, and a hand-edited
    /// hotkey becomes a warning instead of a lost sound.
    #[test]
    fn an_imported_board_rebuilds_like_startup() {
        let stored = vec![storage::LibraryConfig {
            id: 3,
            name: "Main".into(),
            sounds: vec![
                storage::SoundConfig {
                    id: 1,
                    name: "Bound".into(),
                    path: "audio/bound.wav".into(),
                    hotkey: Some("Ctrl+1".into()),
                    volume: 2.0,
                    gain_db: 40.0,
                    ram: None,
                },
                storage::SoundConfig {
                    id: 2,
                    name: "Broken".into(),
                    path: "audio/broken.wav".into(),
                    hotkey: Some("Ctrl+Nope".into()),
                    volume: 0.5,
                    gain_db: 0.0,
                    ram: Some(true),
                },
            ],
        }];

        let (libraries, sounds, warnings) = build_board(&stored, true);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].id, 3);
        assert_eq!(sounds.len(), 2, "a bad hotkey never drops the sound");
        assert_eq!(sounds[0].path, model::resolve_path("audio/bound.wav"));
        assert_eq!(sounds[0].library_id, 3);
        assert_eq!(sounds[0].volume, 1.0, "volumes are clamped on the way in");
        assert_eq!(
            sounds[0].gain_db, SOUND_GAIN_MAX_DB,
            "trims are clamped on the way in"
        );
        assert!(sounds[0].ram, "a bound sound follows the preload default");
        assert!(!sounds[0].search_key.is_empty());
        assert!(sounds[1].hotkey.is_none());
        assert!(!sounds[1].ram, "no binding, no retention");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Broken"), "{warnings:?}");

        // With the global preload off, a sound without a hotkey stays on disk.
        let (_, sounds, _) = build_board(&stored, false);
        assert!(!sounds[0].ram);
    }

    /// A blocked chord that arrives through a hand-edited config is unbound, and
    /// the sound itself is kept: capture refuses the same chord (F5).
    #[test]
    fn a_stored_blocked_chord_is_unbound_and_the_sound_kept() {
        let stored = vec![storage::LibraryConfig {
            id: 1,
            name: "Main".into(),
            sounds: vec![storage::SoundConfig {
                id: 1,
                name: "Paste".into(),
                path: "audio/paste.wav".into(),
                hotkey: Some("Ctrl+V".into()),
                volume: 1.0,
                gain_db: 0.0,
                ram: Some(true),
            }],
        }];

        let (_, sounds, warnings) = build_board(&stored, true);

        assert_eq!(sounds.len(), 1, "only the binding is dropped");
        assert_eq!(sounds[0].hotkey, None);
        assert!(!sounds[0].ram, "no binding, no retention");
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("Ctrl+V is a system shortcut and can't be used"),
            "{warnings:?}"
        );
    }

    #[test]
    fn new_sound_ids_start_past_everything_on_the_board() {
        let empty: Vec<Sound> = Vec::new();
        assert_eq!(free_sound_id(&empty, 1), 1);
        let sounds = vec![sound_with_id(4), sound_with_id(9)];
        assert_eq!(free_sound_id(&sounds, 1), 10, "past the highest id");
        assert_eq!(
            free_sound_id(&sounds, 20),
            20,
            "the counter wins when it is already ahead"
        );
    }

    /// The activity log refreshes a repeated message and drops the oldest once
    /// full, instead of shifting a vector on every overflow.
    #[test]
    fn the_activity_log_dedupes_and_caps() {
        let mut statuses = VecDeque::new();
        for index in 0..MAX_STATUS_ENTRIES + 5 {
            record_status(&mut statuses, StatusKind::Info, format!("entry {index}"));
        }
        assert_eq!(statuses.len(), MAX_STATUS_ENTRIES);
        assert_eq!(
            statuses.front().map(|entry| entry.text.as_str()),
            Some("entry 5"),
            "the oldest entries fall off the front"
        );

        let before = statuses.len();
        record_status(&mut statuses, StatusKind::Error, "entry 204".to_string());
        assert_eq!(statuses.len(), before, "a repeat refreshes, never stacks");
        let newest = statuses.back().expect("the log has an entry");
        assert_eq!(newest.text, "entry 204");
        assert!(
            matches!(newest.kind, StatusKind::Error),
            "the newer kind wins"
        );
    }

    /// Grid and list used to be two renderers; this pins the geometry decisions
    /// the merged one still makes.
    #[test]
    fn item_geometry_matches_the_layout_it_renders() {
        let card = Rect::from_min_size(Pos2::ZERO, Vec2::new(240.0, CARD_HEIGHT));
        let grid = item_geometry(ItemLayout::Grid, card, true, true, false);
        assert_eq!(grid.radius, theme::RADIUS_LG);
        assert!(grid.flash_overlay);
        let chip = grid.chip.expect("cards always draw the chip");
        assert!(card.contains_rect(chip));
        assert!(
            grid.name.top() >= chip.bottom(),
            "the name starts below the chip"
        );
        let volume = grid.volume.expect("cards always draw the volume bar");
        assert!(card.contains_rect(volume));

        let row = Rect::from_min_size(Pos2::ZERO, Vec2::new(700.0, ROW_HEIGHT));
        let list = item_geometry(ItemLayout::List, row, true, true, false);
        assert_eq!(list.radius, theme::RADIUS_MD);
        assert!(!list.flash_overlay);
        assert!(row.contains_rect(list.chip.expect("RAM chip while the state is shown")));
        let volume = list.volume.expect("a wide row has room for the track");
        assert!(row.contains_rect(volume));
        assert!(
            volume.left() >= list.name.right(),
            "the track starts after the name"
        );

        // RAM state hidden and the file healthy: no chip, and its space goes to
        // the volume track.
        let dense = item_geometry(ItemLayout::List, row, true, false, false);
        assert!(dense.chip.is_none());
        assert!(dense.volume.expect("volume").width() > volume.width());

        // Too narrow for a usable track: dropped instead of squashed.
        let tight = Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, ROW_HEIGHT));
        assert!(item_geometry(ItemLayout::List, tight, true, true, false)
            .volume
            .is_none());
    }

    /// A settings row stacks below the width breakpoint and keeps the control
    /// beside the label above it, with room for both.
    #[test]
    fn settings_rows_stack_only_below_the_breakpoint() {
        assert!(row_stacks(SETTINGS_ROW_STACK_W - 1.0));
        assert!(!row_stacks(SETTINGS_ROW_STACK_W));
        assert!(
            !row_stacks(SETTINGS_CONTENT_MAX_W),
            "the widest settings column keeps rows side by side"
        );
        for control in [
            SETTINGS_SWITCH_SIZE.x,
            SETTINGS_BUTTON_SIZE.x,
            SETTINGS_THEME_W,
            SETTINGS_TEXT_COL_W,
        ] {
            assert!(
                SETTINGS_ROW_STACK_W - control - SETTINGS_ROW_LABEL_GAP >= SETTINGS_ROW_LABEL_MIN_W,
                "a {control} px control leaves no room for the label at the breakpoint"
            );
        }
    }

    /// The board's one right edge (B4): cards plus gaps fill the content width
    /// exactly, and the toolbar's search field plus gap plus pill do too — so
    /// the last column and the RAM pill end on the same pixel at any width.
    #[test]
    fn the_grid_and_the_toolbar_share_one_content_right_edge() {
        // Board widths for the 960 px default and a 1200 px window: the window
        // less the sidebar and the central panel's 14 px side margins.
        let board_width = |window: f32| window - SIDEBAR_WIDTH - 2.0 * 14.0;
        for window in [960.0_f32, 1200.0] {
            let content = board_width(window);
            assert!(content >= BOARD_CONTENT_MIN_W);

            let (columns, card) = grid_geometry(content);
            assert!((1..=GRID_MAX_COLUMNS).contains(&columns));
            let filled = card * columns as f32 + GRID_GAP * (columns - 1) as f32;
            assert!(
                (filled - content).abs() < 0.01,
                "the grid fills {content} px, not {filled} px, at {window} px"
            );

            let search = search_field_width(content, 164.0, 10.0);
            assert!((search + 10.0 + 164.0 - content).abs() < 0.01);
            // With RAM state hidden the field takes the whole row.
            assert!((search_field_width(content, 0.0, 0.0) - content).abs() < 0.01);
        }
    }

    /// No column count or rounding leaves a gap at the right edge.
    #[test]
    fn the_grid_geometry_never_undershoots_its_width() {
        for width in (BOARD_CONTENT_MIN_W as u32..=1400).step_by(7) {
            let width = width as f32;
            let (columns, card) = grid_geometry(width);
            let filled = card * columns as f32 + GRID_GAP * (columns - 1) as f32;
            assert!(
                (filled - width).abs() < 0.01,
                "{width} px of content left {filled} px of cards"
            );
        }
    }

    /// Right-click on the master gain returns it to unity, which is a value the
    /// slider itself can produce.
    #[test]
    fn the_master_gain_reset_lands_on_unity_inside_the_slider_range() {
        assert_eq!(MASTER_GAIN_NEUTRAL_DB, 0.0, "0 dB is unity gain");
        assert!(MASTER_GAIN_RANGE.contains(&MASTER_GAIN_NEUTRAL_DB));
        assert!(MASTER_GAIN_RANGE.contains(&3.5), "3.5 dB is a legal value");
    }

    /// Modals centre on the body they belong to (B13). The board's content is
    /// left of the window centre (sidebar on the right); the settings body is
    /// right of it (rail on the left) — never the other way round.
    #[test]
    fn a_modal_centres_on_the_content_rect_not_the_window() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(960.0, 540.0));

        let board = Rect::from_min_max(
            Pos2::new(0.0, 38.0),
            Pos2::new(screen.right() - SIDEBAR_WIDTH, screen.bottom() - 34.0),
        );
        let offset = content_centre_offset(board, screen);
        assert!(
            (offset.x + SIDEBAR_WIDTH * 0.5).abs() < 0.01,
            "a board modal sits half a sidebar left of the window centre"
        );
        assert!(
            (screen.center() + offset - board.center()).length() < 0.01,
            "the anchored centre lands on the board's centre"
        );

        let settings = Rect::from_min_max(
            Pos2::new(200.0, 38.0),
            Pos2::new(screen.right(), screen.bottom() - 34.0),
        );
        let offset = content_centre_offset(settings, screen);
        assert!(
            offset.x > 0.0,
            "a settings modal moves right, never left of centre"
        );
        assert!((screen.center() + offset - settings.center()).length() < 0.01);

        // Nothing painted yet: no correction rather than a NaN anchor.
        assert_eq!(content_centre_offset(Rect::ZERO, screen), Vec2::ZERO);
    }

    fn device(id: &str, name: &str) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            name: name.into(),
        }
    }

    /// A configured device that is gone must surface a real substitute instead of
    /// a fabricated endpoint, and the stored id must survive so it comes back.
    #[test]
    fn a_gone_monitor_falls_back_to_the_default_and_says_which_one_plays() {
        let devices = vec![device("speakers", "Speakers"), device("hdmi", "HDMI")];
        let mut cfg = storage::Settings::default();
        set_slot_endpoint(
            &mut cfg,
            Slot::Monitor,
            Some("gone".into()),
            Some("Headphones".into()),
        );

        let route = route_slot(&devices, Some("speakers"), Slot::Monitor, &cfg, None);
        assert_eq!(
            route.device.as_ref().map(|d| d.id.as_str()),
            Some("speakers"),
            "the Windows default is the substitute"
        );
        assert_eq!(
            route.substituted.as_deref(),
            Some("\"Headphones\" not found - using \"Speakers\"")
        );
        assert_eq!(
            cfg.monitor_device.as_deref(),
            Some("gone"),
            "the configured id is kept for a re-plug"
        );
    }

    /// Nothing configured is the first-run state: use the default quietly. An
    /// empty device list is "not connected", not a substitution either.
    #[test]
    fn an_unconfigured_slot_takes_the_default_without_a_warning() {
        let devices = vec![device("speakers", "Speakers")];
        let cfg = storage::Settings::default();
        let route = route_slot(&devices, Some("speakers"), Slot::Monitor, &cfg, None);
        assert_eq!(route.device.map(|d| d.id), Some("speakers".to_string()));
        assert!(route.substituted.is_none());

        let empty: Vec<DeviceInfo> = Vec::new();
        let route = route_slot(&empty, None, Slot::Monitor, &cfg, None);
        assert!(route.device.is_none());
        assert!(route.substituted.is_none());
    }

    /// "None (silent)" is a stored choice, not an empty endpoint: both routing
    /// and startup resolution must leave the slot with no device, otherwise the
    /// output the user switched off keeps playing (FR-5.5.9).
    #[test]
    fn a_silent_slot_is_never_given_a_substitute_device() {
        let devices = vec![
            device("speakers", "Speakers"),
            device("cable-in", "CABLE Input (VB-Audio)"),
        ];
        let mut cfg = storage::Settings::default();
        set_slot_endpoint(
            &mut cfg,
            Slot::Monitor,
            None,
            Some(SILENT_DEVICE_LABEL.into()),
        );
        assert!(slot_is_silent(&cfg, Slot::Monitor));
        assert!(
            !slot_is_silent(&cfg, Slot::Cable),
            "an unconfigured slot is not a silent one"
        );

        let route = route_slot(&devices, Some("speakers"), Slot::Monitor, &cfg, None);
        assert!(
            route.device.is_none(),
            "\"None (silent)\" must not be substituted: that is how the old device kept playing"
        );
        assert!(
            route.substituted.is_none(),
            "a choice is not a substitution"
        );

        // Startup resolution must keep the choice instead of guessing again.
        let selection = resolve_outputs(&devices, Some("speakers"), &mut cfg);
        assert!(selection.monitor.is_none());
        assert_eq!(
            cfg.monitor_name.as_deref(),
            Some(SILENT_DEVICE_LABEL),
            "the stored silence survives a restart"
        );
        assert!(cfg.monitor_device.is_none());
        // The other slot is still resolved normally.
        assert_eq!(
            selection.cable.as_ref().map(|d| d.id.as_str()),
            Some("cable-in")
        );
        assert_eq!(cfg.cable_device.as_deref(), Some("cable-in"));
    }

    /// The cable's substitute is a virtual device when one is attached, and never
    /// the endpoint the monitor already plays on.
    #[test]
    fn a_gone_cable_prefers_a_virtual_device_over_the_default() {
        let devices = vec![
            device("speakers", "Speakers"),
            device("cable-in", "CABLE Input (VB-Audio)"),
            device("hdmi", "HDMI"),
        ];
        let mut cfg = storage::Settings {
            monitor_device: Some("speakers".into()),
            ..storage::Settings::default()
        };
        set_slot_endpoint(
            &mut cfg,
            Slot::Cable,
            Some("gone".into()),
            Some("VB-Audio".into()),
        );

        let route = route_slot(
            &devices,
            Some("speakers"),
            Slot::Cable,
            &cfg,
            Some("speakers"),
        );
        assert_eq!(
            route.device.as_ref().map(|d| d.id.as_str()),
            Some("cable-in")
        );
        let note = route.substituted.expect("a substitution is reported");
        assert!(note.contains("CABLE Input"), "{note}");

        // Without a virtual device the cable takes the default, but still not the
        // one the monitor is using.
        let only_plain = vec![device("speakers", "Speakers"), device("hdmi", "HDMI")];
        let route = route_slot(
            &only_plain,
            Some("speakers"),
            Slot::Cable,
            &cfg,
            Some("speakers"),
        );
        assert_eq!(route.device.as_ref().map(|d| d.id.as_str()), Some("hdmi"));
        assert!(looks_virtual("CABLE Input") && looks_virtual("Virtual Audio"));
        assert!(!looks_virtual("Speakers"));
    }

    /// The id wins over a stale name, and a device that came back under a new id
    /// is still found by its name.
    #[test]
    fn a_device_is_found_by_id_first_and_by_name_second() {
        let devices = vec![
            device("speakers", "Speakers (Realtek)"),
            device("new-cable", "CABLE Input"),
        ];
        assert_eq!(
            resolve_device(&devices, Some("speakers"), Some("Old speakers")).map(|d| d.name),
            Some("Speakers (Realtek)".to_string())
        );
        assert_eq!(
            resolve_device(&devices, Some("old-id"), Some("CABLE Input")).map(|d| d.id),
            Some("new-cable".to_string())
        );
        assert!(resolve_device(&devices, Some("old-id"), Some("Old cable")).is_none());
    }

    /// A start clears a failure note but not a substitution: audio is still
    /// coming out of a device the user did not pick (FR-5.5.8).
    #[test]
    fn a_substitution_stays_reported_after_its_route_starts() {
        let note = DeviceNote::warning("\"Headphones\" not found - using \"Speakers\"");
        let kept = note
            .cleared_by_start()
            .expect("a substitution survives a successful start");
        assert!(kept.text.contains("not found"));
        assert!(matches!(kept.kind, StatusKind::Warning));
        assert!(DeviceNote::failure("device in use")
            .cleared_by_start()
            .is_none());
    }

    /// The trimmed dB reads with a sign, so a boost and a cut never look alike.
    #[test]
    fn a_gain_reads_with_an_explicit_sign() {
        assert_eq!(format_gain_db(0.0), "0.0 dB");
        assert_eq!(format_gain_db(3.0), "+3.0 dB");
        assert_eq!(format_gain_db(-3.0), "-3.0 dB");
        assert_eq!(format_gain_db(0.01), "0.0 dB");
    }
}
