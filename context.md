# SoundDeck — Session Handoff

Date: 2026-10-26
Repo: `C:\My files\Open code projects\Model testing` (git, branch `master`)
App: `sounddeck/` — Rust + eframe/egui 0.31.1, WASAPI, portable, no Electron

## Where things stand

- Last commit: `1e8dc72 docs: retire shipped phase specs, record the looping track`.
- Working tree: `plan.md` rewritten for audio latency, loop simplification + volume,
  volume consistency, full-window Settings, and developer docs (uncommitted);
  `context.md` updated. Untracked `References/`.
- Tests: 33 passing; release build deployed to `sounddeck\review\SoundDeck.exe`.
- The review app uses the user's real config and audio files. Never overwrite
  `sounddeck\review\config\` with demo data.
- Storage, per-file themes, motion, the UI polish batch, and the looping track are
  shipped. The next pass is the latency/meter fix, then loop simplification, volume
  consistency, Settings, and `ARCHITECTURE.md`.

## Source of truth

Read `plan.md` before touching audio or UI. The shipped baseline is summarized there;
section 1 is the looping-track spec with locked decisions, edge cases, tests and the
implementation order. Do not re-litigate settled decisions.

Useful files:

- `sounddeck\README.md` — features, shortcuts, config/theme notes, diagnostics.
- `sounddeck\src\app.rs` — UI, actions, sidebar, cards/rows, settings.
- `sounddeck\src\audio\engine.rs` — mixer, exclusive playback, meters, device routes.
- `sounddeck\src\audio\decode.rs` — one-shot decode to 48 kHz stereo.
- `sounddeck\src\audio\stream.rs` — loop decoder thread (new).
- `sounddeck\src\playback.rs` — RAM/disk sound registry.
- `sounddeck\src\hotkeys.rs` — global hotkeys, tray, window focus.
- `sounddeck\src\storage\mod.rs` — settings/libraries schema, normalize.
- `sounddeck\src\ui\theme.rs` — palettes, radii, fonts, truncation helper.

## Working rules

- Commit every major change; one feature commit plus a docs commit is the current plan.
- The user is often using the machine. Verify visually with `PrintWindow` +
  `SetProcessDPIAware`; never steal foreground focus. Do not leave test instances
  running.
- Never rewrite the user's review config or replace it with demo seeds.
- Two copies from different exe paths fight over the same hotkeys. Run only one.
- Portable: paths relative to the exe where possible, no AppData.
- No database. JSON for data, Markdown for themes. Audio files stay referenced, not
  copied.
- The shared theme reference at `C:\My files\My Resources\themes.md` is not read by the
  app; do not edit it for app-specific changes.
- PowerShell 5.1 `Set-Content -Encoding utf8` writes a BOM. For JSON use
  `[System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))`.
- `app.rs` mixes CRLF and LF; the `edit` tool handles both, .NET `Replace` with LF-only
  strings does not.
- egui 0.31 `Margin` fields are `i8`, not `f32`.

## Current behaviors worth knowing

- Libraries are tabs in the title bar; only the active tab's hotkeys are registered and
  only its hotkey sounds stay in RAM. Switching tabs stops sound playback.
- Triggering a sound fades out any current voice, including a retrigger of itself.
- Volume sliders are logarithmic (`-60…0 dBFS`); percentages show slider position.
- Master gain is `0…+12 dB`; right-click resets it to `0.0 dB`.
- Minimize hides to tray; close/Alt+F4 quits. Single instance per exe path.
- The sidebar compacts below 620 px height: no dB scale, no preload row, no credit.
  When the loop card is shown, the Monitor/Cable captions also collapse.
- The looping track is streamed from disk, follows Monitor+Cable, plays under sounds,
  survives tab switches and `Stop all`, and starts stopped on launch. The card is
  pinned above Settings; enable it in Settings → Playback.
- The icon is the blue accent version.

## Sensitive data

No credentials or API keys. The review config references the user's personal audio files
outside the project; treat those paths as private and do not paste them into commits,
docs or reports.

## Next steps

1. Implement `plan.md` in its rollout order:
   - consumer-side metering + 0.15 s output ring,
   - simplify the loop stream (3 states, 16-chunk buffer) + `loop_volume`,
   - one volume curve/behaviour (keep current visuals),
   - full-window tabbed Settings,
   - `ARCHITECTURE.md` written against the final code.
2. Deploy after each UI-visible commit and verify latency by ear.
3. Replace the developer placeholders once the user supplies name, role, link and blurb.
