# SoundDeck 0.5.0 — manual QA checklist

Walk the app top to bottom in this order. Each item: **Do** → **Expect**. Tick what
passes; for anything else write `ID · what you did · what happened` (screenshots help).
Nothing in the app changes until this pass is done — A7 (empty-library delete) is
already parked and waiting.

**Before you start**

- [ ] Copy `sounddeck\review\config\` somewhere safe. Sections 12–14 mutate settings,
      and **Import replaces the whole board**. (The app never touches audio files, but
      the board itself can go.)
- [x] Test at 960×540 and once more at 1200×800; note the window scale you use (SPEC
      targets 100 %; other factors = usable, not pixel-perfect).
- [x] You are running `review\SoundDeck.exe` = build 0.5.0 (About must say 0.5.0).

**Known nuances — expected, not bugs**

| Thing | Why |
|---|---|
| A Win hotkey (e.g. `Win+A`) also opens Action Center | Pass-through is a settled decision; the chord reaches Windows too (README warns) |
| A streamed sound shows **no progress line** the first time it plays | Progress appears only after the length probe lands (next trigger) — settled |
| **Deleting an empty library still shows the dialog** | A7, already planned — that's the change you asked for |
| A device unplugged **at launch** is forgotten (falls back silently) | Device auto-rebind is a future item; runtime unplug IS reported |
| Startup warnings are silent in release | Decided 2026-10-28 (no `startup.log`) |
| OS-level hotkeys (including stop-all) still fire while a modal is open | Pass-through is settled: the hook never swallows a key. Board shortcuts do stand down |

---

## 1. First run & window chrome

- [x] **Fresh install**: rename `review\config` away, launch. **Expect**: board opens with
      one **Main** tab, empty-state card ("drop files here"/click to add), no errors;
      `config\themes\` contains the 5 seeded themes; `config\settings.json` + `libraries.json` created. - [only when I interact with anything that changes the defaults]
- [x] Drag the title bar (not over tabs) → window moves; double-click title bar → maximize/restore.
- [x] Hover the three window buttons → Minimize/Maximize highlight, **Close turns red**.
- [x] Resize from each window edge (6 px hot zone) → resize cursor; below 860×500 it refuses.
- [x] Size is **not** remembered: quit, relaunch at a different size → back to 960×540.
- [x] Launch a **second copy** (same exe) → it focuses the running window and exits (no second window, no crash).
- [x] Library tabs: horizontally scrollable when many; "+" button visible.
- [x] Restore your real `config` folder now.

## 2. Tray & window lifecycle

- [x] Tray icon present; tooltip reads "N sounds" (or "N playing" while audio runs); updates within ~3 s of a change.
- [x] **Single left-click** the tray icon → nothing happens (deliberate).
- [x] **Double-click** → window hides (taskbar button disappears too); double-click again → restored, focused, same tab/scroll/selection; meters do not flash stale peaks.
- [x] Right-click tray → **Show/Hide, Stop all, Quit** all work. Quit saves and exits cleanly.
- [x] Minimize (default settings) → button in **taskbar**; restore works.
- [x] Settings → Window → **Minimize to tray ON** → minimize hides to tray (no taskbar button); restore via tray; app does **not** instantly re-hide after restoring (stale-minimize guard).
- [x] Close button / Alt+F4 → saves and **quits** (there is no close-to-tray).
- [x] While hidden: CPU near zero (Task Manager), hotkeys still fire (test one). - 0.5% overall util ceiling

## 3. Adding sounds

- [x] Sidebar **Load sounds** and **Ctrl+O** both open the file dialog; multi-select several files → all appear as cards named after the file stem; one summary line in the status bar.
- [x] Drop files anywhere on the board → same result. Drop a **folder** → its supported audio files are added.
- [x] Add the same file again (or via a different path spelling) → skipped as duplicate (summary says so), no second card.
- [x] Add an unsupported file (e.g. `.txt`/`.zip`) → skipped, counted in the summary.
- [x] Long/odd names (very long name, emoji) → card name truncates with "…" and never breaks layout.

## 4. Playing & audio (the core loop)

- [x] Click a card → sound plays; audio comes out of **both** Monitor and Cable simultaneously (listen on both).
- [x] Click another card mid-play → previous sound **fades out** (exclusive playback, no overlap). Retriggering the same card also restarts cleanly — no clicks/pops at either edge.
- [x] **Space** plays the selected card; context menu **Stop** stops just that sound.
- [ ] Volume bar on the card: drag → percent updates (log feel: small moves at the bottom matter); mouse **wheel** over it adjusts and **does not scroll the page**; value persists after restart. *(B10 fixed — the bar consumes the wheel on every frame the pointer is over it; re-check on cards, list rows and the sidebar sliders. No speaker glyph on any bar any more — B5.)*
- [ ] Context menu **Set gain…** (sits between Stop and Rename) opens the trim dialog: slider over **−12.0…+12.0 dB in 0.5 dB steps**; the numeric field and the slider follow each other both ways; typing `40` clamps to `+12.0 dB` and `-40` to `-12.0 dB`; garbage (`abc`, empty, `--`) is ignored — no panic, no NaN; the sign is always shown (`+3.0 dB`, `0.0 dB`); **OK** applies to a sound that is **already playing**; **Cancel / Esc / X / backdrop click** leave the trim untouched.
- [x] Progress line: visible mid-playback on a previously played/short file; **first** trigger of a freshly added streamed file may show none — then appears on the next play.
- [-] **Missing file** (rename a file on disk, then click its card) → status bar shows `"name": file missing - right-click > Locate`; **no file dialog opens**; repeated clicks refresh the line (no log spam).
- [-] Right-click that card → **Locate file…** → picker → choose the renamed file → card healthy again; **search finds it by the new filename** immediately.
- [x] Trigger a hotkey while Notepad is focused → sound plays **and** the keystroke lands in Notepad (pass-through).
- [x] Hold a hotkey down → exactly **one** trigger (no auto-repeat spam).
- [x] **Stop all** (tray item, Esc chain, sidebar card click) → all sounds fade out together; the loop track keeps playing (if on).

## 5. Search & layout

- [x] Type in search → only matching sounds in the **active** tab remain (matches name AND filename, case-insensitive); typing feels smooth (debounced, no per-keystroke hitch).
- [x] Clear search (button/Esc) → full list back; a search with no matches shows the "no results" state + clear action.
- [x] **Ctrl+F** focuses search. **Ctrl+L** toggles grid ↔ list; the choice survives restart.
- [x] List view rows: same controls as cards (name, pill, chip, volume, progress, menu), volume hidden on very narrow rows.
- [ ] **Right edges line up**: the toolbar's RAM pill and the grid's last column end on the same x — check at 960 px **and** 1200 px (B4; the toolbar and the grid size from one `board_content_width`).

## 6. Select, rename, remove, undo

- [x] Click a card → selection highlight; **F2** opens rename; Esc cancels (name unchanged); typing + Enter/Save applies; empty name is rejected.
- [x] **Delete** removes the selected sound; status line confirms.
- [x] **Ctrl+Z** restores it at its **original position** with volume/hotkey intact and **no false "missing" flag** if the file exists.
- [x] Ctrl+Z with nothing to undo → "Nothing to undo" (info, not error).
- [x] While renaming, type `Ctrl+Z`, `Ctrl+F`, `Ctrl+O` → **nothing happens** (no undo, no dialogs stealing focus); **Ctrl+Q** and **Ctrl+,** still work.
- [ ] Right-click a card → **Show in Explorer** → Explorer opens with the **file selected**. Try `weird name, with spaces & commas.wav`, `emoji-tone.wav` and the 150-character name: all three must land on the file, not on Documents (B3 — the shell API takes a path, not a command line). A file that no longer exists opens its folder instead.

## 7. Drag & drop organization

- [x] Drag a card to another slot in grid **and** list → reorders exactly where dropped.
- [ ] Drop on a **library tab** → the tab tints while the drag is over it; the sound moves there (disappears from the current list) and the **active tab does not change**; context menu **Move to ▸** does the same. Dropping on the sound's **own** tab is a no-op. *(B11 fixed.)*
- [x] Moving a **playing** sound to another library stops its playback; moving a silent one keeps everything.
- [x] Start dragging, delete the drop-target sound from another path mid-drag → drop is **skipped** (nothing jumps to a wrong slot).

## 8. Libraries / tabs

- [x] **+** creates a library and immediately opens its rename dialog; it becomes active.
- [x] Tab context menu: **Rename…** works (Esc cancels); **Delete** (see below). Last remaining library has **no delete** (or refuses).
- [x] Switch tabs → sound list swaps, search cleared, playback stops, **hotkeys switch to the new tab's set**, loop keeps playing.
- [ ] **Ctrl+Tab cycling** (Settings → Window → "Cycle tabs with Ctrl+Tab", **off by default**): off → `Ctrl+Tab` does nothing to the board (it falls through untouched); on → `Ctrl+Tab` = next library, `Ctrl+Shift+Tab` = previous, both wrap around; ignored while a text field has focus or a modal is open. Survives a restart.
- [x] Delete library **with sounds** → dialog: `Move to [▾]` (defaults to first remaining), **Move sounds**, **Delete sounds** (red label), **Cancel**:
      - [x] Cancel → nothing changes.
      - [x] Move → sounds appear in the chosen library; counts correct; log summary.
      - [x] Delete sounds → sounds gone from board; **audio files still on disk**; log summary.
- [x] Delete an **empty** library → dialog appears today (**known nuance, A7**); record whether it bothers you beyond the empty case.
- [-] RAM rule: only the **active** tab's hotkey sounds sit in RAM (check chips in two tabs).

## 9. Hotkey management

- [x] Context menu **Set hotkey…** → modal shows the chord as you press it; **Esc** cancels; Save stores it; the pill shows `Ctrl+Shift+X` style text.
- [x] Click the pill → capture modal prefilled with the current chord; **Clear hotkey** (menu) removes it; sound still plays by click.
- [x] **Duplicate refusal**: give sound B a chord already on sound A → Save disabled with "already used by …" (name shown). Same against the **stop-all** chord, both directions.
- [-] **Unbind-on-conflict**: set the stop-all hotkey to a chord a sound owns → the **sound** loses its hotkey (pill empty, warning in activity log, RAM chip gone); stop-all works. [Impossible to test]
- [x] Hand-edit a duplicate chord into `libraries.json`, restart → deterministic: first sound in tab order keeps it; the other is unbound with a warning.
- [ ] Bare key hotkey (`F8`) works; chord with Win (`Win+X`) binds from the capture modal **and** fires (while also triggering Windows — known). *(B8 fixed: the modal folds the physical Win key into the chord, and derives it from the physical keyboard when the shell swallowed the event.)*
- [ ] **Blocked chords**: in the capture modal press `Ctrl+C` / `Ctrl+V` / `Ctrl+Z` / `Space` / `Delete` / `F2` / `Alt+F4` → the dialog shows exactly `Ctrl+C is a system shortcut and can't be used` and **Save does nothing** (nothing changes, no toast). Then hand-edit a blocked chord into `libraries.json` (a sound's `hotkey` and the stop-all key), relaunch → both are **unbound** with a warning in the activity log, the sounds stay on the board and still play by click.
- [ ] Stop-all hotkey (Settings → Playback, off by default): enable → sidebar widget appears above the meters; pill-click captures, right-click clears; the chord stops everything **while hidden**. **Clicking the card body** stops all sounds (fade-out) and logs `Stopped all sounds`; the pill still captures on click and clears on right-click. *(B7 fixed.)*
- [x] Hotkeys fire while the app is **minimized to tray** and while another app has focus.

