//! Global hotkeys (low-level keyboard hook) + system tray, running on one
//! background thread with a hidden Win32 message window.
//!
//! The hook observes keys but always passes them to the focused app, so a
//! `Shift+T` hotkey also types `T` in Notepad. Sounds are triggered by posting
//! to the hidden window; the hook callback stays short.
//!
//! This module also owns the two pieces of hotkey knowledge the rest of the app
//! asks for: the key table (`KEYS`, used by parsing, display and capture) and the
//! duplicate-chord rules (`chord_owner`, `duplicate_sounds`, FR-5.7.5).

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;

// ---------------------------------------------------------------------------
// Hotkey model / parsing (platform independent, unit tested)
// ---------------------------------------------------------------------------

/// Virtual-key codes used by the key table and the modifier reader. Names follow
/// the Win32 SDK so a reader can look them up; the hook reports the left/right
/// variants, other callers report the generic ones.
const VK_BACK: u32 = 0x08;
const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;
const VK_INSERT: u32 = 0x2D;
const VK_DELETE: u32 = 0x2E;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
/// `VK_F1`; `VK_F1..=VK_F24` are contiguous, so the table uses `VK_F1 + n`.
const VK_F1: u32 = 0x70;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
const VK_OEM_1: u32 = 0xBA;
const VK_OEM_PLUS: u32 = 0xBB;
const VK_OEM_COMMA: u32 = 0xBC;
const VK_OEM_MINUS: u32 = 0xBD;
const VK_OEM_PERIOD: u32 = 0xBE;
const VK_OEM_2: u32 = 0xBF;
const VK_OEM_3: u32 = 0xC0;
const VK_OEM_4: u32 = 0xDB;
const VK_OEM_5: u32 = 0xDC;
const VK_OEM_6: u32 = 0xDD;
const VK_OEM_7: u32 = 0xDE;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Mods {
    pub fn prefix(self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.win {
            out.push_str("Win+");
        }
        out
    }

    /// True when no modifier is held (the capture dialog uses it to spot a plain
    /// `Backspace`/`Delete` press that clears the field).
    pub fn is_empty(self) -> bool {
        !(self.ctrl || self.alt || self.shift || self.win)
    }

    /// Modifier state derived from the virtual keys physically held, so chord
    /// matching and the tracked key state can never disagree. Accepts the
    /// left/right codes the hook reports and the generic codes other callers use.
    pub fn from_keys(keys: &[u32]) -> Self {
        fn held(keys: &[u32], group: &[u32]) -> bool {
            keys.iter().any(|vk| group.contains(vk))
        }
        Self {
            ctrl: held(keys, &[VK_CONTROL, VK_LCONTROL, VK_RCONTROL]),
            alt: held(keys, &[VK_MENU, VK_LMENU, VK_RMENU]),
            shift: held(keys, &[VK_SHIFT, VK_LSHIFT, VK_RSHIFT]),
            win: held(keys, &[VK_LWIN, VK_RWIN]),
        }
    }
}
#[cfg(windows)]
fn find_binding(bindings: &[(Hotkey, u64)], mods: Mods, vk: u32) -> Option<u64> {
    bindings
        .iter()
        .find(|(hotkey, _)| hotkey.vk == vk && hotkey.mods == mods)
        .map(|(_, id)| *id)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub mods: Mods,
    pub vk: u32,
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.mods.prefix(), vk_name(self.vk))
    }
}

impl Hotkey {
    pub fn label(&self) -> String {
        self.to_string()
    }

    /// Parse a stored/typed hotkey such as "Alt+1", "Ctrl+Shift+S", "F8".
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Empty hotkey".into());
        }
        let mut mods = Mods::default();
        let mut key: Option<&str> = None;
        for raw in text.split('+') {
            let part = raw.trim();
            if part.is_empty() {
                return Err(format!("Invalid hotkey: {text}"));
            }
            let lower = part.to_ascii_lowercase();
            match lower.as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "alt" | "option" => mods.alt = true,
                "shift" => mods.shift = true,
                "win" | "windows" | "super" | "meta" => mods.win = true,
                _ => {
                    if key.is_some() {
                        return Err(format!("More than one key in \"{text}\""));
                    }
                    key = Some(part);
                }
            }
        }
        let key = key.ok_or_else(|| format!("No key in \"{text}\""))?;
        let vk = vk_from_name(key).ok_or_else(|| format!("Unknown key \"{key}\""))?;
        Ok(Self { mods, vk })
    }

    /// Convert a captured egui key press into a Windows virtual-key hotkey.
    pub fn from_egui(key: egui::Key, modifiers: &egui::Modifiers) -> Option<Self> {
        Some(Self {
            mods: mods_from_egui(modifiers),
            vk: vk_from_egui(key)?,
        })
    }
}

/// One bindable key: its stored/displayed name, virtual-key code, the egui keys
/// that produce it, and extra spellings accepted when parsing. Parsing, display
/// and the capture dialog all read this table, so the three cannot drift apart.
struct KeySpec {
    name: &'static str,
    vk: u32,
    egui: &'static [egui::Key],
    aliases: &'static [&'static str],
}

impl KeySpec {
    const fn new(name: &'static str, vk: u32, egui: &'static [egui::Key]) -> Self {
        Self {
            name,
            vk,
            egui,
            aliases: &[],
        }
    }

    /// Also accept `aliases` (lowercase) when parsing a stored or typed hotkey.
    const fn aliased(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }
}

