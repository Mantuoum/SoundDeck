# SoundDeck

A lightweight Windows soundboard inspired by Soundux. Native Rust + egui UI,
real WASAPI output, no Electron, no webview. Portable: `sounddeck.json` lives
next to the executable.

## Run

- Release build: `target\release\sounddeck.exe`
- Review copy: `review\SoundDeck.exe`
- Build yourself: `cargo build --release`

## Features

- Drag and drop audio files (WAV, MP3, OGG, FLAC, M4A/AAC) or add them with the file dialog.
- **Libraries/tabs** in the title bar, after the app name. Add with `+`, rename
  or delete from a tab's context menu. Sounds can be moved between libraries from
  a sound's context menu, and the active library is remembered.
- **Only the active library is live**: its hotkeys are registered and only its
  hotkey sounds stay in RAM. Switching tabs stops playback and swaps the whole
  hotkey/RAM set. Hotkeys in background tabs never fire.
- **Single active sound**: triggering a sound fades out whatever is playing
  (including the same sound) and starts the new one.
- Global hotkeys (`Alt+1`, `Ctrl+Shift+S`, `F8`, ...).
- **Dual output**: one monitor device and one cable device, with independent
  volumes and a link button. Volume sliders are logarithmic (`-60` to `0 dBFS`),
  so the track matches perceived loudness; stored gains stay linear.
- **Master gain** in dB (`-60` to `+12 dB`, default `0 dB`) applied to both
  outputs, with a reset button (or double-click the slider).
- **Peak meter** in the sidebar with a `-60` to `0 dBFS` scale, one bar per
  output, zone colours, and a peak-hold marker.
- Only sounds with a hotkey can be kept in RAM.
  - Global `Load hotkey sounds into RAM` switch.
  - Per-sound `RAM` / `DISK` chip overrides that sound until the next global toggle.
  - Toolbar pill mirrors the state: `RAM`, `DISK`, or `MIXED`; click it to toggle.
  - Disk mode decodes on trigger, plays, and releases the samples.
- Five themes (Catppuccin Macchiato default, Mocha, Frappe, Latte, Graphite) with
  a `#0097FF` accent. Themes are documented in `C:\My files\My Resources\themes.md`.
- Grid and list layouts, search (scoped to the active library), drag to reorder,
  rename, remove with undo.
- Missing file detection with click-to-locate.
- System tray. **Minimize hides to the tray; Close quits the app.**
- One instance per executable path. Launching the same exe again focuses the
  window that is already open (including one hidden in the tray).
- Settings screen (`Ctrl+,` or the Settings button) for theme, window behaviour,
  developer info, and opening the config folder.

## Keyboard

| Key | Action |
| --- | --- |
| `Ctrl+O` | Add sounds |
| `Ctrl+F` | Focus search |
| `Ctrl+L` | Toggle grid/list |
| `Ctrl+,` | Settings |
| `Ctrl+Z` | Undo removal |
| `Ctrl+Q` | Quit |
| `Esc` | Close overlay / cancel drag / clear search / stop playback |
| `Delete` | Remove selected |
| `Space` | Play selected |
| `F2` | Rename selected |

## Audio notes

- Shared-mode WASAPI at the device's mix format; internal format is 48 kHz stereo.
- Two render streams are kept in sync from one mixer.
- Triggers are exclusive: the previous voice gets a 5 ms fade-out while the new
  one starts.
- Decoded audio is cached only for hotkey sounds that are marked RAM.
- Files without a hotkey always play from disk on demand.

## Hotkey limitations

`RegisterHotKey` is used, so it does not need administrator rights. Windows does
not expose mouse buttons through this API, and Win-key combinations are not
captured in the UI.

## Developer placeholders

Edit the constants near the top of `src/app.rs`:

```rust
const DEVELOPER_NAME: &str = "Your Name";
const DEVELOPER_ROLE: &str = "Independent developer";
const DEVELOPER_LINK: &str = "your-link.example";
const DEVELOPER_BLURB: &str = "Add your bio, links, and contact details here.";
```

## Diagnostics

```powershell
.\SoundDeck.exe --diag path\to\sound.wav
```

Enumerates output devices, opens the monitor/cable streams, decodes the file,
plays it, and prints progress plus WASAPI events.

## Third-party

- Catppuccin palettes (MIT) with a custom `#0097FF` accent.
- Satoshi font by Indian Type Foundry / Fontshare (see `assets/Satoshi-FFL.txt`).
- Icons generated for this project.
