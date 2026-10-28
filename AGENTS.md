# AGENTS.md — working in the SoundDeck codebase

Read this file before changing anything in `sounddeck/src`. It explains how the app
works, the conventions of this codebase, and the guardrails that keep changes clean.

Companion documents:

| File | Role | Rule |
| --- | --- | --- |
| `SPEC.md` | Behavioural contract (MUST/SHOULD) | Source of truth for *behaviour*. Settled decisions live here — never re-litigate them. |
| `sounddeck/ARCHITECTURE.md` | Implementation map | Update it when threads, storage, or the frame loop change. |
| `plan.md` | Current iteration decision log | The work list. Do not invent scope beyond it without asking. |
| `sounddeck/README.md` | User manual | User-facing behaviour only. |

---

## 0. Maintaining this file

**AGENTS.md must stay true.** A stale AGENTS.md is worse than none — it misleads every
future session.

1. **Update in the same change.** If a change alters architecture, threading, data
   flow, module boundaries, or a convention, update the affected section of this file
   in the same commit. No separate "docs later" pass.
2. **Section triggers:**

   | If you changed… | Update… |
   | --- | --- |
   | Threads, sample flow, engine/playback/storage behaviour | §2 (How the app works) |
   | Module responsibilities or dependencies | §3 (Module boundaries) |
   | Error handling, naming, UI/storage/audio patterns | §4 (Conventions) |
   | "Where to change what" steps | §5 (Feature playbooks) |
   | A rule this file should have prevented you from breaking | §6 (Guardrails) |
   | Product behaviour | `SPEC.md` first, then §7 if it is a settled decision |

3. **Keep it lean.** This file carries rules and mental models, not history. Delete
   text that no longer describes reality rather than appending corrections. One-line
   entries in the update log are enough.
4. **Update log** (newest last): `2026-10-28 — created from codebase review; Part 1
   (how it works), boundaries, conventions, guardrails.`

---

## 1. Repository layout

```
SoundBoard/
├── SPEC.md, plan.md, AGENTS.md, README.md, context.md   ← docs (root)
├── scripts/package.ps1                                   ← release packaging
└── sounddeck/
    ├── Cargo.toml, build.rs          ← deps; Windows icon/version resource
    ├── assets/                       ← icon, bundled Satoshi fonts
    ├── ARCHITECTURE.md               ← developer guide (keep current!)
    └── src/
        ├── main.rs          boot: single instance, window, --diag/--stats
        ├── app.rs           ALL UI state + actions (largest file, ~5.6k lines)
        ├── model.rs         paths, extensions, formatting
        ├── hotkeys.rs       keyboard hook, hidden window, tray
        ├── stats.rs         --stats counters
        ├── icon.rs          PNG → window/tray icon
        ├── audio/           engine (mixer), wasapi (devices), reader, decode,
        │                    resample, stream
        ├── playback/        mod (registry), policy (RAM-vs-disk), lock (file lock)
        ├── storage/         mod (schemas), io (atomic writes), migrate (legacy import)
        └── ui/              theme*.rs, volume.rs, anim.rs (paint-side only)
```

---

## 2. How the app works

SoundDeck is a portable Windows soundboard: no installer, everything beside the `.exe`.
It plays each triggered sound to **two audio outputs simultaneously** ("Monitor" =
local speakers/headphones, "Cable" = a virtual cable feeding stream/voice chat) and
triggers via **global hotkeys that work while the app is minimized**.

### 2.0 Rust concepts encountered (short primer)

| Rust | Rough equivalent | Note |
| --- | --- | --- |
| `Arc<T>` | shared ref-counted pointer | `clone` bumps a counter; freed when last clone drops. Cross-thread sharing. |
| `Mutex<T>` | `threading.Lock` tied to the value | `.lock()` returns a guard; auto-releases on drop. Panics **poison** it (later locks return `Err`). |
| `RwLock<T>` | readers-writer lock | many readers or one writer. |
| `AtomicBool/AtomicU32` | lock-free scalars | flags/counters with memory orderings. |
| `Result<T,E>` / `?` | try/except | `?` = early-return the error. |
| `Option<T>` | `T \| None` | no nulls. |
| `serde` | dataclasses + `json` | struct ↔ JSON via derive. |
| `unsafe { }` | ctypes / C extension | only at Win32/WASAPI boundaries. |
| channel (`Sender`/`Receiver`) | `queue.Queue` | crossbeam FIFOs between threads. |

