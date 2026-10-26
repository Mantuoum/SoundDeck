# SoundDeck

A lightweight Windows soundboard inspired by Soundux. Native Rust + egui UI,
real WASAPI output, no Electron, no webview. Portable: settings and libraries
live in `config/` next to the executable.

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
- **Looping track** (opt-in): enable it in Settings → Playback to show a loop
  player under the meters. One file, streamed from disk, repeating forever, with
  its own volume independent of Monitor/Cable. It plays underneath sound effects
  and keeps playing through `Stop all`, library switches, and tray minimize. No
  hotkey, no seeking, no progress bar.
- Global hotkeys (`Alt+1`, `Ctrl+Shift+S`, `F8`, ...).
- **Dual output**: one monitor device and one cable device, with independent
  volumes and a link button. Volume sliders are logarithmic (`-60` to `0 dBFS`),
  so the track matches perceived loudness; stored gains stay linear.
- **Master gain** in dB (`0` to `+12 dB`, default `0 dB`) applied to both
  outputs. Right-click the slider to reset it to `0.0 dB`.
- **Peak meter** in the sidebar with a `-60` to `0 dBFS` scale, one full-width
  bar per output, zone colours, and a peak-hold marker.
- Only sounds with a hotkey can be kept in RAM.
  - Global `Load hotkey sounds into RAM` switch.
  - Per-sound `RAM` / `DISK` chip overrides that sound until the next global toggle.
  - Toolbar pill mirrors the state: `RAM`, `DISK`, or `MIXED`; click it to toggle.
  - Disk mode decodes on trigger, plays, and releases the samples.
- Themes are editable Markdown files in `config/themes/`. Graphite is the default
  and the fallback for any missing token; every colour (including the accent) can
  be overridden per theme. Use **Reload themes** in Settings after editing.
  The shipped palettes use a `#0097FF` accent.
- Grid and list layouts, search (scoped to the active library), drag to reorder,
  rename, remove with undo.
- Missing file detection with click-to-locate.
- System tray. **Minimize hides to the tray; Close quits the app.**
- The sidebar compacts automatically below 620 px window height: the dB scale,
  developer credit, and inline RAM row are hidden (the RAM switch stays in
  Settings → Playback). When the loop player is visible on a short window, the
  Monitor/Cable captions also collapse so the meters stay in view.
- One instance per executable path. Launching the same exe again focuses the
  window that is already open (including one hidden in the tray).
- Settings screen (`Ctrl+,` or the Settings button) for loop player, RAM preload,
  theme, `Animate UI`, developer info, and opening the config folder.

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

## Looping track

- Enable **Looping track** in Settings → Playback. The card appears under the peak
  meter; until a file is chosen, Play is disabled.
- Controls are Play/Stop plus the picker (folder icon) to choose or locate the file.
  Start and stop are instant.
- It has its own volume bar, independent of Monitor/Cable. Master gain still applies
  to everything.
- The path and volume are remembered in `settings.json`, but playback always starts
  stopped.
- A missing or corrupt file keeps its path and shows the error; the picker becomes
  the Locate action. Play turns into Retry after a decode failure.
- It is not a library sound: no search entry, no hotkey, no RAM preload. It keeps
  playing through `Stop all`, tab switches, and tray minimize. Closing the app
  stops it.

## Config files

```
SoundDeck.exe
config/
  settings.json     window, devices, volumes, theme, layout, active tab, loop file
  libraries.json    tabs and the sounds registered in each tab
  themes/           one Markdown palette per theme
```

Sounds keep their original paths; audio files are never copied. The looping
track is stored as `loop_enabled`, `loop_path` and `loop_volume` in
`settings.json`, following the same reference-only rule. A legacy `sounddeck.json` is imported once on first
launch and renamed to `sounddeck.v1.json.bak`. Each JSON file is written
atomically and keeps one rolling `.bak`; interrupted writes are recovered on the
next start.

## Audio notes

- Shared-mode WASAPI at the device's mix format; internal format is 48 kHz stereo.
- Two render streams are kept in sync from one mixer.
- Triggers are exclusive: the previous voice gets a 5 ms fade-out while the new
  one starts. The looping track is deliberately exempt from this rule.
- Decoded audio is cached only for hotkey sounds that are marked RAM.
- Files without a hotkey always play from disk on demand.
- The looping track is decoded by a dedicated thread and streamed through a small
  buffer, so it is never counted in the RAM preload and has no 20-minute cap. It has
  its own level before the master gain.
- Sound effects are still capped at 20 minutes when loaded into memory.

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
