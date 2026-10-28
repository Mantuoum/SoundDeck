#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod hotkeys;
mod icon;
mod model;
mod playback;
mod stats;
mod storage;
mod ui;

use std::sync::Arc;

/// Start size: exactly half of a 1920x1080 display.
const START_WIDTH: f32 = 960.0;
const START_HEIGHT: f32 = 540.0;

/// FNV-1a hash of the executable path, used to scope the instance mutex to a
/// single portable copy.
#[cfg(windows)]
fn path_hash(path: &std::path::Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().to_lowercase().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// What the single-instance guard found at startup.
#[cfg(windows)]
enum Instance {
    /// This process may run. The handle is `None` only when the mutex could not
    /// be created at all, in which case exclusivity simply cannot be enforced.
    Primary(Option<windows::Win32::Foundation::HANDLE>),
    /// Another instance of this executable path is already running.
    AlreadyRunning,
}

/// Claims the named mutex that keeps this process the only instance of this
/// executable.
///
/// "Already running" is decided by `GetLastError()`, never by a failed
/// `CreateMutexW`: a creation failure means there is no other instance to hand
/// over to, so the process runs normally instead of exiting silently.
#[cfg(windows)]
fn single_instance() -> Instance {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, BOOL, ERROR_ALREADY_EXISTS, WIN32_ERROR,
    };
    use windows::Win32::System::Threading::CreateMutexW;

    let exe = std::env::current_exe().unwrap_or_default();
    let name = HSTRING::from(format!("Local\\SoundDeck-{:016x}", path_hash(&exe)));

    unsafe {
        SetLastError(WIN32_ERROR(0));
        let handle = match CreateMutexW(None, BOOL(0), PCWSTR(name.as_ptr())) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!(
                    "[startup] single-instance mutex unavailable ({err}); \
                     continuing as the only instance"
                );
                return Instance::Primary(None);
            }
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            hotkeys::focus_main_window();
            return Instance::AlreadyRunning;
        }
        Instance::Primary(Some(handle))
    }
}

fn load_icon() -> Option<Arc<eframe::egui::IconData>> {
    let (rgba, width, height) = icon::decode_png(include_bytes!("../assets/icon.png")).ok()?;
    Some(Arc::new(eframe::egui::IconData {
        rgba,
        width,
        height,
    }))
}

/// Drops the whole process below normal priority. Audio worker threads raise
/// themselves again individually, so playback stays smooth while the UI never
/// competes with whatever the user is actually working in.
fn lower_process_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
    };
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}

/// `panic = "abort"` means a panic is a process kill with no console and no
/// unwinding. Write the reason next to the executable so it can be reported.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_string());
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown location".to_string());
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let line = format!("[unix {stamp}] thread={name} {payload} at {location}\n");

        let path = model::app_dir().join("crash.log");
        let too_big = std::fs::metadata(&path)
            .map(|meta| meta.len() > 64 * 1024)
            .unwrap_or(false);
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true);
        if too_big {
            options.truncate(true);
        } else {
            options.append(true);
        }
        if let Ok(mut file) = options.open(&path) {
            use std::io::Write;
            let _ = file.write_all(line.as_bytes());
        }
    }));
}

fn main() -> Result<(), eframe::Error> {
    install_panic_hook();

    // 1 ms timer resolution keeps the mixer's 5 ms tick stable. Since Windows
    // 10 2004 this only affects this process, not the whole system.
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
    }

    // The board is a background utility: never compete with the foreground app
    // for CPU. The audio threads raise themselves above normal where it matters.
    lower_process_priority();

    if std::env::args().any(|arg| arg == "--stats") {
        stats::start_reporter();
    }

    if std::env::args().nth(1).as_deref() == Some("--diag") {
        let result = run_diag();
        unsafe {
            windows::Win32::Media::timeEndPeriod(1);
        }
        return match result {
            Ok(()) => Ok(()),
            Err(err) => {
                eprintln!("diag failed: {err}");
                std::process::exit(1);
            }
        };
    }

    // One instance per executable path. A second launch focuses the window
    // that is already open (including one hidden in the tray) and exits.
    let _instance = match single_instance() {
        Instance::Primary(handle) => handle,
        Instance::AlreadyRunning => {
            unsafe {
                windows::Win32::Media::timeEndPeriod(1);
            }
            return Ok(());
        }
    };

    let loaded = storage::load(&model::app_dir());
    // The window always starts at half of a 1920x1080 screen; resizing during
    // the session is not persisted.
    let window_size = eframe::egui::Vec2::new(START_WIDTH, START_HEIGHT);

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(model::WINDOW_TITLE)
        .with_inner_size(window_size)
        .with_min_inner_size([860.0, 500.0])
        .with_decorations(false)
        .with_resizable(true)
        .with_transparent(false);
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        vsync: true,
        centered: true,
        ..Default::default()
    };

    let open_settings = std::env::args().any(|arg| arg == "--settings");
    let result = eframe::run_native(
        model::WINDOW_TITLE,
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, loaded, open_settings)))),
    );

    unsafe {
        windows::Win32::Media::timeEndPeriod(1);
    }
    result
}

/// Headless(ish) smoke test for the WASAPI stack:
/// `sounddeck.exe --diag [file.wav]`
fn run_diag() -> Result<(), String> {
    use crate::audio::decode::decode_file;
    use crate::audio::engine::{DeviceRoute, Engine};
    use std::sync::Arc;
    use std::time::Duration;

    let list = audio::wasapi::list_devices()?;
    println!("Render devices ({}):", list.devices.len());
    for device in &list.devices {
        println!("  - {} :: {}", device.name, device.id);
    }

    let monitor = list
        .default_id
        .as_ref()
        .and_then(|id| list.devices.iter().find(|d| &d.id == id))
        .or_else(|| list.devices.first())
        .cloned()
        .ok_or_else(|| "no output devices".to_string())?;
    let cable = list
        .devices
        .iter()
        .find(|d| {
            let name = d.name.to_ascii_lowercase();
            name.contains("cable") && d.id != monitor.id
        })
        .cloned();

    println!(
        "Monitor: {}\nCable:   {}",
        monitor.name,
        cable.as_ref().map(|d| d.name.as_str()).unwrap_or("<none>")
    );

    let engine = Arc::new(Engine::new(1.0, 0.7, 0.0));
    engine.set_routes(
        Some(DeviceRoute {
            id: monitor.id.clone(),
            name: monitor.name.clone(),
        }),
        cable.clone().map(|d| DeviceRoute {
            id: d.id,
            name: d.name,
        }),
    );

    std::thread::sleep(Duration::from_millis(600));
    for event in engine.poll_events() {
        println!("event: {event:?}");
    }

    let file = std::env::args()
        .nth(2)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| model::app_dir().join("test_sounds").join("airhorn.wav"));
    if file.exists() {
        println!("Decoding {}", file.display());
        let audio = decode_file(&file).map_err(|e| format!("{e:#}"))?;
        println!("Decoded {:.2}s", audio.duration_secs);
        engine.play(1, audio.samples.clone(), 0.7);
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(250));
            let active = engine.active();
            println!(
                "active voices: {} progress {:.2}",
                active.len(),
                active.first().and_then(|v| v.progress).unwrap_or(0.0)
            );
            if active.is_empty() {
                break;
            }
        }
        for event in engine.poll_events() {
            println!("event: {event:?}");
        }
        let (m, c) = engine.buffered_frames();
        println!("ring buffered frames: monitor {m}, cable {c}");
    } else {
        println!("No test file at {}", file.display());
    }
    println!("diag OK");
    Ok(())
}