### 2.1 Big picture

```
GLOBAL KEYBOARD (hook thread) ──┐
CLICK in UI (app.rs) ─► Action ─┤
                                ▼
                     Playback::play(sound id)
                      │                    │
            samples in RAM?              no │
                 yes ▼                     ▼
          Engine::play (instant)   policy: preload? ─yes→ loader thread → RAM → play
                                          │
                                        no ▼
                                  stream thread decodes-as-it-plays
                      ▼
          Mixer thread: mix voices + loop, gains, clamp   (5 ms chunks)
                 ┌──────┴──────┐
          Ring[Monitor]  Ring[Cable]      ← 0.15 s queue each
                 ▼             ▼
          WASAPI thread   WASAPI thread   ← resample to device format
                 ▼             ▼
           headphones      virtual cable
```

Everything internal is **48 kHz stereo interleaved `f32`** — the canonical format.
Files convert once at decode; devices convert once at output. SFX go to *both* outputs
(a buffer copy in the mixer); the loop track routes per-output independently.

### 2.2 Threads

| Thread | Started by | Lives | Does |
| --- | --- | --- | --- |
| UI (eframe) | OS | always | draws frames, owns all `App` state |
| `sounddeck-mixer` | `Engine::new` | always | renders 5 ms chunks into the two rings |
| `sounddeck-loader` | `Playback::new` | always | decodes sounds into the RAM cache |
| `sounddeck-loop` | `LoopStream::spawn` | while loop plays | decodes loop file into chunks |
| `sounddeck-stream` | `stream::spawn_once` | per streamed sound | decodes one-shot into chunks |
| `sounddeck-render-0/1` | `wasapi::spawn_render` | per active device | ring → device buffer |
| `sounddeck-hotkeys` | `HotkeyService::start` | always | hidden window, keyboard hook, tray |
| `sounddeck-devices` | `wasapi::list_devices` | one call | COM device enumeration |

### 2.3 Boot (`main.rs`)

1. Panic hook writes `crash.log` beside the exe before abort (`panic = "abort"`).
2. `timeBeginPeriod(1)` (steady 1 ms timers for the mixer); process priority lowered —
   mixer/render threads raise themselves to above-normal.
3. `--stats` → counter thread to `%TEMP%\sounddeck-stats.log`; `--diag <file>` → audio
   smoke test and exit; `--settings` → open on Settings.
4. **Single instance**: named mutex derived from the exe path; second launch focuses
   the first window and exits. One instance per exe path (two copies fight over hotkeys).
5. `storage::load(app_dir())` (see §2.4) → undecorated 960×540 window (min 860×500,
   size deliberately not persisted) → `App::new` → `eframe::run_native`.

`App::new`: unpack config → install theme (seeds `config/themes/*.md` from embedded
palettes on first run) → parse stop-all hotkey → build `sounds`/`libraries` + per-sound
`search_key` → create `Engine` + `Playback`, register sounds → enumerate devices
(Monitor = default output; Cable = first "cable"/"virtual"-named device) → start
hotkey/tray service → record JSON fingerprints (so the first save rewrites nothing).

### 2.4 Storage (`storage/`)

Two versioned JSON files in `config/`: `settings.json` (theme, layout, device ids +
names, volumes, master gain dB, link flag, loop settings, toggles, stop-all hotkey) and
`libraries.json` (libraries = tabs; sounds nested with id, name, path, hotkey, volume,
`ram`). `ram: null` inherits the global preload switch. Paths inside the app folder are
stored **relative** (portable); outside, absolute. Audio files are referenced, never
copied (SPEC FR-5.3.14: the app never creates/renames/deletes audio files).

- **Atomic write** (`io.rs`): write `.tmp` → rotate current → `.bak` → rename `.tmp`
  in (copy-retry when Windows holds a lock).
- **Recovery order**: main → `.tmp` (promote) → `.bak` (copy back) → defaults. A
  corrupt main file is preserved as `.corrupt` (FR-5.11.3).
- **Normalize** on every load: clamp volumes 0..1 / gain 0..12 dB, unique non-zero ids,
  fill empty names, bump `version`.
