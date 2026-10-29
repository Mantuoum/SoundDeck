# SoundDeck — Developer Architecture

This is the developer guide for SoundDeck: what each part of the code does, how the
pieces talk to each other, and where to change things. It assumes you know Rust basics
but not this application.

- **User manual:** `README.md`
- **Roadmap / decisions:** `../plan.md`

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
| Sound registry | `src/playback/mod.rs` | Jobs, cache, streaming handoff, duration probe, per-sound volume/trim |
| Diagnostics | `src/stats.rs` | `--stats` counters and the 5 s log line |
| Playback policy | `src/playback/policy.rs` | RAM vs stream decisions, caps, admission, process valve |
| Audio device I/O | `src/audio/wasapi.rs` | Device enumeration and render threads |
| Mixer | `src/audio/engine.rs` | Voices, loop, rings, gains, metering |
| Decoding | `src/audio/reader.rs` | One Symphonia reader for preload, loop and one-shot streams |
| Decoding (full) | `src/audio/decode.rs` | Collect the reader into RAM, preload only |
| Streaming | `src/audio/stream.rs` | Loop and one-shot decoder threads, chunk channel |
| Resampling | `src/audio/resample.rs` | Stateful 48 kHz stereo conversion |
| Storage | `src/storage/*` | JSON schemas, atomic writes, backups, migration |
| Hotkeys/tray | `src/hotkeys.rs` | Low-level keyboard hook, tray icon, message loop |
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
| Duration probe | `Playback::spawn_probe` | Per unknown-duration sound | Reads a streamed file's length off the trigger path |
| Loop decoder | `LoopStream::spawn` | While the loop plays | Decode the loop file into a chunk channel |
| WASAPI render | `spawn_render`, once per output | While the device is open | Pull the ring, convert to the device format, write WASAPI |
| Hotkey/tray window | `HotkeyService::start` | App lifetime | Message-only window: hotkeys, tray, focus |
| Device enumeration | `wasapi::list_devices` | One call | COM enumeration on a short-lived thread |
| Shell call | `App::spawn_shell_call` | One per Explorer action | `SHParseDisplayName` + `SHOpenFolderAndSelectItems` off the frame; failures come back on the log channel (`explorer_tx`) |

### Preload cache and memory limits (`playback/policy.rs`, `playback/mod.rs`)

- Retained decoded audio: **32 MB**, **16 sounds**, **30 s** per sound.
- A longer sound is refused for preload *before* decoding (container probe) and
  streams from disk; the log says why.
- Admission is LRU: the not-currently-playing buffer with the oldest `last_used`
  is dropped to make room. The playing sound is never evicted.
- **Eviction only pays for cache caps.** `Refusal::evictable()` is true for
  `SoundCap`/`ByteCap` only; a sound that is too big on its own, a process at its
  ceiling, or a decoder that produced nothing is refused with **no victims**, so one
  unfittable file cannot dump a preload batch that has already loaded (skip-and-continue).
  The refusal is reported once per verdict and `set_retain` is a no-op when the flag did
  not change (the tab-switch sync re-asserts it on every switch), so re-asserting the same
  retention neither re-decodes nor re-logs it.
- The refusal wording is `Refusal::message(name)` — the size caps read
  `"<name>" is too big; Streaming from disk`, the 30 s cap `"<name>" is too long;
  Streaming from disk`, an empty decode `"<name>" has no audio`. `PlaybackEvent::Refused`
  carries `{ id, reason }`; the UI supplies the name.
- A process commit ceiling (125 MB) is the last-resort valve; if the query fails,
  the cache fails closed and refuses to grow.
- Everything that is not resident streams: one bounded channel (8 × 1024 frames,
  ~130 KB) regardless of file length. No playback path decodes a whole file.
- `show_ram_state` (default off) controls the chips, counts and toolbar pill; the
  per-sound retention control lives in the context menu and Settings.
- A panic writes `crash.log` next to the executable before aborting
  (`panic = "abort"` leaves no other trail).

### Data flow

