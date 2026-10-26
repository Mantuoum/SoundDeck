# SoundDeck — Looping Track

Status: storage, per-file themes, motion, and the UI polish batch are shipped. The
looping track shipped in `cd22482`; `context.md` tracks the live state.

## Shipped baseline (reference, do not redo)

These decisions are already in the code. They constrain new work but need no
re-implementation.

**Config**

- Layout:
  ```
  SoundDeck.exe
  config/
    settings.json
    libraries.json
    themes/*.md
  ```
- JSON for machine data, one Markdown file per theme. Audio paths are references,
  stored as given or exe-relative (`model::portable_path`), never copied.
- Atomic writes, one rolling `.bak` per config file, `.tmp` swept at startup, BOM
  tolerated on read and never written.
- Schema clamps: volumes `0..1`, master gain `0..12 dB`. Sounds cap at 20 minutes and
  decode to 48 kHz stereo; the loop is the exception (streamed, no cap).
- Runtime settings: active tab, theme id, layout, device ids/names, volumes, gain,
  link volumes, hotkey RAM preload, `animate_ui`. `window_size` is parsed and ignored;
  the window always opens at 960×540.

**Themes**

- `config/themes/<id>.md`; Graphite is the default and the per-token fallback.
- Tokens: `bg`, `panel`, `panel_deep`, `card`, `card_hover`, `field`, `border`,
  `border_soft`, `text`, `muted`, `faint`, `accent`, `purple`, `amber`, `red`
  (plus optional `accent_soft`).
- Theme reload is manual only. Accent is freely overridable.

**Visual system**

- Radii: `SM=5` (segmented pill, chips), `MD=8` (controls, rows), `LG=12` (cards),
  `XL=14` (modals, popups, capsule search).
- Spacing uses the existing helpers (`sound_surface`, `section_line`, `ghost_button`,
  `accent_button`, `icon_hover`); new UI follows them instead of bespoke painting.
- Motion: 60–90 ms ease-out, no bounce, gated by `animate_ui` AND the Windows flag.

**Verification**

- 21 tests green; release build deployed to `sounddeck\review\SoundDeck.exe`.

## Non-goals

- No database. JSON + atomic writes is the durability model.
- No audio copying or media library management.
- No playback, hotkey or RAM behaviour changes **other than the opt-in looping track**,
  which is streamed, unhooked from the hotkey registry, and exempt from exclusive
  playback.
- No live filesystem watcher for themes.
- No playlists, per-loop routing, or seeking within the loop.

---
## 1. Looping track

Status: **shipped** in `cd22482`. See 1.15 for the as-built notes and the two layout
deviations.

### 1.1 Goal and scope

One background track, looped forever, independent of the sound grid.

In scope:

- One file, picked by the user, referenced in place. Never copied, never renamed.
- Streamed from disk, so any length works (a 2-hour set is fine).
- Play / Stop only. No seek, no progress bar, no hotkey, no per-track volume.
- Hidden until enabled in Settings, so the default sidebar is unchanged.
- Follows Monitor + Cable routing, both slot volumes, and master gain.
- Plays underneath sound effects. Exempt from exclusive playback, `Stop all`, and
  library switching.

Out of scope:

- Playlists, crossfading between tracks, more than one loop slot, shuffle/repeat modes
  other than "repeat one".
- Per-loop device routing or a separate loop fader.
- Resuming at the last position: every Play starts at frame 0.
- Putting the loop into `libraries.json`, search results, the grid, the hotkey registry,
  or the RAM preload set.
- Drag-and-drop onto the loop card (the existing grid drop path is unchanged).

### 1.2 Locked decisions

| Topic | Decision |
| --- | --- |
| Length / memory | **Stream from disk.** No duration cap; constant ~0.5 MB buffer |
| Routing | Follow Monitor + Cable; no extra fader |
| Interaction with sounds | Play underneath, no ducking; both directions exempt |
| Scope | Global: survives tab switches and `Stop all` |
| Launch state | Remember `loop_path`; always start stopped |
| Placement | Sidebar card under the meters; only rendered when enabled |
| Bad file | Keep the path, show the error, offer Locate; never silently clear |
| Start/stop | Instant, no fade (accepted click risk) |

### 1.3 Config

`config/settings.json` gains two fields, in this order after `preload_hotkey_sounds`:

```json
"loop_enabled": false,
"loop_path": null
```

Rules:

- `loop_enabled` is **visibility only**. It does not auto-start playback.
- `loop_path` is stored exactly as picked by the file dialog: absolute, or relative to
  the exe folder. Same rule as `SoundConfig.path` — no canonicalization, no copying.
- Turning the feature off stops playback, hides the card, and **keeps** `loop_path`.
- There is no `loop_volume`. Monitor/Cable slot volumes and master gain apply in the
  mixer, so following is free.
- Normalize on load: `loop_path.trim()`; empty or whitespace-only → `None`. Do not
  clear it because the file is missing, and do not `stat` it at startup (a network path
  must not delay launch).
- `SETTINGS_VERSION` stays at 2. `#[serde(default)]` on `Settings` already covers old
  files; unknown keys are ignored as before.
- Hand-edited `loop_enabled: true` with `loop_path: null` is valid: the card appears
  with "No file selected".
- The settings write path is the existing debounced `mark_dirty()` — no new file, no
  new backup.

### 1.4 Architecture

The loop cannot be a `Voice`: voices are fully decoded `Arc<Vec<f32>>`, are stopped by
exclusive playback, and live in RAM. The loop is streamed and exempt, so it gets its own
channel through the mixer.

```
UI thread
  Play ──► Engine::start_loop(path)
             ├─ LoopStream::spawn ──► decoder thread (symphonia)
             │      status: Idle/Buffering/Playing/Underrun/Error
             │      bounded channel(64 × 1024 frames ≈ 1.36 s, ~0.5 MB)
             └─ Mixer.loop_rx = Some(rx)

mixer thread (5 ms chunks)
  Mixer::render ──► voices + loop chunk ──► master gain ──► out
     └─ rings[Monitor] / rings[Cable] push_scaled(slot volume) ──► WASAPI
```

New module `src/audio/stream.rs`:

```rust
pub enum LoopState {
    Idle,
    Buffering,
    Playing,
    Underrun,
    Error(String),
}

pub struct LoopStream { /* stop flag, join handle, shared status */ }

impl LoopStream {
    /// Spawns the decoder thread and returns immediately with the chunk receiver.
    /// `status` is shared with the mixer so underruns can be reported too.
    pub fn spawn(path: PathBuf, status: Arc<Mutex<LoopState>>) -> (Self, Receiver<Vec<f32>>);
    pub fn state(&self) -> LoopState;
    /// Sets the stop flag and joins. The caller drops the receiver first.
    pub fn stop(self);
}
```

`Engine` additions:

```rust
pub fn start_loop(&self, path: PathBuf) -> Result<()>;
pub fn stop_loop(&self);
pub fn loop_state(&self) -> LoopState;
```

`start_loop` pre-checks that the path exists and is a file so the UI gets an immediate
`Err`. Everything after that is reported through `LoopState` (missing, unsupported,
corrupt, device-independent errors), because probing happens on the decoder thread.
It stops any existing loop first, spawns the decoder, and attaches the receiver to the
mixer immediately. It does not wait for prebuffering: a few milliseconds of silence at
the head is better than blocking the UI thread. The status flips
`Buffering → Playing` when the mixer consumes the first chunk.

The stream handle lives in `Engine` as `Mutex<Option<LoopStream>>`; the receiver, the
current chunk, and a clone of the shared status live inside `Mixer`. That is how the
mixer reports `Underrun` and flips back to `Playing` without a second status channel.

### 1.5 Decoder thread

1. Validate: exists, is a file, readable. Errors map to `Error("File not found: …")`,
   `Error("Cannot open …")`, etc.
2. Probe with a `Hint` from the file extension and `enable_gapless: true`, exactly like
   `decode.rs`. Remember `track_id`; reject `CODEC_TYPE_NULL` / no track.
3. Decode packet by packet, convert each block to interleaved stereo f32 @ 48 kHz, and
   send 1024-frame chunks on the bounded channel. A blocking `send` is the backpressure:
   when the mixer is 1.36 s ahead, the decoder sleeps in `send`.
4. On `UnexpectedEof`: end of pass. On `ResetRequired` (codec parameters changed
   mid-file): recreate the decoder from the new stream parameters and keep going in the
   same pass; if the codec cannot be rebuilt, end the pass and reopen.