- **Migration** (`migrate.rs`): legacy `sounddeck.json` imported once; `settings.json`
  written last as the "migration finished" marker so a crash retries next launch;
  legacy renamed `sounddeck.v1.json.bak`.
- **Save lifecycle**: every mutation calls `mark_dirty()`; `tick` debounces saves
  (700 ms); `persist()` diffs **JSON fingerprints** and writes only changed files;
  quit forces a final write.

### 2.5 Playback registry & RAM policy (`playback/`)

`Playable` per sound: path, volume, `samples: Option<Arc<Vec<f32>>>` (resident),
`loading`/`pending_play`, `error`/`missing`, `FileLock`, `refused`, `duration`,
`generation`, `retain`, `last_used` (LRU).

**`play(id)` trigger algorithm** (UI *or* hotkey thread):
1. Samples resident? → `engine.play` instantly.
2. File gone? → `Failed{missing}` event (UI offers Locate).
3. Already decoding? → `pending_play = true`.
4. `retain` on? → queue decode job to the single loader thread; play when it lands.
5. Else **stream from disk** via `stream::spawn_once` (bounded 8×1024-frame channel,
   ~130 KB in flight regardless of file length; blocking `send` gives back-pressure).

**Loader thread** (serialized jobs): snapshot (abort if `generation` changed) →
`policy::route()` → Stream (mark `refused` with reason) or Preload (decode unlocked →
`evict_to_fit` LRU, never evicting a playing sound → admit or mark `BudgetFull`).
Events `Loaded/Failed/Refused/Warning` are polled by the UI.

**Policy caps** (`policy.rs`): ≤ 30 s per sound, ≤ 32 MiB and ≤ 16 sounds resident,
process commit ≤ 125 MiB (fail **closed** if the query fails). Only sounds with a
hotkey **and** in the active library are RAM-eligible (FR-5.3.5); `sync_active_library`
re-derives this on every tab switch.

**File lock** (`lock.rs`): each configured file is held open read-only with delete
sharing denied — Windows refuses rename/delete while it is on the board (FR-5.3.15).

### 2.6 Audio pipeline (`audio/`)

- **Decode** (`reader.rs`): Symphonia probes the container (extension hint), decodes
  packets; up to 64 consecutive bad packets tolerated; mid-file rate/channel changes
  rebuild the resampler. `decode.rs::decode_file` = "collect the reader", preload only,
  rejects > 20 min and empty files.
- **Resample** (`resample.rs`): `StereoResampler` linear interpolation; mono duplicated
  to L=R; absolute input-frame counting makes output **chunk-invariant** (tested).
  Any file → 48 kHz stereo here; 48 kHz → device rate happens in the WASAPI thread.
- **Streaming** (`stream.rs`): decoder thread → bounded channel of 1024-frame chunks
  (8 chunks ≈ 170 ms look-ahead; raised to 16 only on measured underruns, FR-5.3.13).
  `LoopStream` seeks back to 0 at EOF forever; state `Idle/Playing/Error`.
- **Mixer** (`engine.rs`): one thread renders 5 ms chunks (240 frames), paced to wall
  clock (max 4-chunk catch-up), parked when idle (FR-5.4.5). Per chunk: mix voices
  (2.5 ms fade-in / 5 ms fade-out, FR-5.4.2) into Monitor → **copy Monitor → Cable**
  (this is how "same sound on both devices" works) → add loop track per routing flags
  (consumed even when both off, so position is kept) → master gain + clamp ±1.0 →
  push into each `Ring` with per-output volume.
- **Exclusive playback** (FR-5.4.1, **settled decision**): `Engine::play` fades out
  every current voice before pushing the new one. Sounds never overlap. The loop track
  lives outside the voice list and is untouched by play/stop-all.
- **Ring**: 0.15 s (14,400 samples) per output, mixer→device thread; drop-oldest on
  overflow, silence-padding on underrun; records consumer-side peaks for the meter.
- **WASAPI** (`wasapi.rs`): shared mode, event-driven per-device render threads; pulls
  ring → resample → channel map → hardware buffer. Two routes may not share a device
  id (one wins + warning, FR-5.5.2). Device failures are reported, the slot is
  restartable without an app restart (FR-5.5.7).

### 2.7 Hotkeys & tray (`hotkeys.rs`)

