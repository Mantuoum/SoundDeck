# SoundDeck — Audio Latency, Consistent Volumes, Full-window Settings

Status: **implemented.** Audio/latency in `220fb11`, loop + volume in `eadb63e`,
volume consistency in `cb8c9af`, Settings in `30ecb6d`, developer guide in this commit.
The sections below are the specification that was executed; 6.5 records the deltas.

Four pieces of work, in order:

1. Make the level meters match what is actually heard, and cut the oversized output
   queue that delays every start/stop.
2. Simplify the loop's streaming path and give it its own volume.
3. Unify the volume maths and behaviour without changing how anything looks.
4. Replace the settings modal with a full-window, tabbed Settings view.

Developer documentation is a fifth deliverable, written **after** these refactors so it
describes the final code. The far-future native UI rewrite stays parked at the end.

## Locked decisions

| Topic | Decision |
| --- | --- |
| Loop playback | Keep streaming; no RAM preload, no length cap |
| Decode buffer | Shrink the decode-ahead channel to ~0.34 s |
| Output queue | Shrink each mixer ring from 2.0 s to **0.15 s** |
| Metering | Measure peaks where WASAPI consumes samples, not where the mixer produces |
| Loop UI states | `Idle`, `Playing`, `Error` only; no Buffering, no Underrun |
| Loop volume | New `loop_volume` (`0..1`, default `1.0`), independent of Monitor/Cable |
| Volume consistency | Same maths, clamping, formatting and interaction rules; **no visual redesign** |
| Settings surface | Full-window view inside the existing window (not a modal, not a new window) |
| Settings nav | Left rail: Playback, Audio, Appearance, About |
| Window size | Still opens at 960×540; settings uses all of it; resize/maximize works |
| Settings save | Immediate, no Save/Cancel; rail footer says changes save automatically |
| Settings back | “Back to soundboard” in the rail, plus `Esc` and `Ctrl+,` |
| Title bar | Library tabs hidden; muted “Settings” breadcrumb while open |
| Status bar | Unchanged; Stop all / Activity log stay available |
| Settings tab state | Session-only; not persisted |
| Decoding | Keep Symphonia; do not bundle FFmpeg |
| Scope | Latency/meter fix, loop simplification + volume, volume consistency, settings, docs |

## Goals

- Stop the meter from showing audio before it is audible.
- Make loop and sound start/stop feel immediate (target ≤250 ms total).
- Remove the loop's “Buffering”/“Underrun” churn without adding 300 MB of RAM.
- Give the loop its own level without touching Monitor/Cable.
- Make every volume control share one curve, one clamp, one formatter, one behaviour.
- Give settings enough width and structure to stop scrolling for basic options.
- Document the final architecture for a beginner Rust developer, in one file.

## Non-goals

- No loop seeking, playlists, crossfade, or multiple loop slots.
- No loop hotkey.
- No new preferences beyond the loop volume.
- No separate native Settings window, no exclusive fullscreen, no settings search.
- No FFmpeg dependency. Symphonia already decodes the advertised formats, and decoding
  is not the latency bottleneck. Bundling FFmpeg would add ~100 MB and licensing work
  without fixing the output queue.
- No other storage schema change beyond `loop_volume`.
- No UI redesign here; the native rewrite remains a separate, parked track.

---

## 1. Audio latency and metering

### 1.1 What is actually happening

The app prepares audio in 5 ms chunks and pushes them into a per-output **ring**: a
fixed-size queue between the mixer and the sound card. The card drains that queue at
real-time speed.

- The ring currently holds **2 seconds**.
- The mixer runs ahead and fills it.
- The peak meter is computed at **mix time**, in `mixer_loop`, before the samples reach
  the ring. The UI therefore lights up while the audio is still queued.
- Start and stop are delayed by the same queue: stopping only prevents *future* mixes;
  up to two seconds of already-mixed loop/sound audio is waiting.

Disk is not involved once a file is decoded. A short WAV/MP3 decodes in milliseconds.
The delay is queued audio, not fetching.

### 1.2 The two fixes

**A. Measure at the consumer side**