5. Wrap: prefer `format.seek(SeekMode::Accurate, SeekTo::Time { time: Time::from(0.0),
   track_id: Some(track_id) })` followed by `decoder.reset()`. Formats that reject the
   seek fall back to reopen + re-probe. If reopening fails, emit
   `Error("File is no longer available")` and exit.
6. Guard against spin: if one full pass produced zero decodable frames, emit
   `Error("No audio samples found")` and exit instead of wrapping forever. Also cap
   consecutive decode errors (e.g. 64) before bailing.
7. Check the stop flag between packets. `Engine::stop_loop` drops the mixer's receiver
   first, so a blocked `send` returns `Err` immediately and the join is bounded to a
   few milliseconds.
8. Send a final `Error("…")` only for fatal conditions. Underruns are reported by the
   mixer, not the decoder.

Conversion lives in a new `src/audio/resample.rs`, extracted from `decode::to_canonical`:

- `StereoResampler { rate, channels, phase, carry }` with
  `push(&mut self, interleaved: &[f32]) -> Vec<f32>`.
- Stateful linear interpolation with one carried input frame, so output is independent
  of chunk boundaries (a hard test requirement).
- 48 kHz stereo input is passed through unchanged (bit-exact).
- Mono duplicates to L/R; more than two channels uses channels 0/1; other layouts are
  clamped the same way as today.
- `decode::to_canonical` becomes a thin one-shot wrapper so existing tests and decoding
  behaviour do not change.
- `MAX_SECONDS = 20 min` stays for sounds only. The loop has no cap.

No new crates: `symphonia`, `crossbeam-channel`, and `anyhow` are already dependencies.

### 1.6 Mixer rules

- `Mixer::render` mixes loop frames **before** `master_gain` is applied, alongside
  voices, so master gain affects the loop. Slot volumes are applied when the mixed
  buffer is pushed into each ring, so Monitor/Cable apply automatically.
- Loop frames are added, never scaled by a sound's volume. There is no per-loop gain.
- `play()` must not touch the loop: exclusive playback only trims `voices`.
- `stop_all()` must not touch the loop. Add a comment saying so, or the next cleanup
  will "fix" it.
- `stop_sound()` is unaffected.
- Partial chunks: keep `loop_chunk` + `loop_pos` in the mixer; refill with `try_recv`
  when exhausted. Never split a chunk across a render call without carrying the
  remainder.
- Underrun: if `try_recv` is empty, output silence for the rest of that 5 ms render and
  keep the loop attached. Set `LoopState::Underrun` through the shared status handle;
  the next chunk flips it back to `Playing`. Do not log per underrun (network files
  would flood the log); log once per play session.
- Stop order: take `loop_rx` and clear `loop_chunk` under the mixer lock (instant
  silence), then join the decoder. Silence must not wait for a decode to finish.
- Meter: loop output counts toward the peak meter, so the bars stay alive while the
  loop plays. This is correct and expected; no special-casing.
- Device changes, reconnects, and `set_routes` do nothing to the loop. The decoder is
  device-agnostic; the rings buffer whatever the mixer produces.
- Quit: stop in `App::on_exit` and again in `Engine::Drop` (idempotent). The decoder must
  not outlive the window, because the release profile uses `panic = "abort"` and a crash
  must not leave a thread holding the file.

### 1.7 UI

**Settings → Playback**, above the RAM row, same `paint_switch` pattern as the other
toggles:

- Label: `Looping track`
- Helper: `Show a loop player under the meters`
- Off → stop and hide. On → show the card; do not start playback.

**Sidebar card**, pinned above the Settings button (outside the scroll area, below
the meters), so it stays visible at 960×540. The pinned block reserves 32 px in compact
mode and 76 px otherwise; the scroll area shrinks to match.

State → UI mapping:

| State | Transport button | Right-hand text | Play enabled |
| --- | --- | --- | --- |
| No path | `Play` | `No loop file` | No |
| Idle (path set) | `Play` | `Ready` | Yes |
| Buffering | `Stop` | `Loading…` | n/a (Stop) |
| Playing | `Stop` | `Playing` | n/a (Stop) |
| Underrun | `Stop` | `Buffering…` | n/a (Stop) |
| Missing | `Play` (disabled); pick button becomes `Locate` | `File missing` | No |
| Error | `Retry` | message (truncated) | Yes, retries |

Full layout (≈66 px, `RADIUS_LG`, `sound_surface` hover, `border_soft`):