```text
Mouse/keyboard ──► App::update ──► Action / direct call ──► Playback / Engine / cfg
Global hotkey  ──► hotkey thread ──► Playback::play ─────────┘
Loop Play      ──► Engine::start_loop ──► LoopStream thread ──► channel ──┐
                                                                          ▼
Playback::play ──► resident? ──► Engine::play ──► Mixer (buffer voice) ──┐
                └► stream  ──► stream thread ──► channel ──► Mixer (stream voice) ─┤
                                                                                  ▼
                                                                    master gain, then rings
                                                              │
                                              WASAPI thread (per output) ──► speakers
```

The UI never blocks on decoding or audio. Calls that need work (decode, device start,
loop open) hand it to one of the threads above.

---

## 2. Startup and shutdown

### Startup (`main.rs`)

1. `timeBeginPeriod(1)` improves the mixer's 5 ms tick; `--stats` starts the counter
   thread first (it must cover the whole run).
2. `--diag` runs the audio smoke test and exits; `--settings` opens on the Settings
   screen; otherwise continue.
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
- `HotkeyService::shutdown` unregisters hotkeys (including stop-all) and removes the tray icon.
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
| `loop_on_monitor` / `loop_on_cable` | Route the loop to each output independently |
| `window_size` | Parsed for old files, ignored |
| `animate_ui` | UI motion preference |
| `minimize_to_tray` | Minimize hides to the tray instead of the taskbar |
| `show_gain_slider` | Show the master-gain slider in the board sidebar |
| `show_ram_state` | Reveal RAM/disk chips, counts and the toolbar pill (default off) |
| `stop_all_hotkey_enabled` / `stop_all_hotkey` | Stop-all hotkey switch and its label form |
| `cycle_tabs_ctrl_tab` | `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle libraries (default off; the chords pass through untouched) |

`Settings::normalize()` clamps volumes, gain, trims/empties the loop path, and fills
missing fields through `#[serde(default)]`. `LibrariesFile::normalize()` clamps each
sound's `volume` and `gain_db` and fixes ids and empty names.

### `libraries.json`

```json
{
  "version": 2,
  "libraries": [
    { "id": 1, "name": "Main", "sounds": [
      { "id": 2, "name": "horn", "path": "audio/horn.wav",
        "hotkey": "Alt+2", "volume": 1.0, "gain_db": 0.0, "ram": true }
    ] }
  ]
}
```

- Tabs are the array order. Sounds are nested; `library_id` is not stored.
- `ram: null` means “inherit the global preload setting”.
- `gain_db` is the per-sound trim (clamped `-12..=+12` on load through
  `audio::engine::clamp_gain_db`); a file from before the field is `0.0` through
  `#[serde(default)]`, and a value written under the old `-24` floor loads at `-12`.
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

### Export/import bundle (`storage/bundle.rs`)

`Export configuration` writes settings + libraries into one JSON document
(`{ "app": "sounddeck", "bundle": 1, "settings": …, "libraries": … }`) via
`io::write_text_atomic` (atomic, no rolling `.bak`: the target is a file the user picked).
Import parses and validates it (`app` stamp, bundle version, then the same
`normalize()` a load runs), and only `App::apply_import` touches the board — nothing is
applied until the user confirms the dialog. The bundle's two members are the config
files' own shapes, so export → import restores exactly what a folder copy would.
Audio files are never copied (FR-5.3.14).

---

## 4. Audio

### Format

Everything internal is **48 kHz, stereo, interleaved `f32`**. `audio/decode.rs` decodes
any supported file with Symphonia and converts it once; `audio/resample.rs` performs
stateful linear interpolation with an absolute output-frame counter, so the result does
not depend on decode block size. Sound effects are capped at 20 minutes.

### Playback registry (`playback/mod.rs`)

`Playback` maps sound IDs to `Playable` entries:

| Field | Meaning |
| --- | --- |
| `path`, `volume`, `gain_db` | Current file, stored linear volume and per-sound dB trim |
| `samples` | `Some(Arc<Vec<f32>>)` when resident |
| `loading`, `pending_play` | Preload in flight / play when it finishes |
| `error`, `missing` | Last decode/link error; existence checked at startup and on playback when not resident |
| `lock` | Open read handle that denies delete/rename while the sound is on the board |
| `refused` | Why the cache declined; cleared by Reload or toggling preload. A refusal the cache cannot pay for (`evictable()` false) evicts nothing and is reported once |
| `duration` | Container duration for stream progress |
| `probe_pending` | A probe worker is reading that duration right now (one per sound) |
| `generation` | Invalidates stale loader results |
| `retain` | Per-sound preload toggle |

- **Resident:** trigger plays the cached `Arc` immediately.
- **Not resident:** `policy::route` decides between a preload job (≤ 30 s, fits the
  budget) and a stream thread. There is no full-file decode on the trigger path.
- **Stream:** a bounded channel feeds a streamed mixer voice; memory is constant.
- **Duration probe:** a streamed sound with no known length gets one detached
  `sounddeck-probe` worker (`probe_pending` deduplicates, `generation` discards stale
  results), so progress appears from the next trigger instead of probing on the
  trigger path.
- **Lock:** every configured file holds a read handle whose share mode omits
  delete. Windows therefore refuses rename/delete until the sound is removed from
  the board or the app exits. Decoders can still open the file.
- The loader is a single thread reading a channel, so preload jobs are serialized, and
  it moves on to the next job when one sound cannot be admitted (skip-and-continue).
- `PlaybackEvent::{Loaded, Failed, Refused, Warning}` is polled by the UI.
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

`Mixer::render(monitor, cable)`:

1. Mix each `Voice` into the common monitor buffer (2.5 ms fade-in, 5 ms fade-out).
2. Copy it to the cable buffer.
3. Mix the loop chunk, wrapping as needed, scaled by `loop_volume`; write it only to
   the outputs enabled by `loop_on_monitor` / `loop_on_cable`. The chunk is consumed
   even when both are off, so the stream keeps its position.
4. Apply `master_gain` to both buffers and clamp to ±1.0.
5. Remove finished voices; the ring push applies each output's volume.

`Engine::play(id, samples, volume)` is **exclusive**: every current voice is told to
fade out before the new one is pushed. `stop_all()` does the same to every voice. The
loop is a separate field and is deliberately untouched by both.