- Add a peak field to `Ring`, updated inside `pop_into` (called by the WASAPI render
  thread) with an atomic max.
- `Engine::take_levels()` reads and clears each ring's peak instead of reading
  `Mixer::meter`.
- Slot gain is already applied when mixing into the ring, so the measured values match
  the signal sent to each device.
- Remove `Mixer::meter` and the `peak_of` call in `mixer_loop`.
- Result: bars match heard audio within the device buffer (≈10–30 ms wired, more over
  Bluetooth). `AudioEvent` and peak-hold behaviour stay unchanged.

**B. Shrink the ring**

- `Ring::new(2.0)` → `Ring::new(0.15)` per output.
- 150 ms is 30 chunks of 5 ms. The mixer catches up bursts of up to 4 chunks, so this
  is enough headroom for normal Windows scheduling.
- Fallback: 0.25 s if manual QA hears crackles. Never return to 2.0 s.
- This also reduces the lead on sound-card progress bars and the loop card.

### 1.3 Expected latency

| Stage | Today | After |
| --- | --- | --- |
| Click → engine | <1 ms | <1 ms |
| Loop first audio | probe/first chunk + up to 2 s queue | first chunk + ≤150 ms queue |
| Sound trigger | up to 2 s queue | ≤150 ms queue |
| Stop → silence | up to 2 s queued tail | ≤150 ms queued tail |
| Meter vs ears | ahead by the queued amount | matches within the device buffer |

Wired shared-mode Windows audio adds roughly 10–30 ms; Bluetooth adds more and cannot
be fixed in the app. Shared mode is required to let Discord/OBS/games use the same
device, so ASIO-style 5 ms latency is out of scope.

### 1.4 Why not load the loop into RAM

A 15-minute stereo loop at 48 kHz f32 is ~330 MB. Streaming the same file costs a few
MB and one decoder thread, and normal media apps do exactly that. The perceived delay
is not caused by streaming; it is the output queue plus the “Buffering” state. Fix those
and streaming is fast enough, with no length cap.

### 1.5 Loop streaming simplification

Keep `audio/stream.rs`; make it smaller and quieter:

- **Decode-ahead channel:** `CHANNEL_CHUNKS` 64 → **16** chunks (≈0.34 s, ~130 KB).
  Enough to absorb decode jitter; less RAM and less lookahead.
- **States:** replace `LoopState { Idle, Buffering, Playing, Underrun, Error }` with
  `{ Idle, Playing, Error(String) }`.
  - `start_loop` sets `Playing` immediately after spawning. The first chunk usually
    arrives within milliseconds; the UI flips to Stop at once.
  - No `Buffering` flash, no `Underrun` state, no mixer-side state transitions.
  - A starved channel simply outputs silence for that render and recovers. Optionally
    log one warning per play session; do not change the UI state.
- **Mixer:** keep `loop_rx`, `loop_chunk`, `loop_pos`; remove `loop_status`.
  `Engine::loop_state()` reads the engine-held status and treats a finished decoder
  thread with a non-error state as `Error("Loop track stopped")`.
- **Start:** pre-check the path, stop any existing loop, spawn the decoder, attach the
  receiver. No prebuffer wait.
- **Stop:** drop the receiver, clear the chunk under the mixer lock, join the decoder,
  set `Idle`. The queued output tail is now ≤150 ms.
- **Wrap:** keep the current seek/reopen logic; the 0.34 s channel hides the seek.
- **No 15-minute cap**, no `loopbank`, no RAM copy of the file.
- Keep `resample.rs`, `decode.rs`, and their tests.

Result: fewer moving parts than today, any-length support, <1 MB of buffers, one
decoder thread only while playing.

### 1.6 Loop volume

- `Settings.loop_volume: f32` (linear `0..1`, default `1.0`), clamped in `normalize()`.
  `#[serde(default)]` covers old files; no settings version bump.
- `Mixer.loop_gain` and `Engine::set_loop_volume(f32)`. Loop samples are scaled by
  `loop_gain` before the master gain and the per-slot Monitor/Cable gains.