```
┌──────────────────────────────────────────────┐
│ [LOOP]  rain_loop.flac                  [⌕]  │  20 px
│ [ ▶ Play ]   Playing                         │  28 px
└──────────────────────────────────────────────┘
```

- Badge: 34×14, `RADIUS_SM`, accent at 16% fill / 70% border, 9 px uppercase `LOOP`.
  It communicates "this repeats", nothing else.
- Filename: `display_name()` of the path, 11.5 px, single line, ellipsis via
  `galley`/`truncate`; tooltip shows the full path.
- Pick button: 16×16 folder/sound icon, `icon_hover`, tooltip `Choose a loop file`.
  In the missing state the same button is the Locate action and its tooltip becomes
  `Locate the loop file`; no second button is added.
- Transport button: 78×24, `RADIUS_MD`, accent when playing, muted otherwise.
  - Stopped: play triangle + `Play`.
  - Playing/Buffering: stop square + `Stop`.
  - Error: `Retry` (re-attempts the same path).
- Right side of row 2: 10.5 px muted status text: `Playing`, `Loading…`,
  `File missing`, or the error message truncated with a tooltip.
- No progress bar, no elapsed time, no volume slider.

Compact layout (`ui.available_height() < 620.0`), one 26 px row:

- 22×22 transport icon button, filename at 11 px, 16×16 pick button. Badge is reduced
  to a 7.5 px `LOOP` caption before the filename; the separate status text is dropped.
  An error or a missing file replaces the filename with the message in `theme::red()`.
- The Monitor/Cable captions above the device selectors collapse while the loop card is
  shown on a short window, so the meters stay in view. They return when the loop card
  is hidden.
- An empty path still shows `No loop file` with Play disabled and the pick button live.

File dialog: reuse the existing `rfd` setup and `SUPPORTED_EXTENSIONS` filter used by
`open_file_dialog` (Audio filter + All files), so double extensions and odd paths behave
identically. The dialog is blocking; that matches the current "Load sounds" path.

Behavior:

- Picking the same path is a no-op (no restart, no re-probe).
- Picking a different path while stopped just replaces the path.
- Picking a different path while playing stops the current stream and starts the new one
  immediately. Brief silence while buffering is acceptable.
- Toggling the feature off while playing is an instant stop; the path survives.
- No new keyboard shortcut. Do not touch `hotkeys.rs` or the registry, so global hotkeys
  and the tray behave exactly as today.
- Status/log lines: one `Info` on start (`Loop track: <name>`), one `Warn` on failure.
  Do not log every underrun.

### 1.8 Behavior matrix

| Event | Loop result |
| --- | --- |
| Trigger any sound / retrigger | Keeps playing underneath, full level |
| `Stop all` (red button) | Keeps playing |
| Switch library tab | Keeps playing |
| Switch layout grid/list | Keeps playing |
| Change Monitor/Cable volume or link toggle | Follows immediately |
| Change master gain | Follows immediately |
| Change output device | Keeps playing; no restart |
| Minimize to tray | Keeps playing |
| Window close / Alt+F4 / tray Quit | Instant stop, thread joined, then exit |
| App crash | Decoder thread dies with the process (`panic = "abort"`) |
| Loop file deleted while playing | Decoder error → stop, card shows the error, path kept |
| Loop file deleted while stopped | Card shows `File missing`, Play disabled, Locate offered |
| Loop file restored at the same path | Card re-enables Play (existence polled ≤ once per 2 s) |
| Pick a different loop file while playing | Old stream stops, new one starts immediately |
| Feature toggled off | Instant stop, card hidden, path kept |
| Second instance launched | Focuses the first; loop unchanged |

### 1.9 Edge cases

1. **No path.** Card visible with `No loop file`; Play disabled; pick button live.
2. **Empty file / no audio track / unsupported codec.** Decoder reports a specific
   error; Play becomes `Retry`; nothing loops, nothing spins.
3. **Zero decodable frames in a full pass.** Fatal, not an infinite wrap (1.5.6).
4. **File shorter than one chunk (e.g. 20 ms).** Works; the pass wraps within a chunk.
   Expect a click at the seam because no crossfade exists.
5. **Very long file (hours).** Allowed. Decoder memory stays at the channel depth.
6. **Mono / 5.1 / 7.1 source.** Mono duplicates; >2 channels uses L/R only; matches
   existing sound decoding.