The mixer thread wakes every 1 ms, renders 5 ms chunks, catches up at most four chunks
if late, and pushes to each `Ring` only when that output has a consumer.
`Mixer::active()` reports each live voice with `progress: Option<f32>`: `None` while a
stream's total length is still unknown (the UI then draws no progress line instead of a
full one), `Some(0..=1)` once the probe has reported the duration.

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
- Device changes are applied by `App::apply_routes`, which calls `Engine::set_routes`
  with a real device from the current enumeration: `route_slot` resolves the stored
  endpoint (id → name) and, when it is gone, substitutes the Windows default (Cable:
  a `cable`/`virtual`-named device first, never the Monitor's) and reports it. The
  stored id and name are left alone so `Refresh devices` restores a re-plugged
  device; `retune_slot` refreshes a stale name in place (and saves).

### The looping track

`audio/stream.rs`:

- Spawns one decoder thread when Play is pressed. It opens the file, seeks/reopens at
  end of pass, converts to 48 kHz stereo, and sends 1024-frame chunks.
- The channel holds 8 chunks (~170 ms); a full channel parks the decoder in `send`.
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
  hovered and carries no icon.
- `audio::engine::clamp_gain_db` is the single range clamp (`-12..=+12`, NaN/inf → `0`):
  `effective_gain`, `Settings`/`LibrariesFile::normalize` and the gain dialog all use it.
  The dialog snaps to 0.5 dB steps and applies nothing before OK.

---

## 5. Hotkeys and tray (`hotkeys.rs`)

- `Hotkey` stores modifiers + a virtual-key code, parsed from strings such as `Alt+2`,
  `Ctrl+Shift+S`, `F8`.
- The service runs one background thread with a hidden message window and a low-level
  keyboard hook (`WH_KEYBOARD_LL`).
- `set_bindings` updates the hook's binding list; no OS registration is involved, so a
  hotkey never conflicts with another app. Within the app a chord has exactly one
  owner: `chord_owner` / `duplicate_sounds` hold the rule (stop-all beats sounds, the
  first sound in tab order beats later ones), the capture dialog refuses a taken chord
  before it is saved, and `App::sync_hotkeys` re-applies the rule on every rebind,
  unbinding each loser exactly like a manual clear.
- The hook callback looks up the modifier+key and posts `PLAY_MSG` to the hidden
  window; the window plays the sound and sends `ServiceEvent::Pressed` for the UI flash.
- The callback always calls `CallNextHookEx`, so the focused app receives the original
  keystroke. Auto-repeat is ignored per key press until key-up.
- Held keys live in a `HeldKeys` set, pruned against the OS key state on every event, so
  modifiers are derived from what is physically down and a missed key-up cannot wedge a
  hotkey.
- The service reports `ServiceEvent::{Pressed, StopAll, Tray, TrayReady, Fatal}`; the UI
  drains those on its own thread.
- The tray icon supports double-click show/hide, right-click menu (Show/Hide, Stop
  all, Quit), and a tooltip updated with playing/RAM counts. Single left-click is
  deliberately inert.
- Minimize follows `minimize_to_tray`: the custom button and an observed OS minimize
  hide to the tray when enabled, otherwise the window stays in the taskbar. Showing
  from the tray restores, focuses and clears stale meter peaks.
- Only the active library's hotkeys are bound. Win-key chords are bindable: egui carries
  no Win modifier flag, so the capture path folds the physical Win state into the chord
  (`chord_from_egui`); when the shell swallowed the chord entirely and egui saw no event,
  it derives the chord from the physical keyboard (`held_chord`) with the same
  `KEYS`/`Mods::from_keys` tables the hook matches with. Mouse buttons are unsupported; no
  administrator rights are needed. Because the hook passes the key through, a Win chord
  also fires the Windows shortcut (the README says so).
- `BLOCKED_CHORDS` (Windows editing set + the board's own shortcuts + `Alt+F4`) is never
  bindable: `blocked_reason` gives the one message (`"<chord> is a system shortcut and
  can't be used"`) that capture shows, and `parse_bindable` is the config-facing parser, so
  a hand-edited blocked chord is unbound at load (sound or stop-all) through the normal
  invalid-hotkey path, with the sound kept and a warning logged.

---

## 6. UI (`app.rs` and `src/ui`)

### Frame structure

`App::update` runs every frame:

1. Detect Settings opening/closing; set fade timestamps and clear text focus.
2. Poll playback/engine/hotkey events and the shell-failure log channel.
3. Handle close requests, global shortcuts, window-edge resize, `tick`.
4. Draw the title bar and status bar.
5. If Settings is open: left settings rail + content panel.
   Otherwise: board sidebar (right) + main page.
6. Draw window chrome, drop overlay (board only), drag ghost, and modals.

`tick` also debounces saves, updates the meters, and hands the frame pacing to
`repaint_interval()`: 33 ms while audio is moving, 100 ms with Settings open during
audio, and 1 s when idle. `handle_actions` only requests an extra frame when it
actually did something.

### Idle and hidden behaviour

A hidden window is *minimized*, not hidden with `Visible(false)`. With the latter,
winit stops delivering paints, eframe stays in `ControlFlow::Poll` and burns a full
core while `update` never runs, so tray events are never drained and the window can
never be restored. Minimizing keeps the frame loop alive and lets the idle timer do
its job. While `hidden`, `update` skips the whole paint pass and runs `hidden_tick`
only (loop state, save debounce, tray tooltip).

`ITaskbarList::DeleteTab` removes the taskbar button while parked in the tray and
`AddTab` restores it. The OS-minimize detector reacts to a transition only, with a
400 ms guard after a restore, because a level check re-hid the window immediately.

`--stats` writes one line every 5 s to `%TEMP%\sounddeck-stats.log`: fps, frame
time, repaint interval, mixer wakeups, ring underruns and loop state. The process
runs at below-normal priority; the mixer and render threads raise themselves to
above-normal so audio still meets its deadlines.

### Board

- **Title bar:** logo, library tabs, window buttons; draggable except over tabs.
- **Sidebar:** scrollable part is Load sounds, layout switch, Monitor/Cable devices +
  volumes + link, master gain (optional), stop-all card (when its hotkey is enabled),
  loop card (when enabled); the footer below it is **pinned** (credit → peak meter →
  Settings button) and never scrolls. Compact layout (<620 px sidebar height) hides
  labels, dB scale and credit.
- **Main page:** search toolbar + RAM pill, grid/list of the active library, empty/no
  results states, drop handling. The toolbar and the grid both size from
  `board_content_width(ui)` (available width less the floating scroll-bar gutter), which
  is what keeps the pill and the last column on one right edge.
- **Cards/rows:** name, hotkey pill, RAM chip, missing state, volume bar, progress
  line (only once the total duration is known), context menu (play, stop, **set gain…**,
  locate, reveal, move, rename, remove).
- **Actions:** cards/rows and the library bar push `Action` values; `handle_actions`
  drains one buffer per frame. Singleton chrome (sidebar, title bar, status bar,
  Settings, tray) mutates directly - wrapping a single `ComboBox` in an event gains
  nothing.
- **Selection/search/drag:** search filters name/path; drag reorders or moves between
  libraries (the drop target is resolved by sound id, so a list that shifted mid-drag
  cannot move the wrong sound); a library **tab** is a drop target too — it tints while a
  drag is over it, the drop moves the sound with the context menu's semantics, and the
  active tab is unchanged; Delete/undo uses a one-slot `UndoDelete`.

### Settings screen

- Reached from the sidebar, `Ctrl+,`, or `--settings`; replaces the board.
- Left rail: Back, Playback, Appearance, Window, About (bottom).
- Content: fixed header + per-tab scroll; grouped cards and label/control rows.
  `SettingsTab` holds the tab metadata. Text is intentionally terse.
- Layout metrics live in the `SETTINGS_*` consts (SPEC FR-5.9.8 / §7.2); rows stack
  below `SETTINGS_ROW_STACK_W`.
- Playback owns loop enable/file, loop output routing and hotkey RAM; Appearance owns
  theme/motion; Window owns minimize-to-tray, sidebar gain visibility and `Ctrl+Tab`
  cycling (`cycle_tabs_ctrl_tab`, default off); About owns
  version/config/dev plus the **Data** group (export, import, reset all settings).
- Every page that owns fields ends with a **Defaults** card whose Reset restores that
  page's fields (`Settings::reset_group`, one field list per group in `storage/mod.rs`);
  libraries and sounds are never part of a reset.
- Devices, output volumes, link and master gain live in the sidebar, not Settings.
- Immediate save; no modal, no backdrop. `Esc` closes a popup first, then Settings.
- Shared helpers: `settings_group`, `settings_value` (one label/control row),
  `settings_toggle` (row + switch + changed), `settings_detail`, `settings_divider`,
  `settings_switch`, `settings_nav_item`, `settings_back`; `pressable` is the button
  press tween + overlay.
- Values that walk the sound list or lock the playback registry (`ram_summary`,
  `cache_usage`) are cached in `PlaybackStats`; the theme list is cached in
  `theme_choices` and rebuilt on reload.
- Board-only shortcuts are disabled while Settings is open; `Ctrl+Q` still quits.
- While a text field has focus (`ctx.memory().focused()`), every board shortcut is
  suppressed: add files, focus search, layout, undo, delete, play, rename. `Ctrl+Q`
  and `Ctrl+,` stay available, and `Esc` never stops playback from a focused field.

### Modals

Capture hotkey, rename, set gain, delete library, import confirmation, reset-all
confirmation and activity log use `egui::Modal` with a **transparent** backdrop, so the
board stays visible without a dim layer and input cannot leak through. They share
`modal_header` + `close_button` (red hover), are anchored on `App::content_rect` — the
rect the body (board or Settings content) was painted into — so a dialog opened from
Settings centres on the settings content instead of the window, and take keyboard input
directly where needed (hotkey capture, the gain field).
The delete-library dialog states the sound count and offers move-to-a-library,
delete-the-sounds, or cancel; nothing is applied until a button is pressed. The import
dialog states the incoming sound/library counts, and `Replace` is the only path into
`App::apply_import`; the reset dialog only guards `Settings::reset_all`.
The sidebar divider is painted inside the central-panel layer, not as a separate
top-level layer, so it can never draw over modals or popups.

### Theme system (`ui/theme.rs`, `theme_file.rs`, `theme_defaults.rs`)

- `config/themes/<id>.md`: heading + `| token | value |` table.
- Tokens: bg, panel, panel_deep, card, card_hover, field, border, border_soft, text,
  muted, faint, accent, purple, amber, red (optional `accent_soft`).
- Missing/invalid token → Graphite's value. **The file stem is the id** (that is what
  `settings.json` stores and every lookup matches); the `# Heading` is the display name.
  `theme::available()` labels an entry `"<name> (<id>)"` when the heading does not spell
  the id, so two files sharing a heading stay distinguishable. `accent_soft` is derived
  when absent.
- Seeding writes the five embedded palettes only while the folder holds **no** Markdown,
  and never overwrites an existing file.
- Reload is manual; `reload_from(dir)` is the test seam behind `reload()`. A reload
  re-resolves the active id (an edited colour repaints without reselecting) and an
  unknown id falls back to Graphite with a warning naming the id and the available ones;
  the UI then corrects the stored id to `graphite`.
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
current voices (exclusive) and starts the new one. If the file is missing, the trigger
logs an error and the sound is relinked from its context menu (`Locate`).

### Libraries and active tab

Tabs are `Library { id, name }`; sounds carry `library_id`. Switching calls
`sync_active_library`: rebinds hotkeys for the new tab, sets RAM retention, preloads,
and stops sound playback. The loop is unaffected. Deleting a library opens a dialog
(`deleting` state) that moves its sounds to a chosen library or removes them from the
board; audio files are never touched, and the last library cannot be deleted.

### Missing files

`Playback::info` reports missing if no samples and the path is gone. Cards/rows show a
red state; triggering the sound logs `file missing - right-click > Locate` and does
nothing else, and `Locate` in the context menu opens the picker. `relink_sound` updates
the path (and its search key) and reloads. The folder is never scanned automatically.

### Volume and gain

Sound volume bars and the loop card use `ui/volume.rs`. Output sliders use the same
curve. `set_slot_volume` applies the link rule. Master gain is dB-only and global;
the sidebar row can be hidden in Settings → Window.
All values are persisted immediately (debounced).

A sound also carries a `gain_db` trim (`-12..=+12`, default `0`), set from the context
menu's **Set gain…** dialog (slider + numeric field, 0.5 dB steps, nothing applied before
OK). `audio::engine::effective_gain(volume, gain_db)` is the only place the two are
multiplied and `engine::clamp_gain_db` the only range clamp; `Playback::play` /
`set_volume` / `set_gain` hand the engine that one linear gain, so a live trim change and a
live slider move share one path. A boosted sound may exceed unity inside the mixer — the
master stage clamps the mix.