- `App` applies `cfg.loop_volume` right after `Engine::new` and on every change.
- UI:
  - Settings → Playback: “Volume” row with a logarithmic slider + percentage.
  - Board loop card: one volume row, matching per-sound bars; full card grows 66 → 84,
    compact 26 → 44. Pinned reserve grows by 18 px. At 960×540 the compact sidebar still
    fits (device captions already collapse when the card is shown).
- Total loop level = `loop_volume × slot volume × master gain`. Link volumes does not
  touch `loop_volume`.
- Volume 0 means playing but silent; the state stays `Playing`.
- Changes apply on the next 5 ms render. No smoothing in this pass; matches sound
  volume behaviour.

### 1.7 Loop edge cases

1. **File longer than any size.** Streams fine; no cap.
2. **Unsupported/corrupt file.** Decoder reports `Error`; path kept; picker active.
3. **Missing file.** Play disabled; picker becomes Locate.
4. **File disappears mid-play.** Decoder error → stop; card shows the message.
5. **Slow network/USB read.** Brief silence, state stays Playing, recovers.
6. **Rapid Play/Stop.** Start stops the previous stream first; stop joins after dropping
   the receiver; no thread leak.
7. **Volume 0 or mute.** Silent but Playing; meter flat.
8. **Seam on MP3/AAC.** Decoder padding can leave a small gap. WAV/FLAC loop cleanly.
9. **Quit while playing.** `on_exit`/`Engine::Drop` stop and join the decoder.
10. **Device switch/reconnect.** No loop action; the ring is device-agnostic.
11. **Master +12 dB plus sounds.** Mixer clamps to ±1.0 as today.
12. **Two copies.** Per-process; unchanged.

### 1.8 Audio tests

- `Ring`: consumer peak reflects popped samples and clears on read; slot gain applies
  before measurement.
- `mixer`: loop gain scales the loop; loop is unaffected by `play()` and `stop_all()`;
  stop silences immediately; wrap after N frames.
- `stream`: wrap, prompt stop, file-handle release, empty file error (existing tests,
  adjusted for the new states).
- `storage`: `loop_volume` roundtrip, default, clamp.
- Manual: measure click-to-sound for a sound and the loop; confirm the meter no longer
  leads audibly; loop 20-minute file; volume drag while playing; device switch.

---

## 2. Volume consistency pass

No visual redesign. Keep the slim bars on cards and the existing sliders on outputs.
Make the logic, constants and behaviour live in one place.

### 2.1 Current inconsistencies

- `volume_to_position` / `position_to_volume` are app-local helpers; the loop volume
  would add a third caller.
- The card bar claims “scroll to adjust” but only handles click/drag.
- Percentage formatting and clamping are repeated.
- Device sliders and card bars store linear gains but map positions identically.
- Master gain uses a separate 0…+12 dB range (correct, but its reset/tooltip should
  follow the same conventions).

### 2.2 New module `src/ui/volume.rs`

```rust
pub const RANGE_DB: f32 = 60.0;

pub fn to_position(linear: f32) -> f32;
pub fn from_position(position: f32) -> f32;
pub fn percent(linear: f32) -> String;     // " 80%" style, consistent width
pub fn bar(ui, rect, id, linear, compact, on_change); // existing look
```

- All volume callers use these functions: sound cards, Monitor, Cable, loop card,
  Settings rows.
- `bar` gains wheel support (2 % per notch) and keeps click/drag. Tooltips and
  percentages use `percent`.
- Clamp 0..1 at the source; `from_position` clamps too.
- Remove the duplicated helpers from `app.rs`.
- `device_volume` keeps its egui slider but uses the shared mapping and formatter.
- Master gain stays a dB control with right-click reset; factor its label/readout row
  so Settings and the sidebar share it.
- No new stored types; `loop_volume`, sound volume and slot volumes stay `f32`.

### 2.3 Behaviour rules (all volume controls)

- Drag/click sets the value; wheel adjusts; release persists.
- Slider position is logarithmic; stored value is linear.
- 0 is silent, 1.0 is 100 %; no negative values.
- Percent readout reflects slider position, not raw gain.
- Changing Monitor/Cable honours the link switch; widgets do not implement linking.
- Loop volume is independent; no link, no master-gain interaction beyond multiplying.

