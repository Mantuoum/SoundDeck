# SoundDeck — Developer Architecture

This is the developer guide for SoundDeck: what each part of the code does, how the
pieces talk to each other, and where to change things. It assumes you know Rust basics
but not this application.

- **User manual:** `README.md`
- **Roadmap / decisions:** `../plan.md`
- **Session handoff:** `../context.md`

SoundDeck is a portable Windows soundboard: Rust, `eframe`/`egui` for the UI, WASAPI for
output, Symphonia for decoding. There is no installer, database, webview, or background
service. Everything lives beside the executable.

---

## 1. System at a glance

| Layer | Files | Responsibility |
| --- | --- | --- |
| Entry point | `src/main.rs` | Single instance, window, diagnostics, eframe startup |
| Application/UI | `src/app.rs` | State, actions, panels, board, Settings, modals |
| Domain helpers | `src/model.rs` | App paths, supported files, formatting, layout enum |
| Sound registry | `src/playback.rs` | RAM/disk sounds, decode jobs, per-sound volume |
| Audio device I/O | `src/audio/wasapi.rs` | Device enumeration and render threads |
| Mixer | `src/audio/engine.rs` | Voices, loop, rings, gains, metering |
| Decoding | `src/audio/decode.rs` | File → 48 kHz stereo f32, one shot |
| Streaming loop | `src/audio/stream.rs` | Loop decoder thread and chunk channel |
| Resampling | `src/audio/resample.rs` | Stateful 48 kHz stereo conversion |
| Storage | `src/storage/*` | JSON schemas, atomic writes, backups, migration |
| Hotkeys/tray | `src/hotkeys.rs` | `RegisterHotKey`, tray icon, message loop |
| Theme | `src/ui/theme*.rs` | Markdown palettes, fonts, radii, icons, style |
| Motion | `src/ui/anim.rs` | Hover/press/fade tweens, OS animation flag |
| Volume | `src/ui/volume.rs` | Log curve, percentage, slim volume bar |
| Icon asset | `src/icon.rs` | PNG decode/resize for the window/tray icon |

### Threads

| Thread | Started by | Lifetime | Job |
| --- | --- | --- | --- |
| UI/event loop | `eframe` | App lifetime | Draw frames, handle input, own the state |
| Mixer | `Engine::new` | App lifetime | Render 5 ms chunks, feed per-output rings |
| Playback loader | `Playback::new` | App lifetime | Decode sounds on demand / for RAM |
| Loop decoder | `LoopStream::spawn` | While the loop plays | Decode the loop file into a chunk channel |
| WASAPI render | `spawn_render`, once per output | While the device is open | Pull the ring, convert to the device format, write WASAPI |
| Hotkey/tray window | `HotkeyService::start` | App lifetime | Message-only window: hotkeys, tray, focus |
| Device enumeration | `wasapi::list_devices` | One call | COM enumeration on a short-lived thread |

### Data flow

```text
Mouse/keyboard ──► App::update ──► Action / direct call ──► Playback / Engine / cfg
Global hotkey  ──► hotkey thread ──► Playback::play ─────────┘
Loop Play      ──► Engine::start_loop ──► LoopStream thread ──► channel ──┐
                                                                          ▼
Playback::play ──► cached samples? ──► Engine::play ──► Mixer (voices) ──► Mixer::render
                                                              │
                                                    master gain, then rings
                                                              │
                                              WASAPI thread (per output) ──► speakers
```

The UI never blocks on decoding or audio. Calls that need work (decode, device start,
loop open) hand it to one of the threads above.

---

## 2. Startup and shutdown

### Startup (`main.rs`)

1. `timeBeginPeriod(1)` improves the mixer's 5 ms tick.
2. `--diag` runs the audio smoke test and exits; otherwise continue.
3. `single_instance()` creates a named mutex derived from the executable path. If it
   already exists, the second process focuses the first window and exits.
4. `storage::load(app_dir())` reads `config/settings.json` and
   `config/libraries.json`, applying recovery/normalization.
5. The viewport is built: 960×540 inner size, 860×500 minimum, no OS decorations,
   resizable. Window size is deliberately **not** restored.
