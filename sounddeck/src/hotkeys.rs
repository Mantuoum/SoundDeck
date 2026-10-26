//! Global hotkeys (RegisterHotKey) + system tray, running on one background
//! thread with a hidden Win32 message window.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{unbounded, Receiver, Sender};

// ---------------------------------------------------------------------------
// Hotkey model / parsing (platform independent, unit tested)
// ---------------------------------------------------------------------------

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

    #[cfg(windows)]
    fn win32_flags(self) -> u32 {
        let mut flags = 0;
        if self.alt {
            flags |= 0x0001;
        }
        if self.ctrl {
            flags |= 0x0002;
        }
        if self.shift {
            flags |= 0x0004;
        }
        if self.win {
            flags |= 0x0008;
        }
        flags
    }
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

    /// Convert an egui key press into a Windows virtual-key hotkey.
    pub fn from_egui(key: eframe::egui::Key, modifiers: &eframe::egui::Modifiers) -> Option<Self> {
        let vk = vk_from_egui(key)?;
        Some(Self {
            mods: Mods {
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
                win: false,
            },
            vk,
        })
    }
}

fn vk_from_name(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    if lower.len() == 1 {
        let ch = lower.chars().next()?;
        if ch.is_ascii_lowercase() {
            return Some(ch.to_ascii_uppercase() as u32);
        }
        if ch.is_ascii_digit() {
            return Some(ch as u32);
        }
        return match ch {
            '-' => Some(0xBD),
            '=' => Some(0xBB),
            ',' => Some(0xBC),
            '.' => Some(0xBE),
            '/' => Some(0xBF),
            ';' => Some(0xBA),
            '\'' => Some(0xDE),
            '[' => Some(0xDB),
            ']' => Some(0xDD),
            '\\' => Some(0xDC),
            '`' => Some(0xC0),
            _ => None,
        };
    }
    if let Some(num) = lower.strip_prefix('f') {
        if let Ok(n) = num.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1);
            }
        }
    }
    match lower.as_str() {
        "space" | "spacebar" => Some(0x20),
        "tab" => Some(0x09),
        "enter" | "return" => Some(0x0D),
        "esc" | "escape" => Some(0x1B),
        "backspace" | "bksp" => Some(0x08),
        "delete" | "del" => Some(0x2E),
        "insert" | "ins" => Some(0x2D),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "pgup" => Some(0x21),
        "pagedown" | "pgdn" => Some(0x22),
        "up" | "arrowup" => Some(0x26),
        "down" | "arrowdown" => Some(0x28),
        "left" | "arrowleft" => Some(0x25),
        "right" | "arrowright" => Some(0x27),
        _ => None,
    }
}