### Loop

Enable in Settings → Playback and choose the file there; the same page routes the loop
to Monitor and/or Cable. Play/Stop and volume live on the board loop card. It survives
tab switches, `Stop all`, and tray minimize, and stops on quit or when disabled.

### Settings and version

`About` reads the version from `Cargo.toml` at compile time. Config folder uses
`explorer`. Developer values are constants until replaced.

### Backup, restore and reset

`About → Data → Export configuration` writes `storage::bundle::to_json(cfg,
storage_libraries())` to the chosen file with `io::write_text_atomic`.

Import reads and validates the file, then the confirmation dialog calls
`App::apply_import`: stop all audio and the loop, retire every `Playback` entry, rebuild
the board with `build_board` (the function startup uses), `apply_config` (theme, motion,
output levels/gain, endpoint resolution, loop routing, stop-all, hotkeys, RAM retention)
and save. Nothing is applied before Replace.

`apply_config` is also the reset path: `Settings::reset_group` / `reset_all` change only
the `Settings` value, then the same re-apply runs. Libraries, sounds and audio files are
never touched by either action.

---

## 8. Concurrency and invariants

- **UI thread owns App state.** Playback/engine state is behind mutexes because the
  hotkey/mixer threads touch it.
- **Lock order:** UI → `Playback.state` → `Engine.mixer`; avoid the reverse. The
  encoder/decoder never calls back into the UI.