6. `App::new` runs:
   - applies `animate_ui` and the Windows animation flag;
   - installs themes from `config/themes/` (writes embedded files if missing);
   - builds runtime `Sound`/`Library` vectors from storage;
   - creates `Engine`, then `Playback`;
   - enumerates render devices, resolves Monitor/Cable, applies routes;
   - starts the hotkey/tray service;
   - records JSON fingerprints so the first save does not rewrite untouched files.
7. `eframe::run_native` enters the UI loop.

### Shutdown

- **Close/Alt+F4/tray Quit:** `quit = true`, save now, viewport closes.
- `App::on_exit` stops the loop decoder.
- `Drop for App` stops the loop again and persists settings/libraries.
- `Drop for Engine` closes the loop, shuts down WASAPI threads, stops the mixer thread.
- `HotkeyService::shutdown` unregisters hotkeys and removes the tray icon.
- Saving is debounced during normal use; quitting forces one last write.

---

## 3. Storage

### Folder layout

```text
SoundDeck.exe
config/
  settings.json
  libraries.json
  libraries.json.bak / settings.json.bak   (one rolling backup each)
  *.corrupt                                (only after a failed parse)
  themes/
    graphite.md, macchiato.md, mocha.md, frappe.md, latte.md
```

Paths inside the app folder are stored relative (`model::portable_path`), so the whole
folder can move together. Audio files outside are absolute references and are never
copied. `sounddeck.json` from older builds is imported once and renamed
`sounddeck.v1.json.bak`.

### `settings.json`

| Field | Meaning |
| --- | --- |
| `version` | Schema version (currently 2) |
| `active_library` | Selected tab at launch |
| `theme` | Theme id (file stem) |
| `layout` | `grid` or `list` |
| `monitor_device` / `monitor_name` | Preferred monitor endpoint + fallback name |
| `cable_device` / `cable_name` | Preferred cable endpoint + fallback name |
| `monitor_volume` / `cable_volume` | Linear `0..1` output levels |
| `master_gain_db` | `0..12 dB` master gain |
| `link_volumes` | One slider moves both outputs |
| `preload_hotkey_sounds` | Global RAM default for hotkey sounds |
| `loop_enabled` | Show the loop player |
| `loop_path` | Loop file reference |
| `loop_volume` | Linear `0..1` loop level, independent of outputs |
| `window_size` | Parsed for old files, ignored |
| `animate_ui` | UI motion preference |

`Settings::normalize()` clamps volumes, gain, trims/empties the loop path, and fills
missing fields through `#[serde(default)]`.

### `libraries.json`

```json
{
  "version": 2,
  "libraries": [
    { "id": 1, "name": "Main", "sounds": [
      { "id": 2, "name": "horn", "path": "audio/horn.wav",
        "hotkey": "Alt+2", "volume": 1.0, "ram": true }
    ] }
  ]
}
```

- Tabs are the array order. Sounds are nested; `library_id` is not stored.
- `ram: null` means “inherit the global preload setting”.
- IDs are persisted sound IDs. `next_id` is derived from `max(id)+1`.

### Writes, backups, recovery (`storage/io.rs`)

- Write `<file>.tmp`, rotate the previous `<file>` to `<file>.bak`, rename the tmp.
- UTF-8 **without BOM** on write; a leading BOM is tolerated on read.
- Startup recovery order: main → `.tmp` → `.bak`; a bad file becomes `.corrupt`.
- Remaining `.tmp` files are swept after load.
- Saves are debounced (~700 ms). Fingerprints (`last_settings_json`,
  `last_libraries_json`) prevent a slider move from rewriting the library file.

### Migration (`storage/migrate.rs`)

Reads the legacy single file, groups sounds by `library_id`, writes the new files, and
renames the old one. The settings file is the “migration finished” marker, so a partial
import retries next launch.

---

## 4. Audio

### Format

Everything internal is **48 kHz, stereo, interleaved `f32`**. `audio/decode.rs` decodes
any supported file with Symphonia and converts it once; `audio/resample.rs` performs
stateful linear interpolation with an absolute output-frame counter, so the result does
not depend on decode block size. Sound effects are capped at 20 minutes.

### Playback registry (`playback.rs`)

`Playback` maps sound IDs to `Playable` entries:

| Field | Meaning |
| --- | --- |
| `path`, `volume` | Current file and gain |
| `samples` | `Some(Arc<Vec<f32>>)` when kept in RAM |
| `loading`, `pending_play` | Decode in flight / play when it finishes |
| `error` | Last decode/link error |
| `generation` | Invalidates stale loader results |
| `retain` | Keep decoded samples after playing |