7. **Odd sample rates (8 kHz … 192 kHz).** Stateful linear resampling; chunk-invariant
   test required.
8. **Sample-rate change mid-file.** `ResetRequired` rebuilds the decoder for the new
   parameters; if that fails, end the pass and reopen; if the reopen fails, stop with an
   error.
9. **Unseekable/odd container.** Fall back to reopening the file; if reopen fails, stop
   with an error.
10. **Network / slow USB file.** Underruns become silence; the loop resumes when data
    arrives. One status warning per session, never a dialog.
11. **File locked by another process.** Open may succeed for reading; if it does not,
    show `Cannot open …` and keep the path.
12. **Permission denied / path too long.** Same error path; the tooltip shows the path.
13. **File renamed while playing.** The open handle keeps reading (Windows); the next
    Play reports missing. No watcher.
14. **Path points to a directory.** Rejected before spawning the thread.
15. **Double-click Play / rapid toggle.** `start_loop` stops the previous stream first;
    the UI also keeps a `loop_busy` guard for the duration of the spawn call so two
    decoder threads cannot exist.
16. **Play with no output devices.** Allowed; the mixer renders silently and the state
    still says `Playing`. The existing device warnings are the source of truth.
17. **Both slots on the same device.** Existing engine rule applies (second route
    dropped); the loop is unaffected.
18. **Volume at 0 or device muted.** State stays `Playing`; the meter reads low.
19. **Master gain at +12 dB.** Loop can clip together with sounds; the mixer clamps to
    ±1.0 as it does today.
20. **Loop file is also a sound in a library.** Allowed; independent playback, but the
    sound will interrupt other sounds, never the loop.
21. **Loop playing while a sound is preloaded into RAM.** Unrelated; RAM pill does not
    count the loop.
22. **Settings hand-edited to `loop_enabled: true` with a bogus path.** Card appears and
    shows the error; the app still starts.
23. **Settings hand-edited with an empty string path.** Normalized to `None`.
24. **Config written by an older build (no loop keys).** Defaults apply: hidden,
    no path.
25. **Read-only exe folder.** Loop still plays (nothing is written); the settings change
    is lost as usual, with the existing warning.
26. **App quit while buffering.** `stop_loop` joins promptly; no hang on exit.
27. **Two copies from different exe paths.** Only one instance per path; if both run,
    they fight over hotkeys as today. The loop is per-process.
28. **Instant start/stop clicks.** Accepted. A 2 ms declick ramp is the fallback if it
    is audible in practice (1.14).

### 1.10 Failure and recovery

| Failure | Detection | Result |
| --- | --- | --- |
| Missing at Play | Pre-spawn check | No spawn; card shows missing; Play disabled |
| Probe/decode fails on start | Decoder `Error` | `Retry`; path kept |
| Decode fails mid-file | Decoder `Error` | Loop stops; card shows message; status warning |
| File disappears mid-play | Read error → `Error` | Stop, keep path; card offers Locate |
| Channel underrun | Mixer `try_recv` empty | Silence for that render; auto-recover |
| Decoder thread panic | Thread finished while status is Buffering/Playing | `LoopState::Error("Loop track stopped")`; Play/Retry recovers |
| Start called twice | `Engine::start_loop` | Previous stream stopped first |
| Quit during decode | Drop / tray path | Stop flag + dropped `rx`; join bounded |

The loop never takes the app down, never blocks the UI thread waiting for audio, and
never leaves a path silently cleared.

### 1.11 Tests

Worth automating:

- `resample`: chunk invariance (same 44.1 kHz input pushed as whole / 1-frame / 4096-frame
  blocks produces identical output), mono duplication, >2-channel mapping, 48 kHz
  passthrough bit-exact, existing `to_canonical` tests still pass.
- `stream`: generate a 0.25 s stereo WAV in the temp folder; spawn; pull ≥ 2× the file
  length through the channel with a timeout; assert the pass yields ≈12000 frames
  (±1), is not silent after the wrap, and that `stop()` closes the channel and joins
  quickly. Then assert the file handle is free (rename it).
- `stream`: zero-frame file (valid WAV header, no samples) reports an error instead of
  looping forever.
- `storage`: settings roundtrip includes `loop_enabled` / `loop_path`; empty/whitespace
  path normalizes to `None`; old JSON without the keys loads as disabled; `loop_enabled`
  alone is preserved.