- **Channels:** loader jobs, playback events, audio events, loop chunks, hotkey/tray
  events, and the shell-failure log channel (`App::explorer_tx`, filled by
  `sounddeck-shell` workers). Channels are drained on the UI thread.
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
entry and `Activity log` keeps the last 200. Every entry goes through one
`record_status` helper, which refreshes the newest entry when the message repeats
instead of stacking copies (SPEC UI-6) and drops the oldest at the cap. Sources:

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

- 218 unit tests cover storage recovery/round-trip, reset-group coverage, bundle
  validation, resampling (chunk invariance), loop streaming (wrap, stop, file release,
  errors), mixer loop gain/wrap/routing, `effective_gain`/`clamp_gain_db`, volume
  curve/clamps, hotkey matching and the blocked-chord table, and theme-file parsing.
- Tests use temporary directories named with the process/time; no user data.
- `--diag` enumerates devices, opens Monitor/Cable, decodes a file, plays it, and
  prints WASAPI events.
- `../scripts/measure.ps1` runs the SPEC §8.1/8.2 pass from the `--stats` log (idle,
  hidden, mixer wakeups, working set). It refuses to start while any SoundDeck process
  is alive, and the hidden phase needs `minimize_to_tray` enabled.
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
| `src/playback/mod.rs` | Sound registry, preload jobs, streaming handoff, events |
| `src/playback/policy.rs` | RAM vs stream decision, caps, LRU admission, process valve |
| `src/playback/lock.rs` | Read lock that denies delete/rename while a sound is configured |
| `src/hotkeys.rs` | Hotkey parse/register, stop-all binding, message window, tray, focus |
| `src/stats.rs` | `--stats` counters and the periodic log line |
| `src/icon.rs` | PNG decode/resize for window and tray icons |
| `src/audio/reader.rs` | One Symphonia reader: open, decode block, resample, rewind |
| `src/audio/decode.rs` | Collect the reader into RAM (preload only), early length cap |
| `src/audio/resample.rs` | Stateful resampler (chunk-invariant) |
| `src/audio/stream.rs` | Loop and one-shot decoder threads, bounded chunk channel |
| `src/audio/engine.rs` | Mixer, buffer and stream voices, loop, rings, gains, routes |
| `src/audio/wasapi.rs` | Device enumeration, shared-mode render threads |
| `src/storage/mod.rs` | Settings/libraries schemas, normalize, reset groups, load/save |
| `src/storage/io.rs` | Atomic writes, BOM, rolling backup, recovery, tmp sweep |
| `src/storage/migrate.rs` | Legacy `sounddeck.json` import |
| `src/storage/bundle.rs` | One-file config bundle: export, parse, version/stamp validation |
| `src/ui/theme.rs` | Palette registry, radii, fonts, style, helpers |
| `src/ui/theme_file.rs` | Markdown theme parser/template/fallback |
| `src/ui/theme_defaults.rs` | Embedded palettes |
| `src/ui/theme_color.rs` | Hex parsing and blending |
| `src/ui/anim.rs` | Ease/tween/hover/press, OS animation flag |
| `src/ui/volume.rs` | Log curve, percentage, shared volume bar |
| `build.rs` | Windows icon/version resource |
| `assets/` | Icon and Satoshi font files |
| `../scripts/package.ps1` | Release packaging: tests, release build, zip, checksum |
| `../scripts/measure.ps1` | SPEC §8.1/8.2 measurement pass from the `--stats` log |

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