- **RAM sound:** trigger plays the cached `Arc` immediately.
- **Disk sound:** trigger queues a `Job`; the loader decodes, plays if
  `pending_play`, and drops the samples unless `retain`.
- The loader is a single thread reading a channel, so jobs are serialized.
- `PlaybackEvent::{Loaded, Failed}` is polled by the UI for logs.
- Hotkeys call `Playback::play` from the hotkey thread; state is behind a `Mutex`.

RAM is assigned only to sounds that have a hotkey and are in the active tab. Switching
tabs calls `sync_active_library`, updates hotkey bindings, sets `retain`, and preloads.

### Mixer (`audio/engine.rs`)

```text
Mixer
  voices: Vec<Voice>       sound effects (exclusive)
  loop_rx/loop_chunk       streamed loop track (exempt)
  monitor_gain/cable_gain  output levels (linear)
  master_gain              dB → linear
```

`Mixer::render(out)`:

1. Mix each `Voice` (with a 2.5 ms fade-in and 5 ms fade-out).
2. Mix the loop chunk, wrapping as needed, scaled by `loop_volume`.
3. Apply `master_gain` and clamp to ±1.0.
4. Remove finished voices.

`Engine::play(id, samples, volume)` is **exclusive**: every current voice is told to
fade out before the new one is pushed. `stop_all()` does the same to every voice. The
loop is a separate field and is deliberately untouched by both.

The mixer thread wakes every 1 ms, renders 5 ms chunks, catches up at most four chunks
if late, and pushes to each `Ring` only when that output has a consumer.

### Output rings and meters

- Each output has one `Ring` (producer: mixer, consumer: WASAPI thread).
- Capacity is **0.15 s**. This bound is the main start/stop latency.
- `push_scaled` applies the per-output volume; `pop_into` is called by the device
  thread, converts samples, and records the peak with an atomic.
- `Engine::take_levels()` reads/clears those consumer-side peaks, so the UI meter
  matches what the device received, not what the mixer prepared.
- The sidebar applies its own decay/peak-hold; the audio layer only reports peaks.

### WASAPI (`audio/wasapi.rs`)

- Enumerates active render endpoints on a COM thread.
- Opens each endpoint in **shared mode** with event-driven callbacks, the device mix
  format, and a per-device render thread.
- Pulls 48 kHz stereo from the ring, linearly resamples to the device rate, maps
  channels, and writes the WASAPI buffer. If the ring is empty it pads silence.
- Reports `DeviceStarted`, `DeviceError`, `Stopped`, and warnings through a channel.
- Two routes may not share an ID; `set_routes` keeps one and warns.
- Device changes are applied by `App::apply_routes`, which calls `Engine::set_routes`.

### The looping track

`audio/stream.rs`:

- Spawns one decoder thread when Play is pressed. It opens the file, seeks/reopens at
  end of pass, converts to 48 kHz stereo, and sends 1024-frame chunks.
- The channel holds 16 chunks (~0.34 s); a full channel parks the decoder in `send`.
- `LoopState` is `Idle`, `Playing`, or `Error`. Start is optimistic (Playing at once);
  errors replace it. There is no Buffering/Underrun state.
- Stop drops the receiver first (unblocking the sender), clears the mixer chunk, joins
  the decoder, and returns to Idle.
- Loop volume is applied in the mixer. It does not alter Monitor/Cable.
- Any length works; memory stays under ~1 MB of buffers.

### Volume and gain

- Stored sound/output/loop volumes are linear `0..1`.
- UI positions are logarithmic over −60…0 dBFS (`ui/volume.rs`), so the slider matches
  perceived loudness. All callers use the same curve and percentage formatter.
- Master gain is separate, `0..+12 dB`, applied to the whole mix; right-click resets.
- The slim `volume::bar` supports click/drag and wheel; it consumes the scroll while
  hovered.

---

## 5. Hotkeys and tray (`hotkeys.rs`)

- `Hotkey` stores modifiers + a virtual-key code, parsed from strings such as `Alt+2`,
  `Ctrl+Shift+S`, `F8`.
- The service runs one background thread with a hidden message window.
- `set_bindings` unregisters the previous set and registers the new one with
  `RegisterHotKey`; duplicate labels are rejected by the app before this call.