- `engine`: attach a synthetic receiver, render one buffer, assert the loop is audible
  under a voice and that master gain scales both; empty receiver yields silence without
  dropping the voice; `stop_loop` silences immediately; `stop_all` leaves the loop
  attached.

Manual QA (review build):

1. Enable in Settings → card appears, Play disabled until a file is picked.
2. Pick a 3-minute MP3 → Play → loops at the seam; Stop is instant; Play restarts at
   the beginning.
3. Play a sound over the loop: both audible; the sound fades out the previous sound,
   not the loop.
4. `Stop all`, switch tabs, switch layouts: loop keeps playing.
5. Change Monitor and Cable volume/gain: loop follows.
6. Rename the file while playing: card shows the error; Restore → Locate/Play works.
7. Enable → disable while playing: instant stop, card hidden; re-enable restores path.
8. Minimize, hotkey a sound, restore: loop never glitched.
9. Quit from the tray while playing: process exits within a second, file handle released.
10. 44.1 kHz mono and 96 kHz stereo files: correct pitch and speed; no drift over
    several loops.

### 1.12 Docs (same commit)

README additions:

- Features: `Looping track` bullet, "streams from disk, any length".
- `Config files`: `loop_enabled`, `loop_path`, "path is a reference, never copied".
- New `Looping track` section: enable in Settings → Playback, pick a file, Play/Stop,
  instant start/stop, follows Monitor/Cable, exempt from `Stop all` and library switches,
  missing-file behavior with Locate.
- `Audio notes`: sounds still cap at 20 minutes and preload to RAM; the loop streams and
  does not appear in the RAM preload count.
- `Hotkey limitations`: state explicitly that the loop has no global hotkey.
- Update the settings snippet with the two new keys.

### 1.13 Implementation order

1. Extract `audio/resample.rs`; keep `to_canonical` delegating to it. Tests pass with no
   behavior change.
2. Add `audio/stream.rs` (probe, decode, resample, bounded channel, wrap, stop, status).
3. Add the Engine loop API and mixer integration (`loop_rx`, chunk carry, underrun,
   `stop_loop`, Drop).
4. Add the two settings fields + normalize + tests.
5. Add the Settings toggle, sidebar card, file dialog, status polling, busy guard, and
   compact layout.
6. Run `cargo test`, release build, deploy to `review\SoundDeck.exe`, manual QA (1.11).
7. Commit all of it plus README. Then the separate docs commit for `plan.md` /
   `context.md` and the remaining README gaps.

### 1.14 Follow-ups (not this commit)

- 2 ms declick ramp on start/stop if instant cuts are audible.
- Optional global hotkey for loop toggle (would need a reserved, non-conflicting slot).
- Optional elapsed-time readout in the card (needs a position counter; the stream
  currently has none).
- Optional track normalization ("even loudness") — a streaming gain stage, not a fader.

### 1.15 As built

- `src/audio/resample.rs` is a stateful, absolute-position resampler. Chunk invariance
  is unit-tested (whole block vs 1-frame vs 1024-frame pushes, bit-identical).
- `src/audio/stream.rs` owns the decoder thread, 64×1024-frame bounded channel, wrap
  seek/reopen, zero-frame guard, and prompt stop (receiver dropped before join).
- `Engine` owns the stream handle plus a shared `LoopState`; the mixer drains the
  channel and reports `Playing`/`Underrun`. `start_loop`/`stop_loop`/`loop_state` are
  the public surface; `Engine::Drop` and `App::on_exit` stop the thread.
- Storage: `loop_enabled`, `loop_path` (trimmed on load), defaults off/empty; migration
  from `sounddeck.json` writes them as off/empty.
- UI: Settings → Playback checkbox, pinned sidebar card, picker, missing/error states,
  in-app logging, 2-second existence poll, tooltips.
- Deviations from the draft:
  1. The card is pinned above Settings instead of living inside the scroll area —
     at 960×540 the scrolled version was entirely below the fold.
  2. The Monitor/Cable captions collapse in compact mode while the card is shown, to
     keep the meters visible; they return when the loop card is hidden.
- Tests: 33 total (5 resampler, 3 stream, 3 engine loop, 1 storage normalize plus the
  existing 21). Release build deployed to the review folder after the commit.