A background thread owns a hidden window + a **low-level keyboard hook**
(`WH_KEYBOARD_LL`) — not `RegisterHotKey` — which is why hotkeys work while minimized.

**Hook algorithm** (`keyboard_proc`): on key-down, if the key is not already tracked as
held, track it, read modifiers live (`GetAsyncKeyState` for Ctrl/Alt/Shift/Win), match
stop-all chord or first binding → `PostMessageW` PLAY/STOP to the hidden window (the
actual `Playback::play` runs there, keeping the hook fast). Key-up untracks the key
(auto-repeat guard: at most one trigger per physical press, FR-5.7.3).
**Always `CallNextHookEx`** — pass-through is **settled behaviour** (FR-5.7.2): the
focused app receives every triggering keystroke. Never "fix" this.

Hotkeys are strings (`"Ctrl+Shift+S"`, `"F8"`) parsed via `Hotkey::parse` against
hand-maintained key tables. Only the active library's hotkeys are bound; switching tabs
rebinds. The hook is best-effort against elevated windows/AV (FR-5.7.8 — documented,
not hidden). Tray: double-click show/hide, right-click menu (Show/Hide, Stop all,
Quit), tooltip updated every 3 s.

### 2.8 UI (`app.rs` + `ui/`)

**egui is immediate mode**: every frame rebuilds the whole UI from `App` state and
throws it away. Cross-frame state (drag, modals, search) lives in `App` fields. Painting
functions **collect `Action` values** (play/remove/rename/move/drag…) which
`handle_actions` applies after painting — mutations are centralized and the borrow
checker stays happy. Singleton chrome (sidebar, settings) mutates directly by design.

**Frame lifecycle** (`update`): detect Settings transition → debounce search
(`search` → `filter_needle`, 120 ms) → `poll_events` (playback/engine/hotkey channels)
→ close/minimize handling → `global_shortcuts` (typing-guarded) → edge-resize
hit-testing → `tick` (loop state, save debounce, meters, frame pacing) → paint title
bar + (board | settings) → chrome, drop overlay, drag ghost, modals. When hidden
(tray): `hidden_tick` only, zero painting — the window is *minimized*, never
`Visible(false)` (the latter freezes event draining and burns a core; known trap).

Frame pacing: 33 ms moving, 100 ms settings+audio, 1 s idle (the idle-CPU guarantee,
NFR-8.1.x).

**Board**: sidebar (Monitor/Cable device + volume sliders + link, optional master gain
0..+12 dB right-click-reset, peak meter, stop-all card, loop card, Settings button),
toolbar (search, RAM pill), grid/list of the active library, cards/rows (name, hotkey
pill, RAM chip, missing state, log-curve volume bar, progress line, context menu).
Delete removes with one-slot undo (`Ctrl+Z`).

**Modals** (transparent backdrop, `modal_header` + red-hover close): hotkey capture
(reads raw keyboard events, Esc cancels), rename, activity log (last 200 entries).

**Settings** (immediate save, no Apply): Playback (loop + hotkey RAM), Appearance
(theme + motion), Window (minimize-to-tray, gain slider visibility), About.

**Themes** (`ui/theme*`): Markdown `config/themes/<id>.md`, `| token | hex |` table;
invalid/missing token falls back to Graphite; 5 embedded palettes seed first run;
runtime `RwLock<Palette>` → egui style. Radii constants `SM/MD/LG/XL`. Fonts: bundled
Satoshi via `theme::ui_font`/`medium`; `theme::truncate` for ellipsis.

**Volume** (`ui/volume.rs`): stored linear 0..1, slider position logarithmic over
−60…0 dBFS (one curve everywhere, UI-4 — never add a second). **Animation**
(`ui/anim.rs`): 60–90 ms tweens, gated on the user setting AND the OS animation flag.

---

## 3. Module boundaries (keep the app modular)

The codebase is deliberately layered so a broken part breaks **only its own layer**.
Preserve these dependency directions; arrows mean "may use".

```
app.rs ──► playback/ ──► audio/          app.rs ──► ui/ (paint only)
   │            │                            app.rs ──► storage/, hotkeys(), model
   │            └──► storage/ (schemas only, no I/O side effects in policy)
   └──► audio/ (Engine handle only)
hotkeys.rs ──► playback (play/stop_all calls only) + model
audio/*  ──► (nothing above it; no UI, no storage, no app)
ui/*     ──► egui + theme internals only; NEVER engine/playback/storage state
storage/* ─► serde + io only; NEVER audio/ui
```