- `WM_HOTKEY` triggers `Playback::play` directly, then sends `ServiceEvent::Pressed` so
  the UI can flash the card.
- Registration failures are sent back as `Registered` events and shown under the hotkey
  pill plus the activity log.
- The tray icon supports left-click/double-click show, right-click menu (Show, Stop
  all, Quit), and a tooltip updated with playing/RAM counts.
- Only the active library's hotkeys are registered. Win-key chords and mouse buttons
  are not supported; no administrator rights are needed.

---

## 6. UI (`app.rs` and `src/ui`)

### Frame structure

`App::update` runs every frame:

1. Detect Settings opening/closing; set fade timestamps and clear text focus.
2. Poll playback/engine/hotkey events.
3. Handle close requests, global shortcuts, window-edge resize, `tick`.
4. Draw the title bar and status bar.
5. If Settings is open: left settings rail + content panel.
   Otherwise: board sidebar (right) + main page.
6. Draw window chrome, drop overlay (board only), drag ghost, and modals.

`tick` also debounces saves, updates the meters, and schedules the next repaint
(33 ms while audio is moving, 120 ms idle/hidden).

### Board

- **Title bar:** logo, library tabs, window buttons; draggable except over tabs.
- **Sidebar:** Load sounds, layout switch, Monitor/Cable devices + volumes + link,
  master gain, RAM row, peak meter, loop card (when enabled), Settings button.
  Compact layout (<620 px sidebar height) hides labels, dB scale, credit and inline RAM.
- **Main page:** search toolbar + RAM pill, grid/list of the active library, empty/no
  results states, drop handling.
- **Cards/rows:** name, hotkey pill, RAM chip, missing state, volume bar, progress
  line, context menu (play, locate, reveal, move, rename, remove).
- **Actions:** UI pushes `Action` values; `handle_actions` is the single place that
  mutates playback or opens dialogs. This keeps painting separate from side effects.
- **Selection/search/drag:** search filters name/path; drag reorders or moves between
  libraries; Delete/undo uses a one-slot `UndoDelete`.

### Settings screen

- Reached from the sidebar, `Ctrl+,`, or `--settings`; replaces the board.
- Left rail: Back, Playback, Audio, Appearance, About (bottom), autosave note, version.
- Content: fixed header + per-tab scroll; grouped cards and label/control rows;
  `SettingsTab` holds the tab metadata.
- Immediate save; no modal, no backdrop. `Esc` closes a popup first, then Settings.
- Shared helpers: `settings_group`, `settings_row`, `settings_switch`,
  `settings_nav_item`, `settings_back`, `gain_control`.
- Board-only shortcuts are disabled while Settings is open; `Ctrl+Q` still quits.

### Modals

Capture hotkey, rename, and activity log remain modal overlays. They take keyboard
input directly where needed (hotkey capture) and are drawn after the board/settings.

### Theme system (`ui/theme.rs`, `theme_file.rs`, `theme_defaults.rs`)

- `config/themes/<id>.md`: heading + `| token | value |` table.
- Tokens: bg, panel, panel_deep, card, card_hover, field, border, border_soft, text,
  muted, faint, accent, purple, amber, red (optional `accent_soft`).
- Missing/invalid token → Graphite's value. Filename stem = theme id; heading = display
  name. `accent_soft` is derived when absent.
- Startup installs the five embedded themes without overwriting existing files.
- Reload is manual; an unknown active theme falls back to Graphite.
- The runtime registry is a global `RwLock` palette; `apply_style` maps it to egui.
- Radii: `SM=5`, `MD=8`, `LG=12`, `XL=14`; use these instead of literals.
- Fonts: Satoshi regular/medium; `theme::ui_font` / `medium`; `truncate` for ellipsis.

### Motion (`ui/anim.rs`)

- 60–90 ms ease-out tweens for hover, press, fades and the layout pill; no bounce.
- `animate_ui` **and** the Windows client-area animation flag gate everything; when
  disabled, tweens snap.
- Tween state lives in egui memory keyed by stable IDs; repaints are requested only
  while settling.

---

## 7. Feature walkthroughs

### Add sounds

`Load sounds`/drag-drop → `add_files` expands folders, filters supported extensions,
skips duplicates, assigns IDs, registers with `Playback`, updates the library, saves.