/// The one key table (AGENTS §6.1). `name` is the spelling written to
/// `settings.json` and shown in the UI, so the order and wording here define the
/// stored format.
const KEYS: &[KeySpec] = &[
    // Named keys.
    KeySpec::new("Space", VK_SPACE, &[egui::Key::Space]).aliased(&["spacebar"]),
    KeySpec::new("Tab", VK_TAB, &[egui::Key::Tab]),
    KeySpec::new("Enter", VK_RETURN, &[egui::Key::Enter]).aliased(&["return"]),
    KeySpec::new("Esc", VK_ESCAPE, &[egui::Key::Escape]).aliased(&["escape"]),
    KeySpec::new("Backspace", VK_BACK, &[egui::Key::Backspace]).aliased(&["bksp"]),
    KeySpec::new("Delete", VK_DELETE, &[egui::Key::Delete]).aliased(&["del"]),
    KeySpec::new("Insert", VK_INSERT, &[egui::Key::Insert]).aliased(&["ins"]),
    KeySpec::new("Home", VK_HOME, &[egui::Key::Home]),
    KeySpec::new("End", VK_END, &[egui::Key::End]),
    KeySpec::new("PageUp", VK_PRIOR, &[egui::Key::PageUp]).aliased(&["pgup"]),
    KeySpec::new("PageDown", VK_NEXT, &[egui::Key::PageDown]).aliased(&["pgdn"]),
    KeySpec::new("Up", VK_UP, &[egui::Key::ArrowUp]).aliased(&["arrowup"]),
    KeySpec::new("Down", VK_DOWN, &[egui::Key::ArrowDown]).aliased(&["arrowdown"]),
    KeySpec::new("Left", VK_LEFT, &[egui::Key::ArrowLeft]).aliased(&["arrowleft"]),
    KeySpec::new("Right", VK_RIGHT, &[egui::Key::ArrowRight]).aliased(&["arrowright"]),
    // Punctuation. The stored spelling is the character itself and the aliases
    // are egui's own names, so a captured key always parses back.
    KeySpec::new(";", VK_OEM_1, &[egui::Key::Semicolon]).aliased(&["semicolon"]),
    KeySpec::new("=", VK_OEM_PLUS, &[egui::Key::Equals, egui::Key::Plus])
        .aliased(&["equals", "plus"]),
    KeySpec::new(",", VK_OEM_COMMA, &[egui::Key::Comma]).aliased(&["comma"]),
    KeySpec::new("-", VK_OEM_MINUS, &[egui::Key::Minus]).aliased(&["minus"]),
    KeySpec::new(".", VK_OEM_PERIOD, &[egui::Key::Period]).aliased(&["period"]),
    KeySpec::new("/", VK_OEM_2, &[egui::Key::Slash]).aliased(&["slash"]),
    KeySpec::new("`", VK_OEM_3, &[egui::Key::Backtick]).aliased(&["backtick", "grave"]),
    KeySpec::new("[", VK_OEM_4, &[egui::Key::OpenBracket]).aliased(&["openbracket"]),
    KeySpec::new("\\", VK_OEM_5, &[egui::Key::Backslash]).aliased(&["backslash"]),
    KeySpec::new("]", VK_OEM_6, &[egui::Key::CloseBracket]).aliased(&["closebracket"]),
    KeySpec::new("'", VK_OEM_7, &[egui::Key::Quote]).aliased(&["quote"]),
    // Digits and letters: the virtual-key code is the ASCII code.
    KeySpec::new("0", b'0' as u32, &[egui::Key::Num0]),
    KeySpec::new("1", b'1' as u32, &[egui::Key::Num1]),
    KeySpec::new("2", b'2' as u32, &[egui::Key::Num2]),
    KeySpec::new("3", b'3' as u32, &[egui::Key::Num3]),
    KeySpec::new("4", b'4' as u32, &[egui::Key::Num4]),
    KeySpec::new("5", b'5' as u32, &[egui::Key::Num5]),
    KeySpec::new("6", b'6' as u32, &[egui::Key::Num6]),
    KeySpec::new("7", b'7' as u32, &[egui::Key::Num7]),
    KeySpec::new("8", b'8' as u32, &[egui::Key::Num8]),
    KeySpec::new("9", b'9' as u32, &[egui::Key::Num9]),
    KeySpec::new("A", b'A' as u32, &[egui::Key::A]),
    KeySpec::new("B", b'B' as u32, &[egui::Key::B]),
    KeySpec::new("C", b'C' as u32, &[egui::Key::C]),
    KeySpec::new("D", b'D' as u32, &[egui::Key::D]),
    KeySpec::new("E", b'E' as u32, &[egui::Key::E]),
    KeySpec::new("F", b'F' as u32, &[egui::Key::F]),
    KeySpec::new("G", b'G' as u32, &[egui::Key::G]),
    KeySpec::new("H", b'H' as u32, &[egui::Key::H]),
    KeySpec::new("I", b'I' as u32, &[egui::Key::I]),
    KeySpec::new("J", b'J' as u32, &[egui::Key::J]),
    KeySpec::new("K", b'K' as u32, &[egui::Key::K]),
    KeySpec::new("L", b'L' as u32, &[egui::Key::L]),
    KeySpec::new("M", b'M' as u32, &[egui::Key::M]),
    KeySpec::new("N", b'N' as u32, &[egui::Key::N]),
    KeySpec::new("O", b'O' as u32, &[egui::Key::O]),
    KeySpec::new("P", b'P' as u32, &[egui::Key::P]),
    KeySpec::new("Q", b'Q' as u32, &[egui::Key::Q]),
    KeySpec::new("R", b'R' as u32, &[egui::Key::R]),
    KeySpec::new("S", b'S' as u32, &[egui::Key::S]),
    KeySpec::new("T", b'T' as u32, &[egui::Key::T]),
    KeySpec::new("U", b'U' as u32, &[egui::Key::U]),
    KeySpec::new("V", b'V' as u32, &[egui::Key::V]),
    KeySpec::new("W", b'W' as u32, &[egui::Key::W]),
    KeySpec::new("X", b'X' as u32, &[egui::Key::X]),
    KeySpec::new("Y", b'Y' as u32, &[egui::Key::Y]),
    KeySpec::new("Z", b'Z' as u32, &[egui::Key::Z]),
    // Function keys, `VK_F1..=VK_F24`.
    KeySpec::new("F1", VK_F1, &[egui::Key::F1]),
    KeySpec::new("F2", VK_F1 + 1, &[egui::Key::F2]),
    KeySpec::new("F3", VK_F1 + 2, &[egui::Key::F3]),
    KeySpec::new("F4", VK_F1 + 3, &[egui::Key::F4]),
    KeySpec::new("F5", VK_F1 + 4, &[egui::Key::F5]),
    KeySpec::new("F6", VK_F1 + 5, &[egui::Key::F6]),
    KeySpec::new("F7", VK_F1 + 6, &[egui::Key::F7]),
    KeySpec::new("F8", VK_F1 + 7, &[egui::Key::F8]),
    KeySpec::new("F9", VK_F1 + 8, &[egui::Key::F9]),
    KeySpec::new("F10", VK_F1 + 9, &[egui::Key::F10]),
    KeySpec::new("F11", VK_F1 + 10, &[egui::Key::F11]),
    KeySpec::new("F12", VK_F1 + 11, &[egui::Key::F12]),
    KeySpec::new("F13", VK_F1 + 12, &[egui::Key::F13]),
    KeySpec::new("F14", VK_F1 + 13, &[egui::Key::F14]),
    KeySpec::new("F15", VK_F1 + 14, &[egui::Key::F15]),
    KeySpec::new("F16", VK_F1 + 15, &[egui::Key::F16]),
    KeySpec::new("F17", VK_F1 + 16, &[egui::Key::F17]),
    KeySpec::new("F18", VK_F1 + 17, &[egui::Key::F18]),
    KeySpec::new("F19", VK_F1 + 18, &[egui::Key::F19]),
    KeySpec::new("F20", VK_F1 + 19, &[egui::Key::F20]),
    KeySpec::new("F21", VK_F1 + 20, &[egui::Key::F21]),
    KeySpec::new("F22", VK_F1 + 21, &[egui::Key::F22]),
    KeySpec::new("F23", VK_F1 + 22, &[egui::Key::F23]),
    KeySpec::new("F24", VK_F1 + 23, &[egui::Key::F24]),
];