Rules:

1. **Dependencies point downward only.** `audio/*` must never reference `ui/*`,
   `app`, `storage`, or `playback`. `ui/*` must never touch the engine or registry —
   it receives data and returns interactions. If you feel the urge to import `app` from
   a leaf module, the design is wrong; introduce a channel/event or move the logic up.
2. **`app.rs` is the wiring layer, not a dumping ground.** New *behaviour* belongs to
   the domain module (playback/audio/storage/hotkeys); `app.rs` gets at most a call site
   and UI. New *widgets* go to `ui/` (or a new `ui/` submodule if one file would
   exceed ~500 lines).
3. **One decision, one place.** RAM-vs-disk is only `policy.rs`; "is this file missing"
   is only `Playback::info`; the volume curve is only `ui/volume.rs`. See §6.
4. **Cross-thread talk uses channels** (`Job`, `PlaybackEvent`, `AudioEvent`,
   `ServiceEvent`, loop chunks) drained on the UI thread. Do not add shared mutable
   state without discussing the lock order (§4.2).
5. **A module's `//!` header states its responsibility** (NFR-8.5.3). If the header no
   longer matches the code, one of them must change in the same commit.

This is also why parallel work is safe when it touches disjoint modules (see
`plan.md` execution notes) and unsafe when two work items touch `app.rs`.

---

## 4. Conventions

### 4.1 Errors

- `anyhow::Result` at boundaries; plain `Option` for internal "absent" cases. Prefer
  `?` over nested `match`.
- **No `unwrap()`/`expect()` outside tests** (NFR-8.3.3: user data must never panic the
  app). No `panic!` as a control-flow path in production code.
- Workers return errors through their event channels (FR-5.1.6) — never panic for an
  expected failure (missing file, decode error, device gone).
- No silent `let _ =` on anything user-visible. Where the user cares (decode, save,
  device, spawn), log or surface it. `let _ =` is acceptable only for best-effort
  cleanup (e.g. a tray remove during teardown).
- Lock poisoning: handle it with the graceful idiom (`if let Ok(...)`, `into_inner`,
  or `unwrap_or_else` to a default) — pick the helper, don't re-invent per call site.

### 4.2 Concurrency

- **Lock order: UI → `Playback.state` → `Engine.mixer`.** Never acquire in reverse.
  Never call engine methods while holding the playback state lock (drop the guard
  first) — today's code mostly complies; keep it that way.
- Do not block the UI thread on decode, joins, device work, or file dialogs' internals
  (NFR-8.3.1). Slow work goes to a worker thread.
- The mixer thread must never do disk I/O or lock anything it doesn't own for the
  chunk it is rendering.
- Atomic counters use `Relaxed` unless ordering with payload data is needed.
- Generations (`Playable.generation`) invalidate stale worker results — use this
  pattern instead of trying to cancel threads.

### 4.3 Naming and types

- `Slot` is `Monitor`/`Cable`; use `slot.index()`/`Slot::BOTH`-style iteration — no
  bare `0..2` loops over device arrays.
- Overloaded names to avoid: `set_master` is per-output (call it output/slot gain);
  `hex` exists in two directions in theme code (`to_hex`/`from_u32`); `cfg` is
  `storage::Settings`.
- egui widget ids: `ui.id().with((tag, entity_id))` with lowercase string tags
  (`"cardvol"`, `"rowvol"`). Never `enum as i32` in ids.
- Function names state side effects (`ensure_loaded`, `mark_missing`, `save_now`).

### 4.4 UI patterns

- Cards/rows collect `Action`s; painting never mutates state (except singleton chrome —
  that is deliberate, per ARCHITECTURE.md §6).
- Reuse before writing: `settings_row`/`settings_switch`/`settings_group`,
  `hotkey_pill`, `modal_header`/`close_button`, `accent_button`/`ghost_button`/
  `tiny_button`, `press_overlay` + `anim::press`, `volume::bar`, `theme::truncate`.
  If a widget appears twice, it becomes a shared helper on the third thought.
- Colors come from theme tokens (FR-5.10.5). No raw `Color32::from_rgb` outside
  `ui/theme*` (except the documented white on the active layout segment).