### Play a sound

Card click/Space/hotkey → `Playback::play`. Cached RAM samples go straight to
`Engine::play`; otherwise the loader decodes, then plays. `Engine::play` fades out all
current voices (exclusive) and starts the new one. If the file is missing, the UI
offers Locate.

### Libraries and active tab

Tabs are `Library { id, name }`; sounds carry `library_id`. Switching calls
`sync_active_library`: rebinds hotkeys for the new tab, sets RAM retention, preloads,
and stops sound playback. The loop is unaffected.

### Missing files

`Playback::info` reports missing if no samples and the path is gone. Cards/rows show a
red state and clicking opens the file picker; `relink_sound` updates the path and
reloads. The folder is never scanned automatically.

### Volume and gain

Sound volume bars and Settings rows use `ui/volume.rs`. Output sliders use the same
curve. `set_slot_volume` applies the link rule. Master gain is dB-only and global.
All values are persisted immediately (debounced).

### Loop

Enable in Settings → Playback. Choosing a file stores `loop_path`. Play starts the
decoder thread and attaches its channel; Stop detaches and joins. The card's bar and
Settings row change `loop_volume`. It survives tab switches, `Stop all`, and tray
minimize; it stops on quit or when disabled.

### Settings and version

`About` reads the version from `Cargo.toml` at compile time. Config folder uses
`explorer`. Developer values are constants until replaced.

---

## 8. Concurrency and invariants

- **UI thread owns App state.** Playback/engine state is behind mutexes because the
  hotkey/mixer threads touch it.
- **Lock order:** UI → `Playback.state` → `Engine.mixer`; avoid the reverse. The
  encoder/decoder never calls back into the UI.
- **Channels:** loader jobs, playback events, audio events, loop chunks, hotkey/tray
  events. Channels are drained on the UI thread.
- **Generations:** loader results are ignored if the sound's generation changed while
  decoding (path change/reload).
- **Exclusive voices:** only `mixer.voices`; the loop lives outside that list.
- **All audio is 48 kHz stereo f32** until the WASAPI thread converts per device.
- **Saved volumes are linear; displayed positions are logarithmic.**
- **Only the active library is live** for hotkeys and RAM; the loop is global.
- **No audio file is copied.** Paths are references.
- **Writes are atomic** and backed up; no growing history.
- **One process per exe path.**
- **Do not block the UI thread** on decode, join, or device work; if an operation can
  take time, it belongs on a worker thread.

---

## 9. Errors and logging

`StatusKind` is `Info`, `Success`, `Warning`, or `Error`; the status bar shows the latest
entry and `Activity log` keeps the last 200. Sources:

- storage recovery/migration warnings;
- theme parse/reload issues;
- device enumeration/open/stop failures;
- hotkey registration conflicts;
- decode/load/missing-file failures;
- loop decoder errors;
- save failures (reported once until a save succeeds).

Missing audio is not fatal: the sound stays in the library and can be relinked.
Device failures leave the engine running with the other output. Save failures keep the
current in-memory state and retry on the next debounce.

---

## 10. Testing and diagnostics

```powershell
cd sounddeck
cargo test
cargo build --release
.\target\release\sounddeck.exe --diag path\to\sound.wav
```

- 38 unit tests cover storage recovery/roundtrip, resampling (chunk invariance),
  loop streaming (wrap, stop, file release, errors), mixer loop gain/wrap, volume
  curve/clamps, and theme-file parsing.
- Tests use temporary directories named with the process/time; no user data.
- `--diag` enumerates devices, opens Monitor/Cable, decodes a file, plays it, and
  prints WASAPI events.
- UI layout and interaction are checked manually via screenshots (the development
  workflow uses `PrintWindow` so the app does not need foreground focus).
- Manual regression list: add/play/relink/remove sounds, tab switching, hotkey RAM,
  search, grid/list, drag/reorder, tray/close, loop play/stop/volume, device
  change, theme reload, Settings tabs/back/Esc.

---

## 11. Where to change what

**Add a setting**

1. Add the field to `storage::Settings`, its default, and `normalize()`.
2. Serialize automatically; update the example in this file and README.
3. Add the control in the relevant Settings page.
4. Apply it where it belongs (engine/config) and call `mark_dirty()`.
5. Add a storage test if the value is clamped/transformed.

