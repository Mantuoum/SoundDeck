# AGENTS.md — SoundDeck

| Doc | Role |
| --- | --- |
| `SPEC.md` | Behaviour contract (MUST/SHOULD). Settled decisions live here — never re-litigate. |
| `sounddeck/ARCHITECTURE.md` | Implementation map — update on thread/storage/frame-loop changes. |
| `plan.md` | Current work list (local, gitignored). No scope beyond it without asking. |
| `QA-CHECKLIST.md` | Manual QA worksheet: ordered pass, expected behaviour per item, known-nuances table. Run before a release; record failures inline — findings become the next `plan.md`. |

## 0. Maintaining this file

Update it in the SAME change as the code it describes — a stale AGENTS.md misleads every
future session. Threads/data flow → §2; boundaries → §3; conventions → §4;
where-to-change-what → §5; a rule §6 should have caught → §6; behaviour → SPEC first.
Delete text that stopped being true instead of appending corrections; log entries stay
one line.

Log: `2026-10-28 — created (codebase review); 0.4.0 iteration syncs` · `2026-10-29 — QA
workflow (checklist + test material)`. Details live in git history.

## 1. Repository layout

```
SoundBoard/
├── SPEC.md, plan.md, AGENTS.md, README.md,     ← docs (root)
├── QA-CHECKLIST.md                             ← manual QA worksheet
├── scripts/package.ps1                         ← release packaging
├── scripts/measure.ps1                         ← NFR-8.1/8.2 measurement pass
├── scripts/make-test-material.ps1              ← QA audio/theme/fixture generator
└── sounddeck/
    ├── Cargo.toml, build.rs   assets/   ARCHITECTURE.md  ← deps, icon/fonts, dev guide
    └── src/
        ├── main.rs       boot: single instance, window, --diag/--stats
        ├── app.rs        all UI state + actions (~5.5k lines)
        ├── model.rs      paths, extensions, formatting
        ├── hotkeys.rs    keyboard hook, hidden window, tray
        ├── stats.rs      --stats counters
        ├── icon.rs       PNG → window/tray icon
        ├── audio/        engine (mixer), wasapi (devices), reader, decode, resample, stream
        ├── playback/     mod (registry), policy (RAM-vs-disk), lock (file lock)
        ├── storage/      mod (schemas), io (atomic writes), migrate (legacy import),
        │                 bundle (one-file export/import)
        └── ui/           theme*.rs, volume.rs, anim.rs (paint-side only)
```

## 2. How the app works

SoundDeck is a portable Windows soundboard: no installer, everything beside the `.exe`.
It plays each triggered sound to **two audio outputs simultaneously** ("Monitor" =
local speakers/headphones, "Cable" = a virtual cable feeding stream/voice chat) and
triggers via **global hotkeys that work while the app is minimized**.

### 2.1 Big picture