- Use radii constants `SM/MD/LG/XL`; layout sizes become named consts at the top of
  the file, not inline magic numbers.
- Percentages and volumes render through `ui/volume.rs` only.
- Modals use `egui::Modal` + transparent backdrop (UI-1). Status messages are terse,
  never stacked duplicates (UI-6).

### 4.5 Storage

- New settings field: `storage::Settings` + `Default` + `normalize()` clamp +
  `#[serde(default)]`, then the Settings control, then the consumer. Bump `version`
  only for breaking shape changes. Add a storage test when the value is clamped or
  transformed.
- All persistence goes through `io::write_json_atomic`. Never write JSON by hand.
- Fingerprints (`last_settings_json`/`last_libraries_json`) prevent gratuitous writes —
  keep them accurate when adding fields.

### 4.6 Audio

- 48 kHz stereo `f32` in, device format out; add formats in `reader.rs`/`resample.rs`,
  not ad hoc.
- Fades on every voice start/stop (2.5 ms / 5 ms) — never hard-cut samples.
- Buffers entering the mixer pass `is_valid_buffer` (extend it rather than trusting
  callers).
- Chunk parity (even sample counts) is an invariant of stream chunks — enforce at the
  producer if you touch `stream.rs`.
- Test WAV helpers are shared test-only utilities; don't copy `write_wav` again.

### 4.7 Testing & release gates

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` must
  pass before any "done" claim. Tests are named `thing_should_behave_when_condition`.
- Pure logic (paths, normalize, migration, volume curve, hotkey parse, ring/resampler)
  MUST stay covered (NFR-8.5.2).
- Manual verification uses `PrintWindow` screenshots — **never steal foreground
  focus**; never leave test instances running (two instances fight over hotkeys).
- Never touch `sounddeck/review/config/` — it is the user's real config and audio.

### 4.8 Environment traps (Windows)

- `app.rs` mixes CRLF and LF — the `edit` tool handles both; .NET string `Replace`
  with LF-only strings does not.
- egui 0.31 `Margin` fields are `i8`, not `f32`.
- PowerShell 5.1 `Set-Content -Encoding utf8` writes a BOM; for JSON use
  `[System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))`.
- `paths` with spaces: quote everything in shell commands.

---

## 5. Feature playbooks (where to change what)

| To add… | Steps |
| --- | --- |
| **A setting** | 1) field + `Default` + `normalize()` in `storage::Settings` 2) control in the right `settings_page_*` 3) apply where it belongs (engine/config) + `mark_dirty()` 4) storage test if clamped 5) SPEC §6.1 + README. |
| **A theme token** | 1) `Palette` field in `ui/theme.rs` 2) default in `theme_defaults.rs` 3) parser arm + template in `theme_file.rs` 4) use `theme::token()`. Consider a table-driven parser (§6) instead of a new match arm. |
| **A sound action** | 1) `Action` variant 2) handler in `handle_actions` 3) trigger in card/row/context menu 4) keep painting side-effect free. |
| **A Settings tab** | 1) `SettingsTab` variant + `title()` + `ALL` 2) rail item (About stays pinned) 3) `settings_page_*` + `settings_content` match. |
| **A volume** | Only `ui/volume.rs` — store linear, display via `to_position`/`percent_text`. Never a second curve. |
| **A file format** | 1) Symphonia feature in `Cargo.toml` 2) `model::SUPPORTED_EXTENSIONS` 3) file-dialog filter 4) test file + decode test. |
| **Audio routing changes** | `Engine` owns devices/routes, `Mixer` owns voices/loop/gains, `Ring` is the only producer→device boundary. Keep blocking work off the mixer thread. |
| **Hotkey behaviour** | Go through the `KeyChord`/conflict helper (after the refactor; today `hotkeys.rs` tables + `app.rs::sync_hotkeys`/`find_conflict`). Capture UI = one shared capture widget. Update SPEC §5.7. |
| **A modal** | Reuse the modal shell + `modal_header` + `close_button`. No new backdrop/frame one-offs. |
| **A tray/menu item** | `hotkeys.rs` `WM_COMMAND` + tray menu build share one `handle_menu_command`; add the id in both places it is matched (or the shared helper). |

---

## 6. Guardrails (bad practices to refuse)

These exist because the codebase review found each of them in practice.

1. **No drift-class code (the big one).** Never keep the same knowledge in two places.
   One key table (not three), one policy function that both decides *and* explains,
   one renderer for cards/rows. Before writing a helper: search for it. Before writing
   a second copy of anything: parameterize the first. *Symptom to watch for: two
   functions whose doc comments say the same thing.*
2. **No new `unwrap`/`expect`/indexing assumptions** in production paths. Lengths of
   arrays (`[T; 2]` meters, slot-indexed stats) are checked or typed (`Slot`), not
   trusted.
3. **No blocking the UI thread.** No decode, join, `probe_seconds`, device open, or
   long dialog work inside `update`/painting/event handlers without an async plan.
4. **No per-frame allocation creep** in paint code: no `String` formatting, `clone()`,
   `HashSet`, or `Vec` building per card per frame when a cached/`&` version exists.
   Immediate mode makes this easy to add accidentally — check `paint` functions.
5. **No magic numbers** in new code. Named consts at file top (layout, timing, limits).
   Theme metrics via `SM/MD/LG/XL`.
6. **No dead state.** No fields written and never read, no `#[allow(dead_code)]`
   parked indefinitely, no ignored `Result` on user-visible operations. If the plan
   says "use or drop", drop it.