fn vk_name(vk: u32) -> String {
    match vk {
        0x20 => "Space".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x1B => "Esc".into(),
        0x08 => "Backspace".into(),
        0x2E => "Delete".into(),
        0x2D => "Insert".into(),
        0x24 => "Home".into(),
        0x23 => "End".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x26 => "Up".into(),
        0x28 => "Down".into(),
        0x25 => "Left".into(),
        0x27 => "Right".into(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        v @ 0x30..=0x39 => ((b'0' + (v - 0x30) as u8) as char).to_string(),
        v @ 0x41..=0x5A => ((b'A' + (v - 0x41) as u8) as char).to_string(),
        v @ 0x70..=0x87 => format!("F{}", v - 0x70 + 1),
        _ => format!("VK{vk:02X}"),
    }
}

fn vk_from_egui(key: eframe::egui::Key) -> Option<u32> {
    use eframe::egui::Key as K;
    Some(match key {
        K::A => 0x41,
        K::B => 0x42,
        K::C => 0x43,
        K::D => 0x44,
        K::E => 0x45,
        K::F => 0x46,
        K::G => 0x47,
        K::H => 0x48,
        K::I => 0x49,
        K::J => 0x4A,
        K::K => 0x4B,
        K::L => 0x4C,
        K::M => 0x4D,
        K::N => 0x4E,
        K::O => 0x4F,
        K::P => 0x50,
        K::Q => 0x51,
        K::R => 0x52,
        K::S => 0x53,
        K::T => 0x54,
        K::U => 0x55,
        K::V => 0x56,
        K::W => 0x57,
        K::X => 0x58,
        K::Y => 0x59,
        K::Z => 0x5A,
        K::Num0 => 0x30,
        K::Num1 => 0x31,
        K::Num2 => 0x32,
        K::Num3 => 0x33,
        K::Num4 => 0x34,
        K::Num5 => 0x35,
        K::Num6 => 0x36,
        K::Num7 => 0x37,
        K::Num8 => 0x38,
        K::Num9 => 0x39,
        K::F1 => 0x70,
        K::F2 => 0x71,
        K::F3 => 0x72,
        K::F4 => 0x73,
        K::F5 => 0x74,
        K::F6 => 0x75,
        K::F7 => 0x76,
        K::F8 => 0x77,
        K::F9 => 0x78,
        K::F10 => 0x79,
        K::F11 => 0x7A,
        K::F12 => 0x7B,
        K::F13 => 0x7C,
        K::F14 => 0x7D,
        K::F15 => 0x7E,
        K::F16 => 0x7F,
        K::F17 => 0x80,
        K::F18 => 0x81,
        K::F19 => 0x82,
        K::F20 => 0x83,
        K::F21 => 0x84,
        K::F22 => 0x85,
        K::F23 => 0x86,
        K::F24 => 0x87,
        K::Space => 0x20,
        K::Tab => 0x09,
        K::Enter => 0x0D,
        K::Escape => 0x1B,
        K::Backspace => 0x08,
        K::Delete => 0x2E,
        K::Insert => 0x2D,
        K::Home => 0x24,
        K::End => 0x23,
        K::PageUp => 0x21,
        K::PageDown => 0x22,
        K::ArrowUp => 0x26,
        K::ArrowDown => 0x28,
        K::ArrowLeft => 0x25,
        K::ArrowRight => 0x27,
        K::Semicolon => 0xBA,
        K::Plus | K::Equals => 0xBB,
        K::Comma => 0xBC,
        K::Minus => 0xBD,
        K::Period => 0xBE,
        K::Slash => 0xBF,
        K::Backtick => 0xC0,
        K::OpenBracket => 0xDB,
        K::Backslash => 0xDC,
        K::CloseBracket => 0xDD,
        K::Quote => 0xDE,
        _ => return None,
    })
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
    Registered {
        sound_id: u64,
        hotkey: Hotkey,
        error: Option<String>,
    },
    Tray(TrayCommand),
    TrayReady(bool),
    Fatal(String),
}

enum Command {
    SetBindings(Vec<(u64, Hotkey)>),
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
    ) -> Self {
        let (tx, rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let thread_id = Arc::new(AtomicU32::new(0));
        let tid = thread_id.clone();

        let thread = thread::Builder::new()
            .name("sounddeck-hotkeys".into())
            .spawn(move || {
                #[cfg(windows)]
                win::run(
                    rx,
                    event_tx,
                    tid,
                    tray_rgba,
                    tray_size,
                    tooltip,
                    playback,
                );
                #[cfg(not(windows))]
                {
                    let _ = (rx, event_tx, tid, tray_rgba, tray_size, tooltip, playback);
                }
            })
            .ok();

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

#[cfg(not(windows))]
pub fn main_window_minimized() -> bool {
    false
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::collections::HashMap;
    use std::mem::size_of;
    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM, BOOL,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    };
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, LoadIconW, PostMessageW, PostQuitMessage, RegisterClassW,
        RegisterWindowMessageW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, TrackPopupMenu,
        TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, HMENU, IDI_APPLICATION, MF_SEPARATOR,
        MF_STRING, MSG, SW_RESTORE, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE,
        WNDCLASSW, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY,
        WM_LBUTTONDBLCLK, WM_NCCREATE, WM_NULL, WM_RBUTTONUP, EnumWindows,
    };

    pub const WAKE_MSG: u32 = WM_APP + 2;
    const TRAY_MSG: u32 = WM_APP + 1;
    const MENU_SHOW: usize = 1;
    const MENU_STOP: usize = 2;
    const MENU_QUIT: usize = 3;

    struct State {
        rx: Receiver<Command>,
        tx: Sender<ServiceEvent>,
        id_map: HashMap<i32, u64>,
        next_id: i32,
        hicon: windows::Win32::UI::WindowsAndMessaging::HICON,
        owns_icon: bool,
        tray_added: bool,
        tooltip: String,
        taskbar_created: u32,
        registered: Vec<i32>,
        playback: Arc<crate::playback::Playback>,
    }

    unsafe impl Send for State {}

    pub fn run(
        rx: Receiver<Command>,
        tx: Sender<ServiceEvent>,
        thread_id: Arc<AtomicU32>,
        rgba: Vec<u8>,
        size: u32,
        tooltip: String,
        playback: Arc<crate::playback::Playback>,
    ) {
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
                return;
            }

            let mut hicon = crate::icon::hicon_from_rgba(&rgba, size).unwrap_or_default();
            let mut owns_icon = !hicon.is_invalid();
            if hicon.is_invalid() {
                hicon = LoadIconW(HINSTANCE::default(), IDI_APPLICATION).unwrap_or_default();
                owns_icon = false;
            }

            let state = Box::new(State {
                rx,
                tx: tx.clone(),
                id_map: HashMap::new(),
                next_id: 100,
                hicon,
                owns_icon,
                tray_added: false,
                tooltip,
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                registered: Vec::new(),
                playback,
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
            if state.tray_added {
                let _ = tx.send(ServiceEvent::TrayReady(true));
            } else {
                let _ = tx.send(ServiceEvent::TrayReady(false));
            }

            thread_id.store(GetCurrentThreadId(), Ordering::SeqCst);
            process_commands(hwnd, state);

            let mut msg = MSG::default();
            loop {
                let result = GetMessageW(&mut msg, HWND::default(), 0, 0);
                if result.0 <= 0 {
                    break;
                }
                if msg.hwnd.is_invalid() && msg.message == WAKE_MSG {
                    process_commands(hwnd, &mut *state_ptr);
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe fn process_commands(hwnd: HWND, state: &mut State) {
        while let Ok(cmd) = state.rx.try_recv() {
            match cmd {
                Command::SetBindings(bindings) => {
                    for id in state.registered.drain(..) {
                        let _ = UnregisterHotKey(hwnd, id);
                    }
                    state.id_map.clear();

                    for (sound_id, hk) in bindings {
                        let id = state.next_id;
                        state.next_id = state.next_id.wrapping_add(1).max(100);
                        let flags =
                            HOT_KEY_MODIFIERS(hk.mods.win32_flags() | 0x4000); // MOD_NOREPEAT
                        match RegisterHotKey(hwnd, id, flags, hk.vk) {
                            Ok(()) => {
                                state.id_map.insert(id, sound_id);
                                state.registered.push(id);
                                let _ = state.tx.send(ServiceEvent::Registered {
                                    sound_id,
                                    hotkey: hk,
                                    error: None,
                                });
                            }
                            Err(err) => {
                                let message = hotkey_error_text(&hk, &err);
                                let _ = state.tx.send(ServiceEvent::Registered {
                                    sound_id,
                                    hotkey: hk,
                                    error: Some(message),
                                });
                            }
                        }
                    }
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
                    return;
                }
            }
        }
    }

    fn hotkey_error_text(hk: &Hotkey, err: &windows::core::Error) -> String {
        let raw = err.code().0 as u32;
        let is_taken = (raw & 0xFFFF) == 1409 || raw == 1409;
        if is_taken {
            format!(
                "{} is already taken by another app or reserved by Windows",
                hk.label()
            )
        } else {
            format!("{} could not be registered: {err}", hk.label())
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = ptr.as_mut();
        match msg {
            WM_HOTKEY => {
                if let Some(state) = state {
                    if let Some(sound_id) = state.id_map.get(&(wparam.0 as i32)) {
                        state.playback.play(*sound_id);
                        let _ = state.tx.send(ServiceEvent::Pressed(*sound_id));
                    }
                }
                LRESULT(0)
            }
            TRAY_MSG => {
                if let Some(state) = state {
                    let event = (lparam.0 as u32) & 0xFFFF;
                    match event {
                        WM_RBUTTONUP | WM_CONTEXTMENU => show_tray_menu(hwnd, state),
                        WM_LBUTTONDBLCLK => {
                            let _ = state.tx.send(ServiceEvent::Tray(TrayCommand::Toggle));
                        }
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                if let Some(state) = state {
                    let id = (wparam.0 & 0xFFFF) as usize;
                    match id {
                        MENU_SHOW => {
                            let _ = state.tx.send(ServiceEvent::Tray(TrayCommand::Toggle));
                        }
                        MENU_STOP => {
                            state.playback.stop_all();
                            let _ = state.tx.send(ServiceEvent::Tray(TrayCommand::StopAll));
                        }
                        MENU_QUIT => {
                            close_main_window();
                            let _ = state.tx.send(ServiceEvent::Tray(TrayCommand::Quit));
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
                    let _ = state.tx.send(ServiceEvent::TrayReady(false));
                    for id in state.registered.drain(..) {
                        let _ = UnregisterHotKey(hwnd, id);
                    }
                    state.id_map.clear();
                    if state.owns_icon && !state.hicon.is_invalid() {
                        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(state.hicon);
                    }
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
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
                        let _ = state.tx.send(ServiceEvent::Tray(TrayCommand::Toggle));
                    }
                    MENU_STOP => {
                        state.playback.stop_all();
                        let _ = state.tx.send(ServiceEvent::Tray(TrayCommand::StopAll));
                    }
                    MENU_QUIT => {
                        close_main_window();
                        let _ = state.tx.send(ServiceEvent::Tray(TrayCommand::Quit));
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
            let mut title = [0u16; 64];
            let len = GetWindowTextW(hwnd, &mut title);
            if len == 0 {
                return BOOL(1);
            }
            if String::from_utf16_lossy(&title[..len as usize]) != "SoundDeck - Soundboard" {
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
        let _ = EnumWindows(
            Some(visit),
            LPARAM(&mut search as *mut Search as isize),
        );
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

    /// True when this executable's main window is minimized to the taskbar.
    pub unsafe fn main_window_minimized() -> bool {
        let hwnd = main_window();
        !hwnd.is_invalid() && IsIconic(hwnd).as_bool()
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

    #[test]
    fn parses_common_hotkeys() {
        assert_eq!(Hotkey::parse("Alt+1").unwrap().vk, 0x31);
        assert_eq!(Hotkey::parse("Ctrl+Shift+S").unwrap().vk, 0x53);
        assert_eq!(Hotkey::parse("F8").unwrap().vk, 0x77);
        assert_eq!(Hotkey::parse("space").unwrap().vk, 0x20);
        assert_eq!(Hotkey::parse("ctrl+alt+pageup").unwrap().vk, 0x21);
        assert!(Hotkey::parse("nope").is_err());
        assert!(Hotkey::parse("Ctrl+Shift").is_err());
    }

    #[test]
    fn labels_roundtrip() {
        for text in ["Alt+1", "Ctrl+Shift+S", "F12", "Ctrl+Alt+PageDown"] {
            let hk = Hotkey::parse(text).unwrap();
            assert_eq!(hk.label(), text);
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(Hotkey::parse("").is_err());
        assert!(Hotkey::parse("+").is_err());
        assert!(Hotkey::parse("A+B").is_err());
        assert!(Hotkey::parse("F25").is_err());
    }
}