fn vk_from_name(name: &str) -> Option<u32> {
    KEYS.iter()
        .find(|spec| {
            spec.name.eq_ignore_ascii_case(name)
                || spec
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
        })
        .map(|spec| spec.vk)
}

fn vk_name(vk: u32) -> String {
    match KEYS.iter().find(|spec| spec.vk == vk) {
        Some(spec) => spec.name.to_string(),
        None => format!("VK{vk:02X}"),
    }
}

fn vk_from_egui(key: egui::Key) -> Option<u32> {
    KEYS.iter()
        .find(|spec| spec.egui.contains(&key))
        .map(|spec| spec.vk)
}

/// Modifier set for a captured key event. egui reports Ctrl/Alt/Shift, but the
/// Win key has no egui flag on Windows, so it is read from the physical state —
/// the same source the hook uses when it matches a chord.
pub fn mods_from_egui(modifiers: &egui::Modifiers) -> Mods {
    mods_with_win(modifiers, win_key_held())
}

fn mods_with_win(modifiers: &egui::Modifiers, win: bool) -> Mods {
    Mods {
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
        win,
    }
}

#[cfg(windows)]
fn win_key_held() -> bool {
    // SAFETY: `GetAsyncKeyState` accepts any virtual-key code and only reports
    // key state, so it has no preconditions to violate.
    unsafe { win::key_down(VK_LWIN as i32) || win::key_down(VK_RWIN as i32) }
}

#[cfg(not(windows))]
fn win_key_held() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Duplicate-chord rules (FR-5.7.5) — the one owner of "who may use a chord"
// ---------------------------------------------------------------------------

/// The current holder of a chord: either the stop-all binding or a sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordOwner {
    StopAll,
    Sound(u64),
}

/// Who already holds `hotkey`, ignoring `except` (the owner the caller is about
/// to change, so nothing conflicts with itself). Callers pass the active
/// library's sounds in tab order as `(id, hotkey)`:
///
/// * the stop-all chord wins over every sound;
/// * among sounds the first in tab order wins.
///
/// `stop_all` must be `None` unless the stop-all hotkey is enabled.
pub fn chord_owner<I>(
    sounds: I,
    stop_all: Option<Hotkey>,
    hotkey: Hotkey,
    except: Option<ChordOwner>,
) -> Option<ChordOwner>
where
    I: IntoIterator<Item = (u64, Option<Hotkey>)>,
{
    if stop_all == Some(hotkey) && except != Some(ChordOwner::StopAll) {
        return Some(ChordOwner::StopAll);
    }
    sounds
        .into_iter()
        .find(|(id, bound)| *bound == Some(hotkey) && except != Some(ChordOwner::Sound(*id)))
        .map(|(id, _)| ChordOwner::Sound(id))
}

/// The sounds that must lose their hotkey because an earlier one (or the
/// stop-all chord) already holds it, in tab order. This is the load/sync
/// tie-break: it is applied on every rebind, so a conflicting `settings.json`
/// always resolves the same way.
pub fn duplicate_sounds<I>(sounds: I, stop_all: Option<Hotkey>) -> Vec<u64>
where
    I: IntoIterator<Item = (u64, Option<Hotkey>)>,
{
    let bound: Vec<(u64, Option<Hotkey>)> = sounds.into_iter().collect();
    let mut duplicates = Vec::new();
    for index in 0..bound.len() {
        let Some((id, Some(hotkey))) = bound.get(index).copied() else {
            continue;
        };
        let earlier = bound.iter().take(index).copied();
        if chord_owner(earlier, stop_all, hotkey, None).is_some() {
            duplicates.push(id);
        }
    }
    duplicates
}

/// Keys the hook treats as held. Fixed capacity, because the hook runs on every
/// keystroke and must not allocate: a physical keyboard cannot press more
/// distinct keys at once than this, and if it somehow does, the extra keys are
/// simply not tracked (chords keep working, that key's auto-repeat guard opens).
const HELD_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
struct HeldKeys {
    keys: [u32; HELD_CAPACITY],
    len: usize,
}

impl HeldKeys {
    fn new() -> Self {
        Self::default()
    }

    fn as_slice(&self) -> &[u32] {
        &self.keys[..self.len]
    }