### 2.4 Tests

- Round-trip: `from_position(to_position(v)) ≈ v` for 0, 0.001, 0.1, 0.5, 0.99, 1.
- Endpoints: 0 → silent, 1 → 0 dB.
- Clamp: negatives/NaN handled safely.
- Formatter produces the same width used by the UI.
- Manual: drag each volume; wheel over a card bar; linked Monitor/Cable; loop 0/50/100;
  master gain reset.

---

## 3. Full-window Settings

### 3.1 Information architecture

| Tab | Contains |
| --- | --- |
| **Playback** | Loop (enable, file, transport, volume, errors), hotkey RAM preload |
| **Audio** | Monitor/Cable device + volume, link switch, master gain + reset, refresh |
| **Appearance** | Theme picker, reload themes, Animate UI |
| **About** | App version, portable note, Open config folder, developer block |

About is pinned to the bottom of the rail; the three real settings tabs sit above it.

### 3.2 Layout

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ▣ SoundDeck    Settings                                      –   □   ✕       │ 38
├──────────────────────┬───────────────────────────────────────────────────────┤
│ ← Back to soundboard │  Playback                                             │
│                      │  Playback and memory.                                 │
│ ▸ Playback           │                                                       │
│   Audio              │  Background loop                                      │
│   Appearance         │  ┌─────────────────────────────────────────────────┐  │
│                      │  │ Looping track                            [ On ] │  │
│                      │  │ Play one file under the sound effects.          │  │
│                      │  │                                                 │  │
│  ─────────────────   │  │ rain_loop.flac        Playing    [Stop] [⌕]    │  │
│   About              │  │ Volume  ▬▬▬▬▬▬▬▬▬●──────  80%                   │  │
│                      │  │ Streamed from disk · starts stopped on launch   │  │
│                      │  └─────────────────────────────────────────────────┘  │
│                      │                                                       │
│ Changes save         │  Hotkey sounds                                        │
│ automatically        │  ┌─────────────────────────────────────────────────┐  │
│ v0.1.0               │  │ Load hotkey sounds into RAM              [ On ] │  │
│                      │  │ 3 of 5 hotkey sounds loaded.                    │  │
│                      │  └─────────────────────────────────────────────────┘  │
├──────────────────────┴───────────────────────────────────────────────────────┤
│ ● Ready                                              Stop all   Activity log │ 34
└──────────────────────────────────────────────────────────────────────────────┘
```

At 960×540 the body is 468 px: rail 200, content 760, 28 px margins → 704 px usable.

**Rail**

- `panel_deep` background, 200 px wide, no scrollbar.
- Back row 30 px, then 34 px nav items with 16 px line icons and 12.5 px labels.
- Active: accent 14% fill, accent text, 1 px accent stroke.
- Hover: text at 3% fill, 60 ms.
- Divider above About; footer “Changes save automatically” + `v0.1.0`.

**Content**

- `bg` background; fixed header with tab title (22 medium) + subtitle (11.5 muted).
- Group title 13.5 medium; card `card` fill, `border_soft`, `RADIUS_LG`, 16 px padding.
- Rows: ≥54 px, 12 px vertical padding, 1 px dividers.
- Label/description takes the remaining width; control column 240 px.
- Below 620 px usable width, the control stacks under the label.
- Content column max 780 px, left-aligned; maximized windows keep the same column.
- Header fixed; groups scroll with a per-tab offset.

**Chrome**

- Title bar keeps logo/name, replaces library tabs with a muted “Settings” breadcrumb,
  keeps window controls and drag/maximize.
- Status bar unchanged.
- Board sidebar and its divider are not drawn while Settings is open.

### 3.3 Visual language

- `RADIUS_MD` for controls/rail items, `RADIUS_LG` for cards, no new radii.
- Shared `paint_switch` (40×22) for every boolean; no checkboxes in Settings.
- Dropdowns and sliders use the 240 px control column.
- Buttons 28 px: `ghost_button` for secondary, accent-tinted for transport.
- One accent per tab; no gradients, shadows or new colours.
- Motion: 90 ms fade on open and on tab change; 60 ms nav hover; no slide. Respect
  `animate_ui` and the Windows flag.

### 3.4 Behaviour

- Entry: sidebar Settings button, `Ctrl+,`, `--settings`.
- Exit: rail back, `Esc`, `Ctrl+,`. `Esc` closes an open popup first
  (`memory.any_popup_open()`), then leaves Settings.
- `Ctrl+Q` still quits; board-only shortcuts (`Ctrl+O/F/L/Z`, Delete, Space, F2) are
  disabled while Settings is open.
- Window close quits; minimize hides to tray.
- On open/close, `memory.stop_text_input()` and clear `focus_search` so the hidden
  search field never eats keystrokes.
- Opening Settings does not stop sounds or the loop; meters and hotkeys stay live.
- Drop-to-add is skipped while Settings is open (no overlay, no file add).
- Tab order follows rail → content; Space toggles a focused switch, Enter opens a
  dropdown.
- Window size is still not persisted.

### 3.5 Tabs

#### 3.5.1 Playback

Subtitle: “Playback and memory.”

**Background loop** (group)

- Row: “Looping track”, description “Play one file on repeat underneath the sound
  effects. It follows Monitor/Cable and keeps playing through Stop all.”, switch.
- Enabled sub-panel:
  - File name + status chip (`Ready`, `Playing`, `File missing`, error).
  - Full path tooltip; `Choose file` (or `Locate` when missing); `Play`/`Stop` button.
  - Volume row: logarithmic slider + percentage, description
    “Independent of Monitor and Cable.”
  - Meta line: “Streamed from disk · starts stopped on launch”.
- Turning the switch off stops and hides the sidebar card, keeps `loop_path`.
- Reuses `pick_loop_file`, `toggle_loop`, and the existing loop state.

**Sidebar loop card** (board, not Settings)

- Gains one volume row using the shared `volume_bar`:
  - Full: speaker icon + bar + percentage.
  - Compact: bar only, same drag behaviour.
- Heights grow 18 px: 66 → 84 full, 26 → 44 compact; the pinned reserve grows with them.
  At 960×540 the compact sidebar still fits.
- Status shows `Playing`/`Ready`/error only; no buffering text.

**Hotkey sounds** (group)

- “Load hotkey sounds into RAM” + switch; disabled with “Assign a hotkey to a sound
  first.” when none exists.
- Summary: “N of M hotkey sounds loaded.” using `playback.loaded_count()`.

#### 3.5.2 Audio

Subtitle: “Outputs, routing and levels.”

- **Monitor**: device dropdown + volume slider.
- **Cable**: device dropdown + volume slider.
- **Routing and gain**:
  - “Link Monitor and Cable” switch; enabling copies Monitor to Cable.
  - “Master gain” 0…+12 dB slider, dB readout, visible Reset; right-click resets.
  - “Refresh audio devices” button.
- No devices: amber note “No output devices. Plug one in and refresh.”; dropdowns show
  `None (silent)`.
- Sidebar and Settings use the same helpers.

#### 3.5.3 Appearance

Subtitle: “Theme and motion.”

- “Theme” dropdown (parsed themes, display-name order).
- “Reload themes” + “Markdown palettes in `config/themes/`.”
- Warnings go to the status bar; a deleted active theme falls back to Graphite.
- “Animate UI” switch + Windows animation note.
- Theme changes apply instantly, including to Settings itself.

#### 3.5.4 About

Subtitle: “Version, developer and config.”

- `SoundDeck v0.1.0`, portable note.
- “Config folder” + Open; short `settings.json · libraries.json · themes/` note.
- Developer name/role/blurb/link from the existing constants; placeholders remain.
- No clickable link in this pass.

### 3.6 Code shape

```text
src/ui/volume.rs            curve + shared bar/behaviour
src/audio/stream.rs         simplified streaming decoder (16 chunks, 3 states)
src/audio/engine.rs         loop_rx/chunk/pos + loop_gain; consumer-side meter
src/app.rs
  enum SettingsTab { Playback, Audio, Appearance, About }
  fields: settings_tab, settings_tab_fade
  update(): title bar → status bar → settings screen OR board
  settings_screen / settings_rail / settings_header
  settings_page_playback / audio / appearance / about
  helpers: settings_group, settings_row, settings_switch, settings_nav_item