```
GLOBAL KEYBOARD (hook thread) ──┐
CLICK in UI (app.rs) ─► Action ─┤
                                ▼
                     Playback::play(sound id)
                      │                     │
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
| `sounddeck-probe` | `Playback::spawn_probe` | one shot per unknown-duration sound | reads a streamed file's length off the trigger path |
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
`gain_db`, `ram`). `ram: null` inherits the global preload switch. Paths inside the app folder are
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
  legacy renamed `sounddeck.v1.json.bak` (`.bak.2`, `.3`, … if taken — backups and
  `.corrupt` snapshots never clobber an earlier one).
- **Save lifecycle**: every mutation calls `mark_dirty()`; `tick` debounces saves
  (700 ms); `persist()` diffs **JSON fingerprints** and writes only changed files;
  quit forces a final write. `serialize_settings`/`serialize_libraries` are the only
  serializer: the fingerprint string and the file are the same bytes (`save_*` just
  writes what `serialize_*` produced).
- **Export/import** (`bundle.rs`): one JSON bundle `{app, bundle, settings, libraries}`
  built from those same structs; import validates the stamps, `normalize()`s, and
  `App::apply_import` swaps the board only after the user confirms. Settings export is
  the only writer of a user-chosen file (`io::write_text_atomic`, no rolling `.bak`).
- **Reset** (`Settings::reset_group`/`reset_all`): one field list per settings page;
  the coverage test fails when a new field reaches no group. Never touches libraries.

### 2.5 Playback registry & RAM policy (`playback/`)

`Playable` per sound: path, volume, `gain_db` trim, `samples: Option<Arc<Vec<f32>>>`
(resident), `loading`/`pending_play`, `error`/`missing`, `FileLock`, `refused`,
`duration`, `probe_pending`, `generation`, `retain`, `last_used` (LRU). `duration`
starts `None` for a streamed sound, so the card draws no progress line until a
`sounddeck-probe` worker has read it (FR-5.4.7).

A voice always carries **one linear gain**: `engine::effective_gain(volume, gain_db)` is
the only place the stored volume and the dB trim are multiplied (FR-5.3.17). Trigger,
deferred play, and both live changes (`set_volume`, `set_gain`) read it from the same
snapshot; the engine itself only knows linear gain.

**`play(id)` trigger algorithm** (UI *or* hotkey thread):
1. Samples resident? → `engine.play` instantly.
2. File gone? → `Failed{missing}` event (the UI logs it; relinking is context-menu → Locate).
3. Already decoding? → `pending_play = true`.
4. `retain` on? → queue decode job to the single loader thread; play when it lands.
5. Else **stream from disk** via `stream::spawn_once` (bounded 8×1024-frame channel,
   ~130 KB in flight regardless of file length; blocking `send` gives back-pressure).

**Loader thread** (serialized jobs): snapshot (abort if `generation` changed) →
`policy::route()` → Stream (mark `refused` with reason) or Preload (decode unlocked →
`evict_to_fit` LRU, never evicting a playing sound → admit or mark `BudgetFull`).
Events `Loaded/Failed/Refused/Warning` are polled by the UI.

**Policy caps** (`policy.rs`): ≤ 30 s per sound, ≤ 32 MiB and ≤ 16 sounds resident,
process commit ≤ 125 MiB (fail **closed** if the query fails). Admission decisions and
their explanations come from one `policy::Refusal` type — extend it, never re-derive
the rules. Only sounds with a
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
  restartable without an app restart (FR-5.5.7). `App::apply_routes` only ever passes a
  device from the live enumeration: `route_slot` resolves the stored endpoint and, when
  it is gone, substitutes the Windows default (Cable: a `cable`/`virtual` name first,
  never the Monitor's) and reports it in the log + Settings (`device_note`, FR-5.5.8).
  The stored id/name survive; `refresh_devices`/`retune_slot` refresh a stale name and
  save only when something changed.

### 2.7 Hotkeys & tray (`hotkeys.rs`)

A background thread owns a hidden window + a **low-level keyboard hook**
(`WH_KEYBOARD_LL`) — not `RegisterHotKey` — which is why hotkeys work while minimized.

**Hook algorithm** (`keyboard_proc`): on key-down, if the key is not already tracked as
held, track it, derive modifiers from the tracked held-key set (each event prunes keys
the OS reports as up, so a missed key-up cannot wedge a hotkey), match stop-all chord or
first binding → `PostMessageW` PLAY/STOP to the hidden window (the actual
`Playback::play` runs there, keeping the hook fast). Key-up untracks the key
(auto-repeat guard: at most one trigger per physical press, FR-5.7.3).
**Always `CallNextHookEx`** — pass-through is **settled behaviour** (FR-5.7.2): the
focused app receives every triggering keystroke. Never "fix" this.

Hotkeys are strings (`"Ctrl+Shift+S"`, `"F8"`) parsed via `Hotkey::parse` against the
single `KEYS` const table — parsing, display and egui mapping all read it (never add a
second mapping). Win chords are bindable: egui has no Win modifier flag, so the capture
path reads the physical Win key state, and pass-through means Windows also acts on the
chord (README warns). Duplicate chords are **refused** at capture; conflicts introduced
elsewhere are unbound deterministically (new binding wins; `chord_owner` /
`duplicate_sounds` in `hotkeys.rs` own the rules, FR-5.7.5). Only the active library's
hotkeys are bound; switching tabs rebinds. The hook is best-effort against elevated windows/AV (FR-5.7.8 — documented,
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
pill, RAM chip, missing state, log-curve volume bar, progress line — drawn only once the
total duration is known — context menu). Delete removes with one-slot undo (`Ctrl+Z`).
Deleting a library opens a dialog (`deleting`): move its sounds to a chosen library, or
remove them from the board (files untouched); cancel leaves everything alone.

Per-frame board data (visible indices, ids, playing/progress map, collected `Action`s)
is rebuilt each frame into `App::board_scratch` (clear + refill) so the *content* is
always fresh while the allocations are not. Stable per-sound strings are cached on
`Sound` — `search_key`, `hotkey_label`, `path_label` — and rebuilt together by
`refresh_labels()` (or `refresh_hotkey_label()` when only the binding changed); call it
after any name/path/hotkey change, never re-derive them while painting.

**Modals** (transparent backdrop, `modal_header` + red-hover close): hotkey capture
(reads raw keyboard events, Esc cancels), rename, delete library, import confirmation,
reset-all confirmation, activity log (last 200 entries, a repeated message refreshes the
newest instead of stacking). While any of them is open, `global_shortcuts` stands down.
Outside modals, a focused text field suppresses every board shortcut (quit and Settings
stay live).

**Settings** (immediate save, no Apply): Playback (loop enable/file/level/routing,
hotkey RAM, Show RAM state, stop-all hotkey), Appearance (theme + motion), Window
(minimize-to-tray, gain slider visibility), About (version, config folder, links, Data:
export/import/reset all). The three field pages end with a Reset card
(`Settings::reset_group`); About's Reset all is confirmed.

**Themes** (`ui/theme*`): Markdown `config/themes/<id>.md`, `| token | hex |` table;
invalid/missing token falls back to Graphite; 5 embedded palettes seed first run;
runtime `RwLock<Palette>` → egui style. Radii constants `SM/MD/LG/XL`. Fonts: bundled
Satoshi via `theme::ui_font`/`medium`; `theme::truncate` for ellipsis.

**Volume** (`ui/volume.rs`): stored linear 0..1, slider position logarithmic over
−60…0 dBFS (one curve everywhere, UI-4 — never add a second). **Animation**
(`ui/anim.rs`): 60–90 ms tweens, gated on the user setting AND the OS animation flag.

---

## 3. Module boundaries

Dependencies point downward (arrows = "may use"):

```
app.rs ──► playback/ ──► audio/          app.rs ──► ui/ (paint only)
   │            └──► storage/ (schemas)      app.rs ──► storage/, hotkeys(), model
   └──► audio/ (Engine handle)            hotkeys.rs ──► playback (play/stop only)
audio/* ─► nothing above it              ui/* ─► egui + theme only, never app state
```

- **Downward only.** `audio/*` never references ui/storage/app; `ui/*` never touches
  engine/playback/storage state; `app.rs` is the wiring layer — behaviour goes to the
  domain module, widgets to `ui/`. One deliberate exception: `storage` reads the
  per-sound trim range (`SOUND_GAIN_MIN_DB`/`MAX_DB`) from `audio::engine`, because the
  clamp and the conversion must come from one definition (FR-5.3.17).
- **One decision, one place**: RAM-vs-disk = `policy.rs`; missing-file = `Playback::info`;
  volume curve = `ui/volume.rs`; per-sound level = `engine::effective_gain` (volume ×
  dB trim). Cross-thread talk = channels drained on the UI thread.
- Modules keep truthful `//!` headers. Parallel work only on disjoint modules;
  `app.rs`/`hotkeys.rs` always serially.

## 4. Conventions

- **Errors**: anyhow at boundaries, `Option` internally, `?` over nested match. No
  `unwrap`/`expect`/`panic` outside tests. Workers report failures via event channels.
  No silent `let _ =` on user-visible work. Poison: one graceful helper idiom, not
  per-call-site inventions.
- **Concurrency**: lock order UI → `Playback.state` → `Engine.mixer`; never call the
  engine under the state lock; never block the UI thread (decode/join/probe/dialogs go
  to workers); no disk I/O on the mixer thread; `generation` invalidates stale worker
  results.
- **Naming/ids**: `Slot::index()` iteration (no bare `0..2`); egui ids
  `ui.id().with((tag, id))` (no `enum as i32`); function names state side effects.
- **UI**: cards/rows collect `Action`s (painting doesn't mutate; singleton chrome
  excepted). Reuse `sound_item`, `modal_shell`, `settings_*`, `hotkey_pill`,
  `volume::bar`, `theme::truncate`, `press_overlay`. Colors = theme tokens only. Radii
  SM/MD/LG/XL. Volumes/percentages only via `ui/volume.rs`. Fetch `theme::palette()`
  once per paint; theme locks go through `theme::read_lock`/`write_lock`.
- **Storage**: settings field = struct + `Default` + `normalize()` clamp +
  `#[serde(default)]` + control + consumer. Writes only via `io::write_json_atomic`
  (`write_text_atomic` for a user-chosen export file: same commit, no `.bak`).
  Keep save fingerprints accurate.
- **Audio**: 48 kHz stereo f32 in, device format out; fades on every voice start/stop;
  buffers pass `is_valid_buffer`; stream chunks are whole frames (truncated tail
  dropped); `Ring` drops stray samples; integer scaling symmetric (`scale_to_int`).
- **Gates before "done"**: `cargo fmt --check`, `cargo clippy --all-targets -- -D
  warnings`, `cargo test`; tests named `thing_should_behave_when_condition`; pure logic
  stays covered. Visual checks via PrintWindow — never steal focus, never leave test
  instances running, never touch `sounddeck/review/`.
- **Windows traps**: `app.rs` mixes CRLF/LF (the edit tool handles it); egui 0.31
  `Margin` is `i8`; PowerShell `Set-Content -Encoding utf8` writes a BOM — use
  `[System.IO.File]::WriteAllText($p,$t,(New-Object System.Text.UTF8Encoding($false)))`;
  quote paths with spaces.
- **Storage budget**: repo ≤ ~5 GB (`target` ≈ 2 GB). Parallel worktrees share
  `CARGO_TARGET_DIR` or run sequentially; `cargo clean` stale caches.

## 5. Feature playbooks

| To add… | Steps |
| --- | --- |
| A setting | field + `Default` + `normalize()` clamp + `#[serde(default)]` → control in the right `settings_page_*` → consumer + `mark_dirty()` → test if clamped → SPEC §6.1 + README |
| A theme token | `Palette` field → default in `theme_defaults.rs` → one `Colour` variant + one `COLOUR_TOKENS` row in `theme_file.rs` (parser/template/coverage test follow) → use `theme::token()` |
| A sound action | `Action` variant → `handle_actions` arm → trigger in `sound_item`/context menu |
| A Settings tab | `SettingsTab` variant + `title()` + `ALL` → rail item (About stays pinned) → `settings_page_*` + `settings_content` match |
| A volume | only `ui/volume.rs` — store linear, display via `to_position`/`percent_text` |
| A file format | Symphonia feature → `model::SUPPORTED_EXTENSIONS` → dialog filter → test file |
| Audio routing | `Engine` = devices/routes, `Mixer` = voices/loop/gains, `Ring` = producer→device boundary; nothing blocking on the mixer thread |
| Hotkey behaviour | `hotkeys.rs`: `KEYS` table + `chord_owner`/`duplicate_sounds` (extend, never copy) → shared capture path in `app.rs` → SPEC §5.7 |
| A modal | `modal_shell` + `modal_header` + `close_button`; no one-off backdrops |
| A tray/menu item | `handle_menu_command` in `hotkeys.rs` |
| A settings reset | field → the owning arm of `Settings::reset_group` (coverage test names any orphan) → side effect in `App::apply_config`; the page's Reset row follows `SettingsTab::group` |
| Export/import data | `storage/bundle.rs` (stamps + `normalize()` + one `BundleError`) → UI only calls it; the board swap belongs to `App::apply_import` |

## 6. Guardrails (refuse these)

1. **No drift-class code**: one key table, one policy function, one renderer. Search
   before writing a helper; parameterize before copying. Symptom: two doc comments
   saying the same thing.
2. No `unwrap`/`expect`/trusted indices in production paths — slot arrays are typed.
3. No blocking the UI thread.
4. No per-frame allocation creep in paint code (String/clone/HashSet/Vec per item).
5. No magic numbers — named consts (radii SM/MD/LG/XL exist).
6. No dead state: write-only fields, parked `#[allow(dead_code)]`, ignored user-visible
   Results.
7. No lock-across-call; no new globals without a stated reason.
8. No behaviour change without a SPEC pass — but never "fix" a settled decision (§7).
9. No scope creep in a work unit; opportunistic refactors go to the refactor batch.
10. No panic on user data (paths, names, hotkeys, themes, JSON) — degrade + preserve.
11. Don't grow `app.rs` sideways: >~50 lines of new logic → question the module.
12. Keep code ↔ ARCHITECTURE ↔ SPEC ↔ this file in sync (§0).
13. Storage budget (§4): repo ≤ ~5 GB; no per-worktree target dirs.

## 7. Settled decisions (do not re-litigate)

Behaviour lives in SPEC.md. The ones agents most often get wrong:

| Decision | Where |
| --- | --- |
| **Pass-through hotkeys**: the keystroke also reaches the focused app. Intentional. | SPEC FR-5.7.2 |
| Win chords are bindable; because of pass-through, Windows also acts on them (README warns) | SPEC FR-5.7.2/5.7.10 |
| **Exclusive playback**: a trigger fades out all current voices. Intentional. | SPEC FR-5.4.1 |
| SFX go to both outputs equally (per-sound routing = future item) | SPEC FR-5.5.5 |
| Volumes stored linear, shown log (−60…0 dBFS); one curve | SPEC UI-4 |
| Portable: no installer/registry/AppData/network; JSON + Markdown themes; audio files never copied | SPEC ENV-2/4, FR-5.3.14 |
| Single instance per exe path; window size not persisted | SPEC ENV-5, FR-5.8.1 |
| RAM state hidden by default (diagnostics) | SPEC FR-5.9.9 |
| Hidden = minimize + taskbar-DeleteTab, never `Visible(false)` (burns a core) | ARCHITECTURE §6 |
| Stream buffer 8 chunks unless measured underruns | SPEC FR-5.3.13 |
| Duplicate hotkeys refused; conflicts unbind (new binding wins) | SPEC FR-5.7.5 |
| Delete-library dialog: move to chosen library / remove sounds / cancel; no undo | SPEC FR-5.2.3 |
| Missing sound: trigger logs an error; relink via context menu only | SPEC FR-5.3.6 |
| Per-sound trim multiplies the stored volume via one helper (`engine::effective_gain`); voices stay linear, boosts clip at master | SPEC FR-5.3.17 |
| A gone output device keeps its stored endpoint; the slot plays on a reported substitute | SPEC FR-5.5.7 |
| No progress bar until duration is known | decided 2026-10-28 |
| User `Graphite.md` shows alongside the built-in theme | decided 2026-10-28 |

Scope exclusions: SPEC §10 Non-goals. `plan.md` is the work list — implement items in
order; parallel work only on disjoint file sets (`app.rs`/`hotkeys.rs` serial); mark
items done as they land; the plan is wiped and rewritten when the iteration ends.

**QA loop.** Before a release, walk `QA-CHECKLIST.md` (expected behaviour per item;
back up `review\config\` before the destructive sections), record failures inline, then
fold the annotations into a fresh `plan.md` iteration. Test material:
`scripts\make-test-material.ps1` → gitignored `test-material/` (deterministic audio,
themes, fixtures — regenerate any time; never commit the generated files).