7. **No lock-across-call.** Drop guards before calling into another subsystem (§4.2).
   No new global mutable state (`OnceLock`/`static`) without a stated reason.
8. **No behavior changes without a SPEC pass.** Behaviour is a contract
   (SPEC.md). If a fix changes what the user sees/does, update SPEC in the same change.
   But never "fix" a **settled decision** (§7) — those are intentional.
9. **No scope creep in a work unit.** Each `plan.md` item is implemented and verified
   on its own; opportunistic refactors go in the refactor batch, not smuggled into a
   bug fix.
10. **No panic on user data** (paths, names, hotkeys, themes, JSON) — NFR-8.3.3. Corrupt
    input degrades to defaults with the original preserved.
11. **Don't grow `app.rs` sideways.** If a change adds more than ~50 lines of new logic
    to `app.rs`, ask whether it belongs in a domain module or `ui/`.
12. **Keep the docs trio in sync**: code ↔ `ARCHITECTURE.md` ↔ `SPEC.md`, plus this
    file per §0.

---

## 7. Settled decisions (do not re-litigate)

Behavioural decisions are **settled in SPEC.md** — read it before proposing behaviour
changes. The ones agents most often get wrong:

| Decision | Where | Note |
| --- | --- | --- |
| **Pass-through hotkeys** | SPEC FR-5.7.2 + §1 | Triggering a hotkey also types the key into the focused app. Intentional. |
| **Exclusive playback** | SPEC FR-5.4.1 + §1 | A trigger fades out all current voices; sounds never overlap. Intentional. |
| SFX go to both outputs equally | SPEC FR-5.5.5 | Per-sound routing is a *future* item, not a bug. |
| Volumes stored linear, shown log (−60…0 dBFS) | SPEC UI-4 | One curve. |
| Portable, no installer/registry/AppData, no network | SPEC ENV-2/4 | |
| JSON config + Markdown themes, audio never copied | SPEC FR-5.3.14 | |
| Single instance per exe path | SPEC ENV-5 | |
| Window size not persisted | SPEC FR-5.8.1 | |
| RAM state hidden by default (diagnostics, not control) | SPEC FR-5.9.9 | |
| Hidden = minimize + taskbar-DeleteTab, never `Visible(false)` | ARCHITECTURE §6 | The `Visible(false)` path burns a core and freezes tray events. |
| Stream buffer 8 chunks unless measured underruns | SPEC FR-5.3.13 | |

Product scope exclusions: SPEC §10 Non-goals (no macOS/Linux, no recording/effects, no
cloud, no FFmpeg, no installer…).

---

## 8. Working with plans and agents

- `plan.md` is the iteration's work list. Implement items **in order**; parallel work
  is allowed only for items whose files are disjoint (see the plan's execution notes).
  Items touch `app.rs` and `hotkeys.rs` serially.
- When a plan item completes: update `plan.md` status, run the release gates (§4.7),
  and apply §0's update triggers to this file.
- When the iteration ends, `plan.md` is wiped and rewritten from the new state of the
  world (implemented work disappears from the plan; the code and docs carry it).