**Add a theme token**

1. Add it to `Palette` in `ui/theme.rs`.
2. Add defaults in `ui/theme_defaults.rs`.
3. Map the token in `ui/theme_file.rs` (and the template).
4. Use `theme::token()` in painting; missing values already fall back.

**Add a sound action**

1. Add a variant to `Action`.
2. Handle it in `App::handle_actions`.
3. Draw the trigger in the card/row/context menu.
4. Keep side effects out of painting.

**Add a Settings tab**

1. Add a `SettingsTab` variant, title, subtitle, icon.
2. Add it to `ALL` and the rail (About stays pinned).
3. Add `settings_page_*` and the match in `settings_content`.

**Add or change a volume**

Use `ui/volume.rs`; store linear, display via `to_position`/`percent_text`. Do not add
another curve.

**Add a file format**

1. Enable the Symphonia feature in `Cargo.toml`.
2. Add the extension to `model::SUPPORTED_EXTENSIONS`.
3. Check the file dialog filter and decoding hint; add a test file.

**Change audio routing or mixing**

`Engine` owns devices/routes; `Mixer` owns voices/loop/gains; `Ring` is the only
producer→device boundary. Keep blocking work off the mixer thread.

---

## 12. File reference

| File | Role |
| --- | --- |
| `src/main.rs` | Process start, single instance, viewport, `--diag`, eframe |
| `src/app.rs` | All UI/state/actions; board, Settings, modals, helpers |
| `src/model.rs` | Paths, portable/relative resolution, extensions, formatting |
| `src/playback.rs` | Sound registry, RAM/disk decode jobs, loader thread, events |
| `src/hotkeys.rs` | Hotkey parse/register, message window, tray, focus |
| `src/icon.rs` | PNG decode/resize for window and tray icons |
| `src/audio/decode.rs` | One-shot Symphonia decode to 48 kHz stereo, 20-min cap |
| `src/audio/resample.rs` | Stateful resampler (chunk-invariant) |
| `src/audio/stream.rs` | Loop decoder thread, 16-chunk channel, states |
| `src/audio/engine.rs` | Mixer, voices, loop, rings, gains, meter peaks, routes |
| `src/audio/wasapi.rs` | Device enumeration, shared-mode render threads |
| `src/storage/mod.rs` | Settings/libraries schemas, normalize, load/save |
| `src/storage/io.rs` | Atomic writes, BOM, rolling backup, recovery, tmp sweep |
| `src/storage/migrate.rs` | Legacy `sounddeck.json` import |
| `src/ui/theme.rs` | Palette registry, radii, fonts, style, helpers |
| `src/ui/theme_file.rs` | Markdown theme parser/template/fallback |
| `src/ui/theme_defaults.rs` | Embedded palettes |
| `src/ui/theme_color.rs` | Hex parsing and blending |
| `src/ui/anim.rs` | Ease/tween/hover/press, OS animation flag |
| `src/ui/volume.rs` | Log curve, percentage, shared volume bar |
| `build.rs` | Windows icon/version resource |
| `assets/` | Icon and Satoshi font files |

---

## 13. Glossary

| Term | Meaning |
| --- | --- |
| **WASAPI** | Windows audio API used for shared-mode output |
| **Shared mode** | The device mixes this app with others; higher latency than exclusive |
| **Endpoint** | A Windows render device (headphones, speakers, virtual cable) |
| **Ring** | Fixed-size sample queue between the mixer and one device thread |
| **Voice** | One playing sound instance inside the mixer |
| **Exclusive playback** | A new sound fades out previous sounds |
| **Loop track** | Separate streamed, repeating background file |
| **RAM preload** | Decoded samples kept in memory for instant trigger |
| **dBFS** | Decibels relative to full scale; 0 is the ceiling |
| **Linear gain** | Raw amplitude multiplier stored in config (`0..1`) |
| **Log position** | Slider position over −60…0 dBFS used for display |
| **Portable path** | Path relative to the executable folder |
| **BOM** | Optional UTF-8 byte-order mark; tolerated on read only |
| **Gapless** | Decoding metadata that removes MP3/AAC encoder padding |
| **Registry / generational load** | Per-sound state that ignores stale decode results |

---

Keep this file current when a schema, audio rule, hotkey rule, or Settings tab changes.
The README stays user-facing; this document is for people changing the code.