## 10. Sidebar: devices, volumes, meter

- [ ] Monitor & Cable dropdowns list devices + "None (silent)"; pick a different device → audio moves there (a running sound may restart on the new route — note what you hear).
- [ ] **"None (silent)" closes the slot**: switch a slot to None **while a sound is playing** → that output goes silent at once (the other keeps playing) and stays silent after a restart; the stored choice is not re-resolved to a device. *(B6 fixed — silence is stored as the slot's name with an empty id, so the substitute rule never applies to it.)*
- [x] Dropdown context **Refresh devices** works; a renamed device shows its new name after refresh.
- [x] Volume sliders: drag moves the % label; **link toggle** makes one slider move both; unlink and they separate; values persist.
- [ ] Master gain: 0…+12 dB, audible at max; label shows `+3.0 dB` style with sign; hidden when "Show gain slider" is off (Settings → Window). **Right-click on the slider snaps it back to `0.0 dB`** without starting a drag or disturbing the output levels (B9).
- [ ] **Sidebar order** (top to bottom of the scrollable part): devices + link → **Gain** → **Stop all** → **Loop**; the credit, the peak meter and the Settings button stay **pinned** at the bottom and never scroll away. On a 540 px-tall window the whole upper stack scrolls instead of hiding a widget (F4).
- [x] Peak meter: green/yellow/red zones track playback on **each** output independently; falls to floor when silence; no stale flash after restore from tray.
- [ ] **Unplug a device** mid-play → status: activity-log warning + Settings → Playback shows the amber `"X" not found - using "Y"` line; audio continues on the fallback. Replug + refresh → resolves back. 

## 11. Loop track

- [x] Settings → Playback → **Looping track: Enabled** → loop card appears in sidebar, **stopped** (no auto-play).
- [x] Choose a file (and **Locate** when missing) → name shows; Play → loops seamlessly at the wrap; volume bar independent of output volumes.
- [x] **Loop outputs**: Monitor only → Cable silent; Cable only → Monitor silent; **both off** → silent but position keeps (re-enable mid-play resumes seamlessly, not from 0).
- [x] Loop survives: tab switch, Stop all, minimize/restore; stops on quit and when disabled.
- [x] Missing loop file → card shows **Error** with message; retry possible; mixer idle (no CPU). *(Play stays disabled while the file is missing — intended, no change.)*
- [-] Corrupt loop file → **Error** state, no crash. [too lazy to test, let's just say it works cuz missing loop file finds error]

## 12. Settings deep pass (⚠ mutates config — backup first)

- [x] Opens via sidebar **Settings** and **Ctrl+,**; left rail: Back, Playback, Appearance, Window, About (pinned bottom); **Esc** leaves.
- [x] **Playback**: every toggle flips and saves immediately (no Apply button): loop Enabled, loop outputs, **Load into RAM** (shows purple half-knob when partly loaded — a click loads all), Show RAM state, Stop-all enabled + its hotkey row, loop file row.
- [x] **Appearance**: theme dropdown switches live; **Reload themes** re-reads disk; **Animate UI** off → hovers/presses/fades snap instantly (and CPU drops).
- [x] **Window**: Minimize to tray; Show gain slider (affects sidebar immediately).
- [x] **About**: version reads **0.5.0**; Config folder opens Explorer at the right place; links open the browser (only http/https); credits/text intact.
- [x] **Reset this page** on each of Playback/Appearance/Window → only that page's fields reset; other pages untouched; log line.
- [x] **Reset all settings** (About → Data) → confirm modal, Cancel works; on confirm everything returns to defaults (theme graphite, grid layout…), **libraries/sounds untouched**; survives restart.
- [x] **Export configuration** → save dialog → `.json` written where you chose; open it: readable, contains settings + libraries.
- [x] **Import configuration**: cancel → nothing changes. Confirm → board **replaced live** (counts, tabs, hotkeys rebind; log summary). Import your earlier export → board restored exactly.
- [x] Import a **corrupt** file / a hand-made newer-version bundle → clear rejection message, board untouched.
- [ ] While Settings is open: board shortcuts are dead (except Ctrl+Q/Ctrl+,); modals opened from Settings are **centred over the settings content**, not 150 px left of centre (B13 — re-check the capture, rename, gain, log, import and reset modals).

## 13. Themes

- [x] Switch theme → whole UI recolours live; choice survives restart.
- [x] Edit a colour in `config\themes\<id>.json` → Settings → Reload themes → change visible **without reselecting the theme** (B12 — reload re-resolves the active id).
- [ ] **Rename a theme file** (`mocha.json` → `mocha2.json`) while it is the active theme → Reload: the log warns `theme "mocha" is not in config/themes (available: …); using Graphite`, the UI repaints in Graphite, and `settings.json` now stores `"graphite"`.
- [ ] **Two files, one name**: copy `graphite.json` to `mytheme.json` and leave `"name": "Graphite"` → the dropdown lists `Graphite` **and** `Graphite (mytheme)`, and picking either is distinguishable. Change the name to `My Graphite` → the entry reads `My Graphite (mytheme)`.
- [ ] Delete a token from a theme file → reload → falls back to Graphite's value; warning in activity log. Re-add it → returns.
- [ ] Invalid hex (`"accent": "banana"`) → fallback + warning; app stays healthy.
- [ ] Corrupt JSON (`"bg": `) → that theme is skipped with a `not valid JSON` warning; every other theme still loads.
- [ ] A file named `Graphite.json` **is** the built-in seed file (same id on a case-insensitive filesystem) — it must be edited in place, not appear twice.
- [ ] A 0.5.0 `<id>.md` theme (drop one in beside the JSON files) → launch: converted to `<id>.json` with the same colours and name, the old file kept as `<id>.md.migrated`, one `converted` line in the log, no duplicate entry in the dropdown.
- [ ] Very long theme names in the dropdown look fine.

## 14. RAM / preload behaviour

- [x] Settings → Playback → **Show RAM state ON** → cards show chips; toolbar pill shows counts; all reflect **actual** state (loaded vs streaming), not intent.
- [x] Add a **> 30 s** file with a hotkey → plays streamed; log says why it isn't preloaded; chip consistent.
- [ ] Add **17+ hotkey sounds** → cache stops at 16 sounds / 32 MB; the excess streams and plays fine; log explains; LRU = least-recently-used gets evicted first; **playing sound is never evicted**. [untested ]
- [ ] **Skip-and-continue (B1)**: with the RAM pill, load a batch that contains **one** file that can never fit (a 45 s file, or one bigger than the 32 MB cache) → the rest of the batch still preloads (chips fill), only the over-budget sound stays on disk, and it is logged **once** — re-asserting the same retention (e.g. clicking the same tab again) does not re-decode or re-log it; only a real change of the per-sound RAM flag retries.
- [ ] The refusal wording reads `"name" is too big; Streaming from disk` for the size caps, `"name" is too long; Streaming from disk` for the >30 s case, and `"name" has no audio` for an empty/corrupt file (B2).
- [x] Context menu RAM block (hotkey sounds): **Keep in RAM** / **Play from disk** / **Reload in RAM** do what they say; chip updates.
- [x] Switch tabs → other tab's sounds drop out of RAM (chip/pill counts change).

## 15. Modals & keyboard matrix

- [x] All modals (capture, rename, delete library, set gain, import confirm, reset confirm, activity log): transparent backdrop (board visible, input blocked, **no dim**), red-hover X, backdrop click cancels, Esc cancels.
- [x] Esc ladder: open a menu → Esc closes it; then Settings → Esc leaves; then search → Esc clears; then (with audio) Esc stops playback.
- [x] Activity log: shows load/device/save/hotkey events; capped at 200 entries; **identical consecutive messages collapse** into one refreshed entry; timestamps look right.
- [x] Shortcuts table from README works as documented (Ctrl+O/F/L/,/Z/Q, Delete, Space, F2, Esc; `Ctrl+Tab`/`Ctrl+Shift+Tab` only with the opt-in on).

## 16. Persistence & recovery

- [x] After all the above: quit, relaunch → **everything** survives (tabs, sounds, order, names, hotkeys, gains, volumes, layout, theme, loop config, device choices, settings toggles).
- [x] `libraries.json`/`settings.json` are pretty, valid JSON with `version: 2`; a `.bak` exists after saves.
- [ ] Task-kill the app (`taskkill /F`) while dirty → relaunch: config still loads (recovery from tmp/bak if needed), board intact.
- [x] Hand-corrupt `settings.json` (write garbage) → launch: defaults with warning logged, original preserved as `settings.json.corrupt` (numbered `.corrupt.2` if one exists).
- [x] Export/import round-trip after a session with real content → identical board (Section 12 covered this; re-check with final state).
- [ ] **Trim range survives an upgrade**: hand-edit a sound's `gain_db` to `-20` in `libraries.json`, relaunch → the value loads clamped at `-12.0 dB` (shown as `-12.0 dB`) and the sound still plays; the next save writes the clamped number. Anything in `-12…+12` is kept as-is.
- [x] **A missing sound keeps its last name across restarts** — the board stores the name, not the file; the card still reads `Airhorn` when `airhorn.wav` is gone. *(Intended, no change.)*
- [ ] **File lock** (wording fix, not a bug): with the app running, try to rename or delete a configured audio file in Explorer → Windows refuses. Remove the sound from the board (or quit) and the rename works. This is FR-5.3.15, not a defect.

## 17. Idle & performance sanity

- [x] Idle 2 minutes visible: Task Manager CPU ≈ 0 %, memory stable (~70 MB + cache).
- [-] Hidden 2 minutes: same or lower; no repaint flicker (check `--stats` if curious).
- [ ] Play 20 different sounds in a row → memory plateaus (cache caps hold), no leaks, all later plays still instant for RAM-resident ones.
- [x] UI stays smooth while a long stream plays (search, scroll, rename during playback).

## 18. Stress & edges

- [x] Hotkey spam (10×/sec for 10 s) → exclusive playback keeps up, no clicks, no stuck voice, no stuck hotkey afterwards.
- [x] Switch audio device **while** playing; change volumes mid-play; toggle link mid-drag.
- [x] ~2-minute file streams to the end correctly (progress reaches 100 %, voice stops).
- [ ] A > 20-minute file → rejected with a clear message, app healthy.
- [x] Rename to an existing name (duplicates allowed?), rename to spaces only, rapid renames.
- [x] Window at 860×500: sidebar compacts (labels/dB scale hide), nothing clipped; maximize at 1920×1080+: settings column readable, board list centred (1100 px cap).
- [ ] 150 % / 200 % Windows scaling: everything usable (not pixel-perfect per SPEC). [untested ]

---