# SoundDeck — Application Specification

| | |
| --- | --- |
| Document | `SPEC.md` |
| Describes | SoundDeck 0.2.0 |
| Status | Living document; update it with the code |
| Keywords | **MUST**, **SHOULD**, **MAY** as defined in RFC 2119 |

This is the behavioural contract. `README.md` is the user manual, `ARCHITECTURE.md`
is the implementation map, and `plan.md` (local, untracked) is the decision log.

---

## 1. Purpose

SoundDeck is a portable Windows soundboard. It plays short sounds and one looped track,
routed to two independent output devices, triggered by global hotkeys that never steal
the keystroke from the focused application.

The target user is a streamer or caller who needs separate mixes: one output for voice
chat (typically a virtual cable), one for the local monitor, with the looped track able
to go to either, both, or neither.

**Settled decisions — intentional behaviour, do not "fix" without changing this
specification:**

- **Pass-through hotkeys** (FR-5.7.2): the focused application always receives the
  triggering keystroke. A hotkey that plays a sound also types its key elsewhere.
  This is a product decision, not an omission.
- **Exclusive playback** (FR-5.4.1): every new trigger fades out all current voices,
  including a retrigger of the same sound. Sounds never overlap. Also a product
  decision.

## 2. Scope

**In scope**

- Sound libraries with per-sound volume, hotkey and RAM-preload behaviour.
- Two output slots with independent devices, volumes, link and master gain.
- A single streamed looping track with independent level and routing.
- Global hotkeys that pass through to the focused window.
- Tray residency with configurable minimize behaviour.
- Markdown themes, JSON config, everything portable beside the executable.
- Measured idle/active CPU and memory budgets (§8).

**Out of scope** — see §10.

## 3. Definitions

| Term | Meaning |
| --- | --- |
| **Sound** | One audio file registered in a library. |
| **Library** | A named list of sounds shown as one tab. |
| **Voice** | One in-flight playback of a sound inside the mixer. |
| **Slot** | One of the two outputs: `Monitor` or `Cable`. |
| **Ring** | Fixed 0.15 s FIFO between the mixer thread and a device thread. |
| **Master gain** | Global gain in dB applied to both mixes after the loop is mixed in. |
| **Loop track** | The single looping file, streamed from disk, not preloaded. |
| **Pass-through** | The focused application still receives a key that triggers a hotkey. |
| **Hidden** | Parked in the tray: minimized, no taskbar button, painting skipped. |

## 4. Platform and environment

| ID | Requirement |
| --- | --- |
| ENV-1 | Windows 10 1809 or newer, x86-64. WASAPI shared mode only. |
| ENV-2 | Portable: no installer, no registry writes, no `%APPDATA%` state. All state lives in `config/` beside the executable. |
| ENV-3 | The folder MUST be movable; paths inside it are stored relative. |
| ENV-4 | No network access at runtime. No telemetry, no accounts, no update checks. |
| ENV-5 | One instance per executable path; a second launch focuses the first and exits. |
| ENV-6 | Audio decoding uses Symphonia only. FFmpeg is not a dependency. |
| ENV-7 | Distribution is a zip containing SoundDeck.exe, README.md and LICENSE; no installer, no config, no audio. |
| ENV-8 | The UI targets 100 % Windows scaling; other factors MUST remain usable, not pixel-perfect. |

## 5. Functional requirements

### 5.1 Lifecycle

| ID | Requirement |
| --- | --- |
| FR-5.1.1 | Startup MUST load config, install themes, build the sound registry, open both outputs and start the hotkey/tray service before the first frame. |
| FR-5.1.2 | Startup failures MUST be reported in the activity log; the app SHOULD still open with the remaining devices. |
| FR-5.1.3 | Closing the window (custom close button, `Alt+F4`) MUST save and quit. There is no close-to-tray. |
| FR-5.1.4 | Dirty state MUST be persisted within 700 ms of the last change, and forced on hide, quit and `on_exit`. |
| FR-5.1.5 | Saves MUST be atomic (temp file + rename) with one rolling `.bak`. |
| FR-5.1.6 | A panic in a worker MUST NOT be the normal failure path; workers return errors through their event channel. |

### 5.2 Libraries

| ID | Requirement |
| --- | --- |
| FR-5.2.1 | A fresh config MUST contain one library named `Main` (id 1) and no sounds. |
| FR-5.2.2 | Libraries MUST be addable, renamable and deletable; the last remaining library MUST NOT be deletable. |
| FR-5.2.3 | Deleting a library MUST ask, in a confirmation dialog, whether to move its sounds to a remaining library the user chooses or to remove the sounds from the board; cancelling MUST leave the library untouched. Removing sounds MUST NOT touch their files (FR-5.3.14). |
| FR-5.2.4 | Exactly one library is active at a time; the active tab is persisted. |
| FR-5.2.5 | The active library selects which sounds are shown and which hotkeys are registered. |
| FR-5.2.6 | Sounds MAY be dragged between libraries and reordered inside one. |

### 5.3 Sounds

| ID | Requirement |
| --- | --- |
| FR-5.3.1 | Sounds MUST be addable from a file dialog and by dropping files or folders onto the board. |
| FR-5.3.2 | Supported extensions: `wav, mp3, ogg, flac, m4a, aac, opus, wma, aif, aiff`. |
| FR-5.3.3 | Duplicates MUST be rejected by canonical path, including the same file reached through a different case or path form. |
| FR-5.3.4 | A sound MUST have: id, display name (defaults to the file stem), path, volume in `0..=1`, optional hotkey, optional RAM preload. |
| FR-5.3.5 | RAM preload MUST only be meaningful for sounds with a hotkey; assigning no hotkey clears it. |
| FR-5.3.6 | A sound whose file is missing MUST be marked in the UI; triggering it MUST report the failure in the status log and MUST NOT open a dialog on its own. Relinking MUST be offered from the sound's context menu (Locate). |
| FR-5.3.7 | Removing a sound MUST be undoable once (`Ctrl+Z` restores the last removal at its original index). |
| FR-5.3.8 | Search MUST filter the active library by display name and by file name, case-insensitively. |
| FR-5.3.9 | Board layout MUST support grid and list; the choice is persisted. |
| FR-5.3.10 | A sound longer than 20 minutes MUST be rejected by the decoder with a clear message. |
| FR-5.3.11 | Decoded audio retained in memory MUST stay within 32 MB and 16 sounds. A single sound longer than 30 s is never preloaded; it plays from disk and the log says why. |
| FR-5.3.12 | A sound that is not resident MUST stream from disk through a bounded channel. It MUST NOT be fully decoded into memory in order to be played once. |
| FR-5.3.13 | The stream channel MUST default to 8 chunks of 1024 frames (~170 ms) and MUST be raised to 16 only if the `--stats` underrun counter or a listening test shows cracks. |
| FR-5.3.14 | SoundDeck MUST NOT create, rename, move or delete audio files. Renaming a card or removing a sound changes the board only. |
| FR-5.3.15 | Every configured file MUST be held open for reading with delete sharing denied, so Windows refuses rename and delete until the sound leaves the board or the app exits. Reads and writes by other processes stay allowed. |
| FR-5.3.16 | If the volume disappears, a resident sound MUST play from memory; a non-resident sound MUST be reported as missing, with no panic and no loss of the other sounds. |

### 5.4 Playback

| ID | Requirement |
| --- | --- |
| FR-5.4.1 | Playback MUST be exclusive per board: a new trigger fades out every voice that is still sounding, including a previous instance of the same sound. |
| FR-5.4.2 | Fades MUST be 2.5 ms in and 5 ms out, so triggers never click. |
| FR-5.4.3 | All sources MUST be converted to 48 kHz stereo `f32` before mixing; device rates are handled by a stateful resampler in the device thread. |
| FR-5.4.4 | The mixer MUST render 5 ms chunks (240 frames) on its own thread and push them into one ring per output. |
| FR-5.4.5 | The mixer MUST NOT busy-poll: with no voices and no loop it parks until a producer wakes it. |
| FR-5.4.6 | Starting a sound MUST be independent of decoding: a disk-backed sound decodes off the UI thread. |
| FR-5.4.7 | Active sounds MUST expose progress in `0..=1` for the card progress line. |
| FR-5.4.8 | "Stop all" MUST fade out every voice and MUST be available from the tray menu. |

### 5.5 Outputs and routing

| ID | Requirement |
| --- | --- |
| FR-5.5.1 | There MUST be exactly two slots, `Monitor` and `Cable`, each with its own device. |
| FR-5.5.2 | Selecting the same device for both slots MUST be allowed and MUST be reported in the activity log once. |
| FR-5.5.3 | Each slot MUST have a linear volume in `0..=1`; an optional link makes the sliders move together. |
| FR-5.5.4 | A master gain in `0..=+12 dB` MUST apply to both mixes after the loop is summed in. |
| FR-5.5.5 | Sound effects MUST go to both slots equally; only the loop is routed per slot. |
| FR-5.5.6 | Peak metering MUST be measured where the device consumes samples, so it reflects what is heard. |
| FR-5.5.7 | A slot whose device disappears MUST stop consuming, MUST report the failure, and MUST be restartable without restarting the app. |
| FR-5.5.8 | Settings MUST show whether each output is ready, with the last error when it is not. |

### 5.6 Looping track

| ID | Requirement |
| --- | --- |
| FR-5.6.1 | The loop MUST be streamed from disk with bounded memory (8 chunks of 1024 frames, ~170 ms) for any file length. |
| FR-5.6.2 | It MUST have its own linear volume in `0..=1`, independent of the slot volumes. |
| FR-5.6.3 | Routing MUST be independent per slot (`loop_on_monitor`, `loop_on_cable`); both off is a valid state. |
| FR-5.6.4 | The mixer MUST keep consuming the stream while both routes are off, so re-enabling is seamless. |
| FR-5.6.5 | State MUST be one of `Idle`, `Playing`, `Error(message)`; the loop card and Settings MUST show the same state and wording. |
| FR-5.6.6 | A missing, corrupt or unreadable file MUST surface as `Error` in the UI and MUST NOT keep the mixer awake. |
| FR-5.6.7 | Enabling the loop MUST NOT auto-start playback; the card appears and waits for Play. |

### 5.7 Hotkeys

| ID | Requirement |
| --- | --- |
| FR-5.7.1 | Hotkeys MUST be global and MUST work while the window is hidden or unfocused. |
| FR-5.7.2 | A hotkey MUST NOT swallow the keystroke: the focused application still receives it. |
| FR-5.7.3 | Key auto-repeat MUST trigger at most once per physical press. |
| FR-5.7.4 | Hotkeys MUST be assignable through a capture dialog that reads the keyboard directly, with `Esc` to cancel. |
| FR-5.7.5 | The capture dialog MUST refuse to bind a combination already used by another sound in the active library or by the stop-all hotkey, naming the current owner. A conflict introduced outside the dialog MUST resolve the same way every time — the stop-all hotkey outranks sounds and the first sound in tab order outranks later ones — with the losing sound's hotkey cleared exactly as if it had no hotkey (FR-5.3.5) and a log entry naming the sound. |
| FR-5.7.6 | Hotkeys MUST be clearable without removing the sound. |
| FR-5.7.7 | Only the active library's hotkeys are registered; switching tabs rebinds them. |
| FR-5.7.8 | Hotkeys are best-effort: elevated foreground windows and some anti-cheat/AV tools can block a low-level hook. This MUST be documented, not hidden. |
| FR-5.7.9 | An optional global stop-all hotkey MUST be off by default, own a sidebar widget above the meters when enabled, pass the key through, and work while hidden. |
| FR-5.7.9 | Bare keys without a modifier are allowed; pass-through means the keystroke still reaches the focused application. |

### 5.8 Window and tray

| ID | Requirement |
| --- | --- |
| FR-5.8.1 | The window MUST have custom chrome (no OS decorations), start at 960×540, minimum 860×500, and not persist its size. |
| FR-5.8.2 | Minimize MUST follow `minimize_to_tray`: off → taskbar, on → tray. |
| FR-5.8.3 | Tray double-click MUST toggle: visible → hidden, hidden or minimized → restored and focused. |
| FR-5.8.4 | A single tray click MUST do nothing; the right-click menu is the fallback. |
| FR-5.8.5 | The tray menu MUST offer Show/Hide (same toggle), Stop all and Quit. |
| FR-5.8.6 | While hidden there MUST be no taskbar button, no painting, and an idle repaint rate of ~1 frame per second. |
| FR-5.8.7 | Restoring MUST return to the same tab, scroll position and selection, with meters reset. |
| FR-5.8.8 | A stale `minimized` signal MUST NOT re-hide the window immediately after a restore. |

### 5.9 Settings

| ID | Requirement |
| --- | --- |
| FR-5.9.1 | Settings MUST replace the board in the same window and MUST have four tabs: Playback, Appearance, Window, About. |
| FR-5.9.2 | Controls that belong to the board (devices, output volumes, link, master gain, RAM summary) MUST NOT be duplicated in Settings. |
| FR-5.9.3 | Playback MUST own: loop on/off, loop file, loop volume, loop routing. |
| FR-5.9.4 | Appearance MUST own: theme, reload, motion. |
| FR-5.9.5 | Window MUST own: minimize to tray, sidebar gain visibility. |
| FR-5.9.6 | About MUST show the application name, version, config folder, licence and project links, plus the author credit. |
| FR-5.9.7 | Every control MUST save immediately (debounced), with no Apply/Cancel step. |
| FR-5.9.8 | Spacing MUST stay compact: group gap 5/8 px, card padding 10 px, divider 7 px, row control height 28 px. |
| FR-5.9.9 | RAM/disk state (card chips, counts, toolbar pill) is hidden by default; a Playback switch reveals it. |
| FR-5.9.10 | About MUST offer **Export configuration**: one version-stamped JSON bundle of settings and libraries, written atomically. Audio files MUST NOT be copied into it; paths stay references. |
| FR-5.9.11 | Import MUST validate the bundle's kind and version, report a corrupt, foreign or newer file as a status error, and replace settings and libraries only after the user confirms a dialog stating how many sounds and libraries will be replaced. Cancel MUST change nothing. |
| FR-5.9.12 | Playback, Appearance and Window MUST each offer a **Reset** that restores only that page's fields. About MUST offer **Reset all settings** behind a confirmation. Neither MUST touch libraries, sounds or audio files. |

### 5.10 Themes and appearance

| ID | Requirement |
| --- | --- |
| FR-5.10.1 | Themes MUST be Markdown files in `config/themes/<id>.md` containing a heading and a `\| token \| value \|` table. |
| FR-5.10.2 | The file stem is the theme id; the heading is the display name. |
| FR-5.10.3 | A missing or invalid token MUST fall back to the Graphite value. |
| FR-5.10.4 | Markdown, not JSON: users must be able to copy a theme and edit colours in a text editor. |
| FR-5.10.5 | Palette colours MUST come from tokens. No hardcoded colours outside the theme module, except the documented `Color32::WHITE` on the active layout segment. |
| FR-5.10.6 | Motion MUST honour both the user setting and the OS animation preference. |
| FR-5.10.7 | Theme reload MUST NOT accumulate font atlases. |

### 5.11 Persistence

| ID | Requirement |
| --- | --- |
| FR-5.11.1 | `config/settings.json` and `config/libraries.json` are versioned JSON (`version: 2`). |
| FR-5.11.2 | Both files are written atomically with one `.bak` each. |
| FR-5.11.3 | A corrupt file MUST be preserved as `.corrupt` and replaced by defaults, with a warning in the log. |
| FR-5.11.4 | Unknown fields MUST be ignored on read; missing fields MUST take defaults. |
| FR-5.11.5 | All values MUST be normalised on load (volumes to `0..=1`, gain to `0..=12`). |
| FR-5.11.6 | The legacy `sounddeck.json` MUST be imported once and kept as `sounddeck.v1.json.bak`. |
| FR-5.11.7 | Serialization failures MUST be reported, never written as an empty file. |

### 5.12 Diagnostics

| ID | Requirement |
| --- | --- |
| FR-5.12.1 | `--diag [file]` opens the devices, decodes and plays a file, prints device/event/progress data, and exits. |
| FR-5.12.2 | `--stats` MUST append one line every 5 s to `%TEMP%\sounddeck-stats.log`: fps, frame time, repaint interval, hidden flag, mixer wakeups, ring underruns, voices, loop state. |
| FR-5.12.3 | `--settings` MUST open the app on the Settings screen. |
| FR-5.12.4 | The activity log MUST record load failures, device errors, save problems and hotkey conflicts. |
| FR-5.12.5 | A panic MUST write a line to `crash.log` next to the executable before the process aborts. |

## 6. Data formats

### 6.1 `config/settings.json`

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `version` | u32 | 2 | Schema version |
| `active_library` | u64 | 1 | Last selected tab |
| `theme` | string | `"graphite"` | Theme id |
| `layout` | `grid` \| `list` | `grid` | Board layout |
| `monitor_device` / `cable_device` | string? | null | WASAPI endpoint ids |
| `monitor_name` / `cable_name` | string? | null | Friendly names, used to re-resolve |
| `monitor_volume` / `cable_volume` | f32 | 1.0 | `0..=1` |
| `master_gain_db` | f32 | 0.0 | `0..=12` |
| `link_volumes` | bool | true | Sliders move together |
| `preload_hotkey_sounds` | bool | true | Default for new hotkey sounds |
| `loop_enabled` | bool | false | Shows the loop card |
| `loop_path` | string? | null | Same path rules as sounds |
| `loop_volume` | f32 | 1.0 | Independent of slots |
| `loop_on_monitor` / `loop_on_cable` | bool | true | Routing |
| `animate_ui` | bool | true | User motion preference |
| `minimize_to_tray` | bool | false | false = taskbar |
| `show_gain_slider` | bool | true | Sidebar gain |
| `window_size` | `[f32;2]`? | null | Parsed for old files, unused |

### 6.2 `config/libraries.json`

```json
{
  "version": 2,
  "libraries": [
    {
      "id": 1,
      "name": "Main",
      "sounds": [
        {
          "id": 1,
          "name": "Airhorn",
          "path": "sounds/airhorn.wav",
          "hotkey": "Shift+T",
          "volume": 0.8,
          "ram": true
        }
      ]
    }
  ]
}
```

`ram: null` means "inherit `preload_hotkey_sounds`". Paths inside the app folder are
stored relative; anything outside is absolute.

### 6.3 `config/themes/<id>.md`

```markdown
# Graphite

| token | value |
| --- | --- |
| bg | #0F1115 |
| panel | #151922 |
| accent | #0097FF |
```

### 6.4 Configuration bundle (export/import)

```json
{
  "app": "sounddeck",
  "bundle": 1,
  "settings": { "...": "the 6.1 shape" },
  "libraries": { "version": 2, "libraries": [ "...": "the 6.2 shape" ] }
}
```

- The two members are the config files' own shapes: an import normalises them exactly
  like a load (`normalize()`, ids, `active_library`), so a bundle is never trusted more
  than a hand-edited file.
- `app` MUST be `sounddeck` and `bundle` MUST be 1..=current; a higher number is refused
  as "from a newer version".
- Paths are references, relative inside the app folder, so a folder that moved together
  re-resolves on import. Audio files are never copied into the bundle or out of it.

## 7. UI specification

### 7.1 Screens

| Screen | Contents |
| --- | --- |
| **Board** | Title bar (logo, library tabs, window buttons), toolbar (search, RAM pill), grid or list of sounds, sidebar (outputs, meters, loop card, Settings). |
| **Settings** | Left rail (Back + four tabs), content column, same title/status bars. |
| **Modals** | Hotkey capture, rename (sound or library), delete library, import confirmation, reset-all confirmation, activity log. |

### 7.2 Layout invariants

| Constraint | Value |
| --- | --- |
| Title bar height | 38 px |
| Status bar height | 34 px |
| Sidebar width | 300 px |
| Board list max width | 1100 px, centred |
| Settings content max width | 780 px |
| About column max width | 680 px |
| Sidebar compact mode | below 620 px height: labels, dB scale and credit hidden |

The sidebar divider MUST be painted inside the central panel layer, never as its own
top-level layer, so it cannot draw over modals or popups.

### 7.3 Interaction rules

| ID | Rule |
| --- | --- |
| UI-1 | Modals use `egui::Modal` with a transparent backdrop: the board stays visible, input behind is blocked, and nothing is dimmed. |
| UI-2 | All close buttons are the same control: dark X, red fill and red X on hover. |
| UI-3 | Cards and rows share the same chrome: name, hotkey pill, RAM chip, missing state, volume bar, progress line, context menu. |
| UI-4 | Volume uses one logarithmic curve everywhere; the bar fill and percentage are derived from the same value. |
| UI-5 | Drop target highlights only while a drag is over the window. |
| UI-6 | Status messages are terse and never stack duplicates for the same event. |

### 7.4 Keyboard (application-local)

| Key | Action |
| --- | --- |
| `Ctrl+O` | Add sounds |
| `Ctrl+F` | Focus search |
| `Ctrl+L` | Toggle grid/list |
| `Ctrl+,` | Settings |
| `Ctrl+Z` | Undo removal |
| `Ctrl+Q` | Quit |
| `Esc` | Close popup → leave Settings → clear search → stop playback |
| `Delete` | Remove selected |
| `Space` | Play selected |
| `F2` | Rename selected |

## 8. Non-functional requirements

### 8.1 Performance budgets (measured on the reference machine)

| ID | Budget | Measured 0.2.0 |
| --- | --- | --- |
| NFR-8.1.1 | Visible idle ≤ 1 % of one core | **0.26 %** |
| NFR-8.1.2 | Hidden ≤ 0.3 % of total CPU | **0.07–0.11 %** |
| NFR-8.1.3 | Idle frames ≤ 1/s, each ≤ 1 ms | **0.7 ms/frame** |
| NFR-8.1.4 | Mixer wakeups ≤ 25/s idle | **20/s** |
| NFR-8.1.5 | Playing audio ≤ 3 % of one core | ~2 % at 30 fps |
| NFR-8.1.6 | Playback start latency not worse than the 0.15 s ring | unchanged |
| NFR-8.1.7 | Trigger to audible MUST be ≤ 250 ms | ~165 ms |

Rules that enforce them:

- No unconditional `request_repaint()`; frames are requested only by events and by
  `repaint_interval()`.
- No painting, meter smoothing or tooltip work while hidden.
- No per-frame filesystem calls or window enumeration.
- Nothing MAY call `timeBeginPeriod` beyond process scope.

### 8.2 Memory

| ID | Requirement |
| --- | --- |
| NFR-8.2.1 | Private working set MUST stay ≤ 100 MB at idle with a normal library. |
| NFR-8.2.2 | Current measurement: 71–75 MB. The bare eframe/egui/glow window with the same crates measures 54.9 MB, so ~55 MB is the floor of this UI stack. |
| NFR-8.2.3 | 40 MB is an aspiration. It is not reachable with eframe/glow and requires a different UI stack (§13). |
| NFR-8.2.4 | RAM-preloaded sounds count against the budget; the loop never does (streamed). |
| NFR-8.2.5 | No decoded buffer may be held after a sound is no longer retained. |
| NFR-8.2.6 | Retained decoded audio ≤ 32 MB and ≤ 16 sounds; the process refuses work that would cross its commit ceiling. Measured after hardening. |
| NFR-8.2.7 | The loop is excluded from these caps: it is streamed, not retained. |
| NFR-8.2.8 | One-shot playback is also excluded: streamed voices hold one bounded channel (130 KB) regardless of file length. No playback path decodes a whole file into RAM. |

### 8.3 Responsiveness and robustness

| ID | Requirement |
| --- | --- |
| NFR-8.3.1 | The UI MUST NOT block on decoding, loop open or device enumeration. |
| NFR-8.3.2 | Device and loop restarts MUST be bounded; a stopped worker MUST NOT leave a handle that looks alive. |
| NFR-8.3.3 | User data (paths, names, hotkeys, themes) MUST NOT be able to panic the app. |
| NFR-8.3.4 | Corrupt config MUST degrade to defaults with the original preserved. |
| NFR-8.3.5 | A missing audio file MUST produce a UI state, not a silent failure. |

### 8.4 Privacy and security

| ID | Requirement |
| --- | --- |
| NFR-8.4.1 | No network traffic, no telemetry, no crash upload. |
| NFR-8.4.2 | External actions are limited to: open the config folder, open a file dialog, open an `http(s)` link. |
| NFR-8.4.3 | URLs MUST be validated as `http(s)` before being handed to the shell. |
| NFR-8.4.4 | No credentials, tokens or personal data are stored anywhere. |

### 8.5 Maintainability

| ID | Requirement |
| --- | --- |
| NFR-8.5.1 | `cargo fmt --check` MUST pass. |
| NFR-8.5.2 | `cargo test` MUST pass; pure logic (paths, normalisation, migration, volume curve, hotkey table, ring/wake behaviour) MUST be covered. |
| NFR-8.5.3 | Every module carries a `//!` header stating its responsibility. |
| NFR-8.5.4 | `ARCHITECTURE.md` MUST be updated with any change to threads, storage or the frame loop. |
| NFR-8.5.5 | 45 tests as of 0.2.0. |

### 8.6 Accessibility

| ID | Requirement |
| --- | --- |
| NFR-8.6.1 | The OS animation preference MUST be honoured. |
| NFR-8.6.2 | Text contrast is the theme author's responsibility; the default themes MUST meet normal-text contrast. |
| NFR-8.6.3 | Screen-reader support is **not** implemented today (egui accesskit is disabled). This is a known gap. |

## 9. Failure handling

| Failure | Required behaviour |
| --- | --- |
| Sound file missing | Card marked; triggering logs an error; relink via context menu → Locate. |
| Sound decode fails | Log entry with the reason; the sound stays registered. |
| Loop file missing/corrupt | Loop state `Error`; mixer parks; retry available. |
| Device unplugged | Slot stops, `DeviceError` logged, `device_ready` cleared, route restartable. |
| Device returns no callback for 2 s | Treated as failure rather than polling forever. |
| Config corrupt | Preserve as `.corrupt`, load defaults, warn. |
| Save fails | `save_failed` set, warning logged, no empty file written. |
| Tray unavailable | `minimize_to_tray` disabled in Settings; minimize falls back to the taskbar. |
| Keyboard hook blocked | Log entry; app remains usable with the board and tray menu. |
| Hotkey conflict | The capture dialog refuses the combination and names its owner; a conflict found on load, tab switch or stop-all change unbinds the losing sound (hotkey cleared, RAM off) and logs it. |

## 10. Non-goals

- macOS or Linux builds.
- Multiple simultaneous loop tracks, crossfading, or per-sound output routing.
- Recording, streaming, or VST/effect processing.
- Cloud sync, accounts, auto-update, telemetry.
- FFmpeg or any external codec binary.
- Installer, MSI, or registry integration.
- Network playback or remote control.
- Native look-and-feel; the UI is intentionally custom-drawn.

## 11. Acceptance criteria

A release is acceptable when all of the following hold.

1. `cargo test` is green and `cargo fmt --check` is clean.
2. Fresh folder, no config: the app opens on the board with one `Main` library.
3. Add, rename, delete, undo, search, drag between libraries all behave per §5.3.
4. Both outputs play the same sound; per-slot volume and link work; master gain is audible.
5. Loop: choose, play, stop, missing file, corrupt file, both routes off and back on.
6. Hotkeys: trigger while another app is focused and the keystroke still lands.
7. Tray: hide, restore, hide again, Stop all, Quit; no taskbar button while hidden.
8. Settings: every control saves immediately and survives a restart.
9. Theme: edit a colour in `config/themes`, reload, see it; delete a token, see the fallback.
10. Budgets: `--stats` shows ≤1 fps idle, ≤25 mixer wakeups/s, hidden CPU ≤0.3 % total,
    private working set ≤100 MB.

## 12. Versioning and compatibility

- The application version follows SemVer (`MAJOR.MINOR.PATCH`) and lives only in
  `Cargo.toml`; the About tab reads it from there.
- `0.x` means "unstable": config may change between minor versions, but a migration
  MUST be provided for anything that would otherwise lose user data.
- `1.0.0` is reserved for the point where the config schema and behaviour are frozen
  enough to promise backwards compatibility.
- Releases are tagged `vMAJOR.MINOR.PATCH`.
- Config files carry their own `version` field; readers MUST accept older versions and
  write the current one.

## 13. Open questions

| Topic | State |
| --- | --- |
| 40 MB memory target | Requires replacing eframe/glow. Options analysed: Rust + Win32/Direct2D (~12–20 MB), egui + software raster (~35–45 MB), Qt (45–70 MB, rejected). Not scheduled. |
| Accessibility layer | No screen-reader support today. |
| Code signing | Unsigned releases trigger SmartScreen; a certificate is a cost decision. |
| Hotkey robustness | Hook is best-effort against elevated windows and anti-cheat tools. |
| UI font | Satoshi is bundled under the Fontshare FFL (`assets/Satoshi-FFL.txt`). An OFL font such as Inter would simplify redistribution but is not required. |
