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
- **Only the active library is live**: its hotkeys are bound and only its
  hotkey sounds stay in RAM. Switching tabs stops playback and swaps the whole
  hotkey/RAM set. Hotkeys in background tabs never fire.
- **Single active sound**: triggering a sound fades out whatever is playing
  (including the same sound) and starts the new one.
- **Looping track** (opt-in): enable it in Settings → Playback to show a loop
  player under the meters. One file, streamed from disk, repeating forever, with
  its own volume independent of Monitor/Cable. The loop can be routed to Monitor,
  Cable, both, or neither. It plays underneath sound effects and keeps playing through
  `Stop all`, library switches, and tray minimize. No hotkey, no seeking, no progress
  bar.
- Global hotkeys (`Alt+1`, `Ctrl+Shift+S`, `F8`, ...). The key **passes through** to
  the focused app, so `Shift+T` also types `T` while playing the assigned sound.
- **Dual output**: one monitor device and one cable device, with independent
  volumes and a link button. Volume sliders are logarithmic (`-60` to `0 dBFS`),
  so the track matches perceived loudness; stored gains stay linear.
- **Master gain** in dB (`0` to `+12 dB`, default `0 dB`) applied to both
  outputs. Right-click the slider to reset it to `0.0 dB`. It can be hidden from the
  sidebar in Settings → Window.
- **Peak meter** in the sidebar with a `-60` to `0 dBFS` scale, one full-width
  bar per output, zone colours, and a peak-hold marker.
- Only sounds with a hotkey can be kept in RAM.
  - Global `Load into RAM` switch in Settings → Playback.
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
- System tray. **Double-click the tray icon to show or hide SoundDeck**; right-click
  for Show/Hide, Stop all, Quit. Minimizing follows Settings → Window (taskbar by
  default; enable **Minimize to tray** to hide instead). **Close quits the app.**
- The sidebar compacts automatically below 620 px window height: the dB scale and
  developer credit are hidden. When the loop player is visible on a short window, the
  Monitor/Cable captions also collapse so the meters stay in view.
- One instance per executable path. Launching the same exe again focuses the
  window that is already open (including one hidden in the tray).
- Settings screen (`Ctrl+,` or the Settings button): full-window **Playback /
  Appearance / Window / About** tabs. Changes apply immediately and save automatically.

## Settings

The Settings button (or `Ctrl+,`) replaces the soundboard with a full-window view.
The rail on the left switches tabs; the status bar stays available.

- **Playback** — enable the looping track, choose its file, route it to Monitor and/or
  Cable; load hotkey sounds into RAM.
- **Appearance** — theme, reload themes, Animate UI.
- **Window** — minimize-to-tray and whether the sidebar shows the master-gain slider.
- **About** — version, config folder, developer details.

Monitor/Cable devices, volumes, the link switch and master gain stay in the board
sidebar where you need them while performing. Each device dropdown ends with
**Refresh devices**. Return with **Back**, `Esc`, or `Ctrl+,`; `Esc` closes an open
dropdown first. There is no Save button; changes apply immediately.

## Keyboard

| Key | Action |
| --- | --- |
| `Ctrl+O` | Add sounds |
| `Ctrl+F` | Focus search |
| `Ctrl+L` | Toggle grid/list |
| `Ctrl+,` | Settings |
| `Ctrl+Z` | Undo removal |
| `Ctrl+Q` | Quit |
| `Esc` | Close popup / return from Settings / clear search / stop playback |
| `Delete` | Remove selected |
| `Space` | Play selected |
| `F2` | Rename selected |

## Looping track

- Enable **Looping track** in Settings → Playback. The card appears under the peak
  meter; until a file is chosen, Play is disabled.
- Controls are Play/Stop and the volume bar on the loop card; **Choose file** lives in
  Settings → Playback. Start and stop are instant.
- It has its own volume bar, independent of Monitor/Cable. Master gain still applies
  to everything.
- **Loop outputs** in Settings → Playback: enable Monitor and Cable separately. Both,
  either, or neither can carry the loop; sound effects still go to both.
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
track is stored as `loop_enabled`, `loop_path`, `loop_volume`, `loop_on_monitor` and
`loop_on_cable` in `settings.json`, following the same reference-only rule. A legacy `sounddeck.json` is imported once on first
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
  its own level before the master gain and its own Monitor/Cable routing.
- Sound effects are still capped at 20 minutes when loaded into memory.

## Hotkey limitations

Global hotkeys use a low-level keyboard hook, so they do not need administrator
rights. The hook **always passes the key through** to the focused app: `Shift+T`
types `T` and plays the sound. Auto-repeat fires once per press.

Windows does not expose mouse buttons through this path, and Win-key combinations
are not captured in the UI. The hook cannot see keystrokes aimed at elevated
windows, and some security/anti-cheat tools may block keyboard hooks.

## Credits

Made by [Mantum](https://youtube.com/@mantu_um).

## License

MIT. See [`LICENSE`](../LICENSE) at the repository root.

## Diagnostics

```powershell
.\SoundDeck.exe --diag path\to\sound.wav
```

Enumerates output devices, opens the monitor/cable streams, decodes the file,
plays it, and prints progress plus WASAPI events.

## Developers

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the module map, audio pipeline, storage
rules, concurrency invariants and “where to change what” notes.

## Third-party

- Catppuccin palettes (MIT) with a custom `#0097FF` accent.
- Satoshi font by Indian Type Foundry / Fontshare (see `assets/Satoshi-FFL.txt`).
- Icons generated for this project.