    fn contains(&self, vk: u32) -> bool {
        self.as_slice().contains(&vk)
    }

    fn push(&mut self, vk: u32) {
        if self.len < HELD_CAPACITY {
            self.keys[self.len] = vk;
            self.len += 1;
        }
    }

    fn retain(&mut self, keep: impl Fn(u32) -> bool) {
        let mut len = 0;
        for index in 0..self.len {
            let key = self.keys[index];
            if keep(key) {
                self.keys[len] = key;
                len += 1;
            }
        }
        self.len = len;
    }
}

/// Drops tracked keys the OS reports as released. A missed key-up (the hook was
/// re-entered, the session was switched) would otherwise keep a key "held"
/// forever, turning every later press into an auto-repeat.
fn retain_held(down: &mut HeldKeys, held: impl Fn(u32) -> bool) {
    down.retain(held);
}

/// True when a window title belongs to this app. Windows appends suffixes such
/// as `[Administrator]` or `- Shortcut` to the title it shows in the taskbar, so
/// the stable title is the *prefix*; the exe-path check in `main_window` still
/// decides which process the window belongs to.
fn title_matches(title: &str) -> bool {
    title.starts_with(crate::model::WINDOW_TITLE)
}

// ---------------------------------------------------------------------------
// Service API
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Toggle,
    StopAll,
    Quit,
}

#[derive(Clone, Debug)]
pub enum ServiceEvent {
    Pressed(u64),
    StopAll,
    Tray(TrayCommand),
    TrayReady(bool),
    Fatal(String),
}

enum Command {
    SetBindings(Vec<(u64, Hotkey)>),
    SetStopAll(Option<Hotkey>),
    UpdateTip(String),
    Shutdown,
}

