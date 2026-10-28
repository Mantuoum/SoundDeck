# SoundDeck

A small, portable Windows soundboard: two independent outputs, global hotkeys
that still reach the focused app, and a streamed loop track. No installer, no
services, no telemetry.

## Download

Grab `SoundDeck-<version>-win64.zip` from
[Releases](https://github.com/Mantuoum/SoundDeck/releases), extract it anywhere,
and run `SoundDeck.exe`.

## What it does

- **Two outputs** — Monitor and Cable, each with its own device and volume, plus a
  global dB master gain. Sound effects go to both; only the loop track is routed per
  output.
- **Global hotkeys** — low-level keyboard hook that passes the key through, so
  the game or app you are in still receives it. Bare keys (F1, `, NumPad5) are
  allowed. A combination belongs to one sound only; Win chords work, but Windows acts
  on them as well.
- **Loop track** — streamed from disk, so a one-hour file costs no RAM.
- **Libraries** — grid or list, drag to reorder, search, per-sound volume and a
  per-sound dB trim (`-24` to `+12 dB`) for files that are too loud or too quiet next
  to the rest. Deleting a library asks whether its sounds move to another library or
  leave the board.
- **Tray** — minimise to tray; sounds keep working while hidden.
- **Backup** — export settings and libraries to one JSON file, and import it on
  another PC. Audio files stay where they are.
- **Portable** — `config/` is created next to the executable. Copy the folder to
  move your setup.

## Requirements

- Windows 10 or 11, 64-bit.
- Any WASAPI output device. For routing into voice chat, install a virtual cable
  and pick it as the Cable output. If a chosen device is unplugged, that output falls
  back to the Windows default and names the substitute in the activity log; plug it
  back in and use **Refresh devices** to return to it.

## Build from source

```powershell
git clone https://github.com/Mantuoum/SoundDeck.git
cd SoundDeck
rustup default stable
cargo build --release --manifest-path sounddeck/Cargo.toml
```

Package a release archive (tests, binary, zip, checksum):

```powershell
.\scripts\package.ps1
```

Output lands in `dist/`. The same script runs in CI on a `v*` tag, so a local
package and a released one are built the same way.

## Layout

| Path | Contents |
| --- | --- |
| `sounddeck/` | The crate: UI, mixer, WASAPI, hotkeys, storage |
| `sounddeck/README.md` | User manual |
| `sounddeck/ARCHITECTURE.md` | Module map and invariants |
| `SPEC.md` | Behavioural contract and performance budgets |
| `scripts/package.ps1` | Release packaging |
| `scripts/measure.ps1` | Performance/memory measurement pass |

## License

MIT — see [LICENSE](LICENSE).

The bundled Satoshi typeface is licensed under the SIL Open Font License; see
`sounddeck/assets/Satoshi-FFL.txt`. It is included in the release archive.

## Credits

Built by [Mantum](https://github.com/Mantuoum). Inspired by
[Soundux](https://github.com/Soundux/Soundux).