```

Shared/extracted helpers:

- `master_gain_control(ui, width)` from the sidebar; used by Audio.
- `link_toggle(ui) -> bool`; used by Audio.
- `theme_picker(ctx, ui, width)`; used by Appearance.
- `device_selector` / `device_volume` as-is inside a 240 px column.
- `ui::volume::bar` for cards; shared curve in Settings rows.
- Loop actions: `pick_loop_file`, `toggle_loop`; loop volume setter.

Removals:

- `settings_modal`, backdrop, window, `max_height` scroll.
- Settings checkboxes; switches instead.
- `LoopState::Buffering` / `Underrun`, mixer `loop_status`, `Mixer::meter`.
- “Refresh audio devices” from Playback (moves to Audio).

`modals()` keeps capture, rename and log overlays.

### 3.7 Settings edge cases

1. **860×500 minimum.** Rail 200, content 660 → 604 usable; rows stack; groups scroll.
2. **960×540.** Playback fits at 704 px; Audio/About may scroll a few rows.
3. **Maximized.** Content capped at 780 px, controls stay 240 px.
4. **No devices.** Warning + `None (silent)`; refresh available.
5. **Same device on both slots.** Engine warning unchanged.
6. **Linked volumes.** Either slider mirrors the other; link-on copies Monitor.
7. **Master gain extremes.** Clamped; Reset/right-click to `0.0`.
8. **Loop missing/error.** Play disabled; picker = Locate; message in the row.
9. **Loop playing while Settings is open.** Shows Stop; returning shows the live card.
10. **Loop disabled while playing.** Stops, hides card, keeps path.
11. **No hotkeys.** RAM switch disabled with the hint.
12. **Theme reload removes the active theme.** Graphite fallback + status warning.
13. **Popup + Esc.** First closes the popup, second exits Settings.
14. **Search focused before opening.** Focus cleared; no hidden filtering.
15. **Sound playing before opening.** Continues; audio edits apply live.
16. **Global hotkey while Settings is open.** Still plays sounds.
17. **Minimize/close from Settings.** Tray restore returns to the same tab/scroll.
18. **Resize while open.** Reflows; no persisted size.
19. **Save failure.** Status warning; no fake “saved” state.
20. **`--settings` launch.** Opens Playback directly.
21. **Drag/drop over Settings.** Ignored.
22. **Long names.** Truncated with tooltips.
23. **Keyboard-only.** Tab/Shift+Tab, Space, Enter, Esc.
24. **Session tab memory.** Last tab per run; restart starts Playback.
25. **Device unplugged while open.** Engine logs; dropdown shows stale name until
    Refresh (same as today).
26. **High DPI.** Logical points scale rail/controls.

### 3.8 Settings tests

- Existing tests stay green; audio/loop tests from sections 1–2.
- Manual screenshots at 960×540, 1280×800, 860×500, maximized.
- Walk every row; verify board state (search/selection/library/scroll) survives.
- Esc with a dropdown open; focus clearing; drop suppression.
- Loop volume from Playback and the sidebar card stays in sync.
- Theme changes affect Settings itself.

---

## 4. Developer documentation

Deliverable: **`sounddeck/ARCHITECTURE.md`**, one file, written after sections 1–3 ship.

Audience: a beginner Rust developer who knows the language basics but not this app.
Explain the app, not Rust.

### 4.1 Outline

1. What SoundDeck is, and the process/thread map.
2. Repository map: every source file and what it owns.
3. Startup and shutdown sequence (`main`, single instance, storage load, themes,
   engine, devices, hotkeys/tray, eframe).
4. Storage: schemas, normalization, atomic writes, backups/recovery, migration,
   portable paths.
5. Audio pipeline: WASAPI shared mode, 48 kHz stereo, mixer, rings, voices, exclusive
   playback, the loop stream, RAM vs disk sounds, decode/resample, meters, volume.
6. Hotkeys and tray: registration rules, active-tab bindings, messages, focus.
7. UI layer: state, update loop, panels, grid/list, cards/rows, actions, modals,
   Settings, search, drag/reorder, themes, motion.
8. Feature walkthroughs: libraries, adding/relinking/removing sounds, volumes, meter,
   loop, hotkey RAM, search, settings, activity log, diagnostics.
9. Concurrency rules and invariants: lock order, channels, generations, no UI blocking.
10. Error handling: missing files, device errors, save failures, warnings.
11. Testing: unit tests, temporary dirs, manual QA, diagnostics.
12. “Where to change what”: add a setting, theme token, hotkey, sound action, widget,
    audio effect.
13. Glossary: WASAPI, ring, voice, exclusive playback, RAM preload, dBFS, portable path,
    gapless, shared mode.

### 4.2 Rules

- README stays the user manual; ARCHITECTURE is linked from a short “Developers” line.
- Tables and short call-flow diagrams, not line-by-line commentary.
- Update it in the same commit as any schema, audio, hotkey or settings change.
- No Rust tutorial content; define app concepts only.

---

## 5. Rollout

Five commits, in order.

### 5.1 `fix(audio): meter follows heard audio and shorter output queue`

1. Consumer-side peak in `Ring`; `take_levels` reads it; delete mixer metering.
2. Ring 2.0 → 0.15 s; manual crackle check.
3. Tests for ring peak and gain scaling.

### 5.2 `refactor(audio): simplify loop streaming and add loop volume`

1. `loop_volume` storage + clamp.
2. `LoopState` → Idle/Playing/Error; remove mixer status and underrun paths.
3. Channel 64 → 16 chunks; optimistic Playing on start.
4. `Mixer.loop_gain`, `Engine::set_loop_volume`.
5. Loop volume row on the card (44/84 px heights, reserve).
6. Tests: wrap, gain, exclusivity, prompt stop; README loop notes.

### 5.3 `refactor(ui): one volume curve and consistent volume behaviour`

1. `ui/volume.rs` with curve/percent/bar; wheel support.
2. Replace app-local helpers and duplicated formatting.
3. Shared master-gain row helper.
4. Tests for round-trip/clamp.

### 5.4 `feat(ui): full-window tabbed settings`

1. `SettingsTab`, state, conditional panel split.
2. Rail + group/row/switch helpers.
3. Port Playback/Audio/Appearance/About; extract sidebar helpers.
4. Remove modal/backdrop; titlebar breadcrumb, focus clearing, Esc popup guard, drop
   suppression.
5. Tests, release build, manual QA, README settings section.

### 5.5 `docs: developer architecture guide`

- Write `ARCHITECTURE.md` against the shipped code; link from README; update
  `context.md` and this plan.

No config version bump. Hotkeys, tray, routing and sound playback rules are unchanged.
Deploy to `review\SoundDeck.exe` after each UI-visible commit.

---

## 6. Open choices / follow-ups

- **Ring size.** 0.15 s is the target; 0.25 s if crackles appear. Truly instant stop
  would require flushing device rings when nothing else is playing.
- **Declick.** Instant loop start/stop can click at a non-zero crossing; a 2 ms ramp is
  the follow-up if audible.
- **Consumer-side progress.** Sound-card progress bars still use mix position and lead
  by ≤150 ms after the ring change; move them too only if it looks wrong.
- **New preferences later:** Window & tray tab (close-to-tray vs quit, start minimized,
  remember size), loop hotkey.
- **Settings search / persisted tab** only if the tab count grows.
- **Developer block** still needs real name/role/link/blurb.
- **Native UI rewrite** remains parked (section 7).

### 6.5 As built

- Output ring is 0.15 s and metering is consumer-side (`Ring::take_peak`); the
  mixer-side meter is gone.
- Loop stream keeps 16 chunks and three states; start is optimistic; errors surface
  from the decoder.
- `loop_volume` is stored in settings and applied in the mixer; the loop card grew to
  44/84 px and Settings has a matching row.
- `ui/volume.rs` owns the curve/bar; the old `volume_rects` scroll plumbing is gone.
- Settings is a 200 px rail plus a 780 px-capped content column; About is pinned in
  the rail; version is `0.2.0`, read from Cargo.
- `sounddeck/ARCHITECTURE.md` is the developer guide; README stays the user manual.
- Tests: 38.

---

## 7. Far-future: native UI rewrite (not scheduled)

Status: **parked.** No work in this plan. Revisit only after the current feature set is
stable and the numbers below justify it.

### 7.1 Why consider it

egui is immediate-mode: it rebuilds the frame and currently asks for a repaint every
120 ms even when idle (33 ms while the meter moves). A retained-mode native UI can
sleep until an input, timer, or audio event and paint only what changed.

Goals, in order:

1. Same layout, spacing, radii, colours, copy and interactions — a rendering/event
   swap, not a redesign.
2. Lower idle CPU and memory: no constant frame rebuild, no steady repaint.
3. Optional: drop UI animations or leave them default-off.

### 7.2 Decision gate (measure before rewriting)

Release build, review config, 960×540 and maximized:

| Scenario | Record |
| --- | --- |
| Board idle, no audio | CPU %, working set, repaint rate |
| Board with a sound playing | CPU %, working set |
| Settings open, idle | CPU %, working set |
| Minimized to tray | CPU %, working set |

Rough targets: **<0.5% CPU** and zero repaints when idle, **<100 MB** UI overhead
(excluding decoded sounds). If egui is already close, stop here.

Cheap egui wins to try first:

- Repaint only on input or active audio; no periodic tick when hidden or silent.
- Stop meter repaints at 33 ms when no device is active.
- Default `Animate UI` off, or remove the animation layer.
- Cache galleys and static chrome; reduce shapes per frame.

### 7.3 Candidate frameworks

| Option | Pros | Cons |
| --- | --- | --- |
| **Win32 + Direct2D/DirectWrite** | Truly native, tiny runtime, event-driven, no GPU dependency, total control | Every widget, text layout, IME, DPI, accessibility, drag/drop and tray glue is manual |
| **Slint** | Rust-native, retained/declarative, event-driven, software and GPU renderers, good Windows support | Theme tokens and custom-painted widgets need porting; younger ecosystem |
| **iced** | Rust-native, retained-ish Elm architecture | Still repaints for meters/animations; wgpu/GPU memory; custom widget/accessibility work |
| **Qt 6** | Mature widgets, accessibility, high-DPI | Heavy runtime, licensing/binding friction |
| **Tauri / Electron / WebView** | Fast iteration | Rejected: webview memory/CPU, the opposite of the goal |

Recommendation if the gate is passed: prototype the board shell + one Settings tab in
both **Slint** and **Win32 + Direct2D**, then pick on font/IME/DPI/tray effort and
measured idle cost. Do not start with the full app.

### 7.4 What must not change

- `audio/`, `storage/`, `hotkeys.rs`, `model.rs` and the config schema are
  framework-agnostic and stay.
- Theme tokens, `RADIUS_*`, spacing, fonts, copy, empty states, card/row layouts and
  window chrome must match. Screenshots become the parity test.
- Portable folder, single instance, tray, global hotkeys, custom resize, status bar,
  activity log, modals, settings tabs keep working.
- Windows integration (WASAPI, `RegisterHotKey`, tray icon, `PrintWindow` QA) remains.

### 7.5 Migration sketch

1. Extract UI state and actions from `app.rs` into a framework-agnostic layer
   (`UiState`, `Action`); useful even without a rewrite.
2. Build a shell prototype: custom title bar, window controls, tray, hotkeys, theme
   tokens, font loading, status bar.
3. Port the board (tabs, grid/list, cards/rows, search, drag/reorder, volumes, meters).
4. Port Settings and the smaller modals.
5. Run old and new builds side by side for parity and performance; ship only when
   screenshots, interaction checklist, idle CPU/RAM and DPI tests pass.

### 7.6 Risks

- Multi-week effort with a real regression surface; the current UI already works.
- Accessibility/IME and font rendering differ per framework and need explicit testing.
- Tray, hotkeys, single-instance and custom chrome wiring must be rebuilt.
- A rewrite does **not** reduce decoded-audio RAM. It only addresses UI overhead.
- Dropping animations is independent and cheap; do that first if motion is unwanted.