pub struct HotkeyService {
    tx: Sender<Command>,
    events: Receiver<ServiceEvent>,
    thread_id: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl HotkeyService {
    pub fn start(
        tray_rgba: Vec<u8>,
        tray_size: u32,
        tooltip: String,
        playback: Arc<crate::playback::Playback>,
        ctx: egui::Context,
    ) -> Self {
        let (tx, rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let thread_id = Arc::new(AtomicU32::new(0));
        let tid = thread_id.clone();

        let fail_tx = event_tx.clone();
        let spawned = thread::Builder::new()
            .name("sounddeck-hotkeys".into())
            .spawn(move || {
                #[cfg(windows)]
                win::run(
                    rx, event_tx, ctx, tid, tray_rgba, tray_size, tooltip, playback,
                );
                #[cfg(not(windows))]
                {
                    let _ = (
                        rx, event_tx, ctx, tid, tray_rgba, tray_size, tooltip, playback,
                    );
                }
            });
        if let Err(err) = &spawned {
            let _ = fail_tx.send(ServiceEvent::Fatal(format!(
                "Could not start the hotkey thread: {err}. Hotkeys and the tray are unavailable."
            )));
        }
        let thread = spawned.ok();

        Self {
            tx,
            events: event_rx,
            thread_id,
            thread,
        }
    }

    pub fn set_bindings(&self, bindings: Vec<(u64, Hotkey)>) {
        let _ = self.tx.send(Command::SetBindings(bindings));
        self.wake();
    }

    pub fn set_stop_all(&self, hotkey: Option<Hotkey>) {
        let _ = self.tx.send(Command::SetStopAll(hotkey));
        self.wake();
    }

    pub fn set_tooltip(&self, tip: String) {
        let _ = self.tx.send(Command::UpdateTip(tip));
        self.wake();
    }

    pub fn try_recv(&self) -> Option<ServiceEvent> {
        self.events.try_recv().ok()
    }

    fn wake(&self) {
        #[cfg(windows)]
        {
            let tid = self.thread_id.load(Ordering::SeqCst);
            if tid != 0 {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                        tid,
                        win::WAKE_MSG,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                }
            }
        }
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        self.wake();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for HotkeyService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub fn focus_main_window() {
    unsafe { win::focus_main_window() }
}

#[cfg(windows)]
pub fn main_window_minimized() -> bool {
    unsafe { win::main_window_minimized() }
}

/// Shows or hides the taskbar button without touching the window styles.
/// Used while the window is parked in the tray as a minimized window.
#[cfg(windows)]
pub fn set_taskbar_tab(visible: bool) {
    unsafe { win::set_taskbar_tab(visible) }
}

#[cfg(not(windows))]
pub fn set_taskbar_tab(_visible: bool) {}

#[cfg(not(windows))]
pub fn main_window_minimized() -> bool {
    false
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::cell::RefCell;
    use std::mem::size_of;
    use std::rc::Rc;
    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CallNextHookEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DestroyWindow, DispatchMessageW, EnumWindows, GetCursorPos, GetMessageW, GetWindowLongPtrW,
        GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, LoadIconW, PostMessageW,
        PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
        SetWindowLongPtrW, SetWindowsHookExW, ShowWindow, TrackPopupMenu, TranslateMessage,
        UnhookWindowsHookEx, CREATESTRUCTW, GWLP_USERDATA, HHOOK, HMENU, IDI_APPLICATION,
        KBDLLHOOKSTRUCT, MF_SEPARATOR, MF_STRING, MSG, SW_RESTORE, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        WH_KEYBOARD_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
        WM_CONTEXTMENU, WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK, WM_NCCREATE, WM_NULL,
        WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW,
    };

    pub const WAKE_MSG: u32 = WM_APP + 2;
    const TRAY_MSG: u32 = WM_APP + 1;
    const PLAY_MSG: u32 = WM_APP + 3;
    const STOP_MSG: u32 = WM_APP + 4;
    /// Long enough for `model::WINDOW_TITLE` plus any suffix the OS appends.
    const TITLE_BUFFER: usize = 256;
    const MENU_SHOW: usize = 1;
    const MENU_STOP: usize = 2;
    const MENU_QUIT: usize = 3;

    struct HookShared {
        bindings: Vec<(Hotkey, u64)>,
        stop_all: Option<Hotkey>,
        down: HeldKeys,
        hwnd: HWND,
    }

    thread_local! {
        static HOOK: RefCell<Option<Rc<RefCell<HookShared>>>> = const { RefCell::new(None) };
    }

    struct State {
        /// Cleared in `WM_DESTROY` before the box is freed, so the message loop
        /// can tell a dangling `state_ptr` from a live one.
        alive: Arc<AtomicBool>,
        rx: Receiver<Command>,
        tx: Sender<ServiceEvent>,
        /// Wakes the UI out of its idle/dead repaint interval.
        ctx: egui::Context,
        hicon: windows::Win32::UI::WindowsAndMessaging::HICON,
        owns_icon: bool,
        tray_added: bool,
        tooltip: String,
        taskbar_created: u32,
        playback: Arc<crate::playback::Playback>,
        hook: Rc<RefCell<HookShared>>,
        keyboard_hook: Option<HHOOK>,
    }

    impl State {
        /// Forward an event and wake the UI so a tray click or hotkey is never
        /// stuck behind a one-second idle repaint.
        fn emit(&self, event: ServiceEvent) {
            let _ = self.tx.send(event);
            self.ctx.request_repaint();
        }
    }

    unsafe impl Send for State {}

    /// Zeroes the published hook-thread id when the thread leaves, whatever the
    /// exit path (including an early return and an unwind): a late `wake()` must
    /// never post to a thread id that no longer exists.
    struct ThreadIdGuard(Arc<AtomicU32>);

    impl Drop for ThreadIdGuard {
        fn drop(&mut self) {
            self.0.store(0, Ordering::SeqCst);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run(
        rx: Receiver<Command>,
        tx: Sender<ServiceEvent>,
        ctx: egui::Context,
        thread_id: Arc<AtomicU32>,
        rgba: Vec<u8>,
        size: u32,
        tooltip: String,
        playback: Arc<crate::playback::Playback>,
    ) {
        let _thread_id_guard = ThreadIdGuard(thread_id.clone());
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None).map(Into::into).unwrap_or_default();
            let class_name = w!("SoundDeckHiddenWindow");

            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                lpszClassName: class_name,
                ..Default::default()
            };
            let atom = RegisterClassW(&wc);
            if atom == 0 {
                let err = GetLastError();
                let _ = tx.send(ServiceEvent::Fatal(format!(
                    "Could not register window class (error {})",
                    err.0
                )));
                ctx.request_repaint();
                return;
            }

            let mut hicon = crate::icon::hicon_from_rgba(&rgba, size).unwrap_or_default();
            let mut owns_icon = !hicon.is_invalid();
            if hicon.is_invalid() {
                hicon = LoadIconW(HINSTANCE::default(), IDI_APPLICATION).unwrap_or_default();
                owns_icon = false;
            }

            let hook = Rc::new(RefCell::new(HookShared {
                bindings: Vec::new(),
                stop_all: None,
                down: HeldKeys::new(),
                hwnd: HWND::default(),
            }));
            // Liveness lives outside the state box: a wake posted before shutdown
            // can still be in the token queue after `WM_DESTROY` freed the state.
            let alive = Arc::new(AtomicBool::new(true));
            let state = Box::new(State {
                alive: alive.clone(),
                rx,
                tx: tx.clone(),
                ctx,
                hicon,
                owns_icon,
                tray_added: false,
                tooltip,
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                playback,
                hook,
                keyboard_hook: None,
            });
            let state_ptr = Box::into_raw(state);
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!("SoundDeck"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND::default(),
                HMENU::default(),
                hinstance,
                Some(state_ptr as *const core::ffi::c_void),
            ) {
                Ok(hwnd) => hwnd,
                Err(err) => {
                    drop(Box::from_raw(state_ptr));
                    let _ = tx.send(ServiceEvent::Fatal(format!(
                        "Could not create background window: {err}"
                    )));
                    return;
                }
            };

            add_tray(hwnd);
            let state = &mut *state_ptr;
            state.hook.borrow_mut().hwnd = hwnd;
            HOOK.with(|slot| *slot.borrow_mut() = Some(state.hook.clone()));
            state.keyboard_hook = match SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_proc),
                HINSTANCE::default(),
                0,
            ) {
                Ok(hook) => Some(hook),
                Err(err) => {
                    let _ = tx.send(ServiceEvent::Fatal(format!(
                        "Could not install the keyboard hook: {err}"
                    )));
                    None
                }
            };
            if state.tray_added {
                let _ = tx.send(ServiceEvent::TrayReady(true));
            } else {
                let _ = tx.send(ServiceEvent::TrayReady(false));
            }

            thread_id.store(GetCurrentThreadId(), Ordering::SeqCst);
            if !process_commands(hwnd, state) {
                return;
            }

            let mut msg = MSG::default();
            loop {
                let result = GetMessageW(&mut msg, HWND::default(), 0, 0);
                if result.0 <= 0 {
                    break;
                }
                if msg.hwnd.is_invalid() {
                    // Thread message (our wake). `process_commands` may destroy the
                    // window, which frees `state`; a wake posted earlier can still
                    // be queued, so check the liveness flag before any deref.
                    if !alive.load(Ordering::Acquire) {
                        break;
                    }
                    if msg.message == WAKE_MSG && !process_commands(hwnd, &mut *state_ptr) {
                        break;
                    }
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Drains queued service commands. Returns `false` after the background
    /// window has been destroyed, at which point `state` is freed.
    unsafe fn process_commands(hwnd: HWND, state: &mut State) -> bool {
        while let Ok(cmd) = state.rx.try_recv() {
            match cmd {
                Command::SetBindings(bindings) => {
                    state.hook.borrow_mut().bindings = bindings
                        .iter()
                        .map(|(sound_id, hotkey)| (*hotkey, *sound_id))
                        .collect();
                }
                Command::SetStopAll(hotkey) => {
                    state.hook.borrow_mut().stop_all = hotkey;
                }
                Command::UpdateTip(tip) => {
                    state.tooltip = tip;
                    if state.tray_added {
                        update_tray_tip(hwnd, state);
                    }
                }
                Command::Shutdown => {
                    remove_tray(hwnd, state);
                    let _ = DestroyWindow(hwnd);
                    return false;
                }
            }
        }
        true
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let msg = wparam.0 as u32;
            if matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
                let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
                let vk = kb.vkCode;
                let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                if let Some(shared) = HOOK.with(|slot| slot.borrow().clone()) {
                    let mut hook = shared.borrow_mut();
                    // Physical state is the truth: forget keys whose key-up never
                    // arrived, or they stay "held" and block that hotkey forever.
                    let mut keys = std::mem::take(&mut hook.down);
                    retain_held(&mut keys, |key| key_down(key as i32));
                    if is_down {
                        if !keys.contains(vk) {
                            keys.push(vk);
                            // Modifiers come from the tracked keys, not a second
                            // live poll that could disagree with them.
                            let mods = Mods::from_keys(keys.as_slice());
                            if hook
                                .stop_all
                                .map(|hotkey| hotkey.vk == vk && hotkey.mods == mods)
                                .unwrap_or(false)
                            {
                                let _ = PostMessageW(hook.hwnd, STOP_MSG, WPARAM(0), LPARAM(0));
                            } else if let Some(id) = find_binding(&hook.bindings, mods, vk) {
                                let _ = PostMessageW(
                                    hook.hwnd,
                                    PLAY_MSG,
                                    WPARAM(id as usize),
                                    LPARAM(0),
                                );
                            }
                        }
                    } else {
                        keys.retain(|key| key != vk);
                    }
                    hook.down = keys;
                }
            }
        }
        // Always pass the key through: the focused app sees exactly what was typed.
        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    /// Physical state of one key. Shared by the hook's stuck-key sweep and the
    /// capture dialog's Win modifier, so both read the same truth.
    pub(super) unsafe fn key_down(vk: i32) -> bool {
        (GetAsyncKeyState(vk) as u16 & 0x8000) != 0
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = ptr.as_mut();
        match msg {
            PLAY_MSG => {
                if let Some(state) = state {
                    let sound_id = wparam.0 as u64;
                    state.playback.play(sound_id);
                    state.emit(ServiceEvent::Pressed(sound_id));
                }
                LRESULT(0)
            }
            STOP_MSG => {
                if let Some(state) = state {
                    state.playback.stop_all();
                    state.emit(ServiceEvent::StopAll);
                }
                LRESULT(0)
            }
            TRAY_MSG => {
                if let Some(state) = state {
                    let event = (lparam.0 as u32) & 0xFFFF;
                    match event {
                        WM_RBUTTONUP | WM_CONTEXTMENU => show_tray_menu(hwnd, state),
                        WM_LBUTTONDBLCLK => {
                            state.emit(ServiceEvent::Tray(TrayCommand::Toggle));
                        }
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                if let Some(state) = state {
                    let id = wparam.0 & 0xFFFF;
                    match id {
                        MENU_SHOW => {
                            state.emit(ServiceEvent::Tray(TrayCommand::Toggle));
                        }
                        MENU_STOP => {
                            state.playback.stop_all();
                            state.emit(ServiceEvent::Tray(TrayCommand::StopAll));
                        }
                        MENU_QUIT => {
                            close_main_window();
                            state.emit(ServiceEvent::Tray(TrayCommand::Quit));
                        }
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WAKE_MSG => {
                if let Some(state) = state {
                    process_commands(hwnd, state);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(state) = state {
                    remove_tray(hwnd, state);
                    state.emit(ServiceEvent::TrayReady(false));
                    if let Some(hook) = state.keyboard_hook.take() {
                        let _ = UnhookWindowsHookEx(hook);
                    }
                    HOOK.with(|slot| *slot.borrow_mut() = None);
                    if state.owns_icon && !state.hicon.is_invalid() {
                        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(state.hicon);
                    }
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    // Announce the free before it happens: the message loop still
                    // holds `state_ptr` and must not touch it afterwards.
                    state.alive.store(false, Ordering::Release);
                    drop(Box::from_raw(ptr));
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => {
                if let Some(state) = state {
                    if msg == state.taskbar_created {
                        state.tray_added = false;
                        add_tray(hwnd);
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
    }

    unsafe fn notify_data(hwnd: HWND, state: &State, flags: u32) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: windows::Win32::UI::Shell::NOTIFY_ICON_DATA_FLAGS(flags),
            uCallbackMessage: TRAY_MSG,
            hIcon: state.hicon,
            ..Default::default()
        };
        set_tip(&mut nid.szTip, &state.tooltip);
        nid
    }

    fn set_tip(dst: &mut [u16], text: &str) {
        let wide: Vec<u16> = text.encode_utf16().take(dst.len() - 1).collect();
        dst[..wide.len()].copy_from_slice(&wide);
        dst[wide.len()] = 0;
    }

    unsafe fn add_tray(hwnd: HWND) {
        let state = &*((GetWindowLongPtrW(hwnd, GWLP_USERDATA)) as *const State);
        let flags = NIF_MESSAGE.0 | NIF_ICON.0 | NIF_TIP.0;
        let nid = notify_data(hwnd, state, flags);
        let ok = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
        let state = &mut *((GetWindowLongPtrW(hwnd, GWLP_USERDATA)) as *mut State);
        state.tray_added = ok;
    }

    unsafe fn remove_tray(hwnd: HWND, state: &mut State) {
        if state.tray_added {
            let nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                ..Default::default()
            };
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            state.tray_added = false;
        }
    }

    unsafe fn update_tray_tip(hwnd: HWND, state: &State) {
        let nid = notify_data(hwnd, state, NIF_TIP.0);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }

    unsafe fn show_tray_menu(hwnd: HWND, state: &mut State) {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Show / Hide SoundDeck"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_STOP, w!("Stop all sounds"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit SoundDeck"));

        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_ok() {
            let _ = SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                hwnd,
                None,
            );
            if cmd.0 > 0 {
                match cmd.0 as usize {
                    MENU_SHOW => {
                        state.emit(ServiceEvent::Tray(TrayCommand::Toggle));
                    }
                    MENU_STOP => {
                        state.playback.stop_all();
                        state.emit(ServiceEvent::Tray(TrayCommand::StopAll));
                    }
                    MENU_QUIT => {
                        close_main_window();
                        state.emit(ServiceEvent::Tray(TrayCommand::Quit));
                    }
                    _ => {}
                }
            }
        }
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    }

    /// Find the main window that belongs to this executable.
    unsafe fn main_window() -> HWND {
        struct Search {
            exe: Option<String>,
            found: HWND,
        }

        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let search = &mut *(lparam.0 as *mut Search);
            let mut title = [0u16; TITLE_BUFFER];
            let len = GetWindowTextW(hwnd, &mut title);
            if len == 0 {
                return BOOL(1);
            }
            if !title_matches(&String::from_utf16_lossy(&title[..len as usize])) {
                return BOOL(1);
            }
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return BOOL(1);
            };
            let mut buffer = [0u16; 1024];
            let mut size = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut size,
            )
            .is_ok();
            let _ = CloseHandle(process);
            if !ok {
                return BOOL(1);
            }
            let path = String::from_utf16_lossy(&buffer[..size as usize]);
            if let Some(exe) = &search.exe {
                if path.eq_ignore_ascii_case(exe) {
                    search.found = hwnd;
                    return BOOL(0);
                }
            }
            BOOL(1)
        }

        let exe = std::env::current_exe()
            .ok()
            .map(|path| path.to_string_lossy().to_string());
        let mut search = Search {
            exe,
            found: HWND::default(),
        };
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
        search.found
    }

    /// Bring this executable's main window to the foreground from any state
    /// (including hidden). Matching on the exe path keeps two portable copies
    /// from focusing each other's windows.
    pub unsafe fn focus_main_window() {
        let hwnd = main_window();
        if !hwnd.is_invalid() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    /// Cached handle of this executable's main window. The window lives for the
    /// whole process, so the expensive enumeration runs at most once and the
    /// minimize poll then costs one `IsIconic` (NFR-8.1: no window enumeration
    /// on a timer).
    static MAIN_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

    /// True when this executable's main window is minimized to the taskbar.
    pub unsafe fn main_window_minimized() -> bool {
        let cached =
            HWND(MAIN_HWND.load(std::sync::atomic::Ordering::Relaxed) as *mut core::ffi::c_void);
        let hwnd = if !cached.is_invalid() && IsWindow(cached).as_bool() {
            cached
        } else {
            let found = main_window();
            MAIN_HWND.store(found.0 as isize, std::sync::atomic::Ordering::Relaxed);
            found
        };
        !hwnd.is_invalid() && IsIconic(hwnd).as_bool()
    }

    /// Adds or removes the taskbar button for the main window.
    ///
    /// A minimized window still owns a taskbar button, which is exactly what
    /// "minimize to tray" is not supposed to leave behind. `ITaskbarList` is the
    /// documented way to drop it without touching the window styles; failures
    /// are ignored because the worst case is a stray button, not bad audio.
    pub unsafe fn set_taskbar_tab(visible: bool) {
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};

        let hwnd = main_window();
        if hwnd.is_invalid() {
            return;
        }
        // The UI thread may already be initialised under another model; that is
        // fine, the failure is expected and harmless here.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(list) =
            CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
        else {
            return;
        };
        let _ = list.HrInit();
        if visible {
            let _ = list.AddTab(hwnd);
        } else {
            let _ = list.DeleteTab(hwnd);
        }
    }

    unsafe fn close_main_window() {
        let hwnd = main_window();
        if !hwnd.is_invalid() {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hotkey(text: &str) -> Hotkey {
        Hotkey::parse(text).expect("test hotkey parses")
    }

    #[test]
    fn parses_common_hotkeys() {
        assert_eq!(hotkey("Alt+1").vk, b'1' as u32);
        assert_eq!(hotkey("Ctrl+Shift+S").vk, b'S' as u32);
        assert_eq!(hotkey("F8").vk, VK_F1 + 7);
        assert_eq!(hotkey("space").vk, VK_SPACE);
        assert_eq!(hotkey("ctrl+alt+pageup").vk, VK_PRIOR);
        assert_eq!(
            hotkey("Win+Ctrl+2").mods,
            Mods {
                ctrl: true,
                win: true,
                ..Mods::default()
            }
        );
        assert!(Hotkey::parse("nope").is_err());
        assert!(Hotkey::parse("Ctrl+Shift").is_err());
    }

    #[test]
    fn labels_roundtrip_in_the_stored_modifier_order() {
        for text in [
            "Alt+1",
            "Ctrl+Shift+S",
            "F12",
            "Ctrl+Alt+PageDown",
            "Win+A",
            "Ctrl+Win+F8",
            "Ctrl+Alt+Shift+Win+S",
        ] {
            let parsed = hotkey(text);
            assert_eq!(parsed.label(), text);
            assert_eq!(Hotkey::parse(&parsed.label()), Ok(parsed));
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(Hotkey::parse("").is_err());
        assert!(Hotkey::parse("+").is_err());
        assert!(Hotkey::parse("A+B").is_err());
        assert!(Hotkey::parse("F25").is_err());
    }

    #[test]
    fn key_table_round_trips_names_vks_aliases_and_egui_keys() {
        for spec in KEYS {
            assert_eq!(vk_from_name(spec.name), Some(spec.vk), "name {}", spec.name);
            assert_eq!(vk_name(spec.vk), spec.name, "vk {:#X}", spec.vk);
            for alias in spec.aliases {
                assert_eq!(vk_from_name(alias), Some(spec.vk), "alias {alias}");
            }
            for key in spec.egui {
                assert_eq!(vk_from_egui(*key), Some(spec.vk), "egui {key:?}");
            }
        }
        // Each key is claimed exactly once, so display and parsing stay total.
        let mut vks: Vec<u32> = KEYS.iter().map(|spec| spec.vk).collect();
        vks.sort_unstable();
        vks.dedup();
        assert_eq!(vks.len(), KEYS.len());
    }

    #[test]
    fn unknown_vk_codes_display_as_codes() {
        assert_eq!(vk_name(0xE2), "VKE2");
        assert!(vk_from_name("VKE2").is_none());
    }

    #[test]
    fn capture_maps_egui_keys_through_the_table() {
        assert_eq!(vk_from_egui(egui::Key::S), Some(b'S' as u32));
        assert_eq!(vk_from_egui(egui::Key::Num7), Some(b'7' as u32));
        assert_eq!(vk_from_egui(egui::Key::ArrowDown), Some(VK_DOWN));
        assert_eq!(vk_from_egui(egui::Key::Plus), Some(VK_OEM_PLUS));
        assert_eq!(vk_from_egui(egui::Key::Copy), None);
    }

    #[test]
    fn capture_keeps_the_win_modifier() {
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert!(mods_with_win(&ctrl, true).win);
        assert!(!mods_with_win(&ctrl, false).win);
        assert_eq!(
            mods_with_win(&ctrl, true),
            Mods {
                ctrl: true,
                win: true,
                ..Mods::default()
            }
        );
    }

    #[test]
    fn modifiers_come_from_the_held_keys() {
        assert!(Mods::from_keys(&[VK_LCONTROL, b'S' as u32]).ctrl);
        assert!(Mods::from_keys(&[VK_CONTROL]).ctrl);
        assert!(Mods::from_keys(&[VK_RMENU]).alt);
        assert!(Mods::from_keys(&[VK_LSHIFT]).shift);
        assert!(Mods::from_keys(&[VK_LWIN]).win);
        assert!(Mods::from_keys(&[VK_RWIN]).win);
        assert_eq!(Mods::from_keys(&[b'S' as u32]), Mods::default());
        assert!(Mods::default().is_empty());
        assert!(!Mods {
            ctrl: true,
            ..Mods::default()
        }
        .is_empty());
    }

    #[test]
    fn released_keys_are_dropped_from_the_tracked_set() {
        let mut down = HeldKeys::new();
        down.push(VK_LCONTROL);
        down.push(b'S' as u32);
        retain_held(&mut down, |vk| vk == VK_LCONTROL);
        assert_eq!(down.as_slice(), &[VK_LCONTROL]);
    }

    #[test]
    fn a_full_held_set_ignores_extra_keys_instead_of_growing() {
        let mut down = HeldKeys::new();
        for index in 0..HELD_CAPACITY as u32 + 4 {
            down.push(b'A' as u32 + index);
        }
        assert_eq!(down.as_slice().len(), HELD_CAPACITY);
        assert!(down.contains(b'A' as u32));
        retain_held(&mut down, |vk| vk % 2 == 0);
        assert!(!down.as_slice().is_empty());
        assert!(down.as_slice().iter().all(|vk| vk % 2 == 0));
    }

    #[test]
    fn window_title_match_accepts_the_os_appended_suffixes() {
        let title = crate::model::WINDOW_TITLE;
        assert!(title_matches(title));
        assert!(title_matches(&format!("{title} [Administrator]")));
        assert!(title_matches(&format!("{title} - Shortcut")));
        assert!(!title_matches("SoundDeck"));
        assert!(!title_matches("SoundDeck - Something Else"));
        assert!(!title_matches(""));
    }

    #[test]
    fn chord_owner_finds_the_sound_holding_a_chord() {
        let first = hotkey("Ctrl+1");
        let sounds = [(1, Some(first)), (2, None), (3, Some(hotkey("Ctrl+2")))];
        assert_eq!(
            chord_owner(sounds, None, first, None),
            Some(ChordOwner::Sound(1))
        );
        assert_eq!(
            chord_owner(sounds, None, first, Some(ChordOwner::Sound(1))),
            None
        );
        assert_eq!(chord_owner(sounds, None, hotkey("F9"), None), None);
    }

    #[test]
    fn chord_owner_prefers_stop_all_over_any_sound() {
        let shared = hotkey("Ctrl+1");
        let sounds = [(1, Some(shared))];
        assert_eq!(
            chord_owner(sounds, Some(shared), shared, None),
            Some(ChordOwner::StopAll)
        );
        assert_eq!(
            chord_owner(sounds, Some(shared), shared, Some(ChordOwner::StopAll)),
            Some(ChordOwner::Sound(1))
        );
        // A disabled stop-all chord owns nothing.
        assert_eq!(
            chord_owner(sounds, None, shared, Some(ChordOwner::StopAll)),
            Some(ChordOwner::Sound(1))
        );
    }

    #[test]
    fn duplicate_sounds_resolve_by_tab_order() {
        let first = hotkey("Ctrl+1");
        let second = hotkey("Ctrl+2");
        let sounds = [
            (10, Some(first)),
            (11, Some(first)),
            (12, None),
            (13, Some(second)),
            (14, Some(second)),
        ];
        assert_eq!(duplicate_sounds(sounds, None), vec![11, 14]);
        let swapped = [(11, Some(first)), (10, Some(first))];
        assert_eq!(duplicate_sounds(swapped, None), vec![10]);
    }

    #[test]
    fn duplicate_sounds_lose_to_the_stop_all_chord() {
        let shared = hotkey("Ctrl+1");
        let sounds = [(10, Some(shared)), (11, Some(hotkey("Ctrl+2")))];
        assert_eq!(duplicate_sounds(sounds, Some(shared)), vec![10]);
    }

    #[cfg(windows)]
    #[test]
    fn binding_matches_modifiers_and_key() {
        let shift_t = hotkey("Shift+T");
        let bindings = vec![(shift_t, 7)];
        let shifted = Mods {
            shift: true,
            ..Mods::default()
        };
        assert_eq!(find_binding(&bindings, shifted, b'T' as u32), Some(7));
        assert_eq!(find_binding(&bindings, Mods::default(), b'T' as u32), None);
        assert_eq!(find_binding(&bindings, shifted, b'Y' as u32), None);
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert_eq!(find_binding(&bindings, ctrl, b'T' as u32), None);
    }
}
