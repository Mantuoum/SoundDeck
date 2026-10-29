#Requires -Version 5.1
<#
.SYNOPSIS
    Generates SoundDeck QA test material (audio, themes, fixtures) into test-material/.

.DESCRIPTION
    Everything here is synthetic and deterministic, so the folder can be regenerated at
    any time. The folder is gitignored; this script is the committed source of truth.

    Covers the scenarios in plan.md "QA round 2" plus the current iteration's repro
    cases (long-name Explorer bug, gain boost/trim, theme reload, RAM budget, streaming,
    decode rejection, format coverage).

.EXAMPLE
    .\scripts\make-test-material.ps1
#>
[CmdletBinding()]
param(
    [string]$OutDir = ''
)

$ErrorActionPreference = 'Stop'
if (-not $OutDir) { $OutDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'test-material' }

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Text;

public static class WavGen
{
    private static void Header(BinaryWriter w, int rate, short ch, short bits, int dataLen)
    {
        w.Write(Encoding.ASCII.GetBytes("RIFF"));
        w.Write(36 + dataLen);
        w.Write(Encoding.ASCII.GetBytes("WAVE"));
        w.Write(Encoding.ASCII.GetBytes("fmt "));
        w.Write(16);                 // fmt chunk size
        w.Write((short)1);           // PCM
        w.Write(ch);
        w.Write(rate);
        w.Write(rate * ch * bits / 8);      // byte rate
        w.Write((short)(ch * bits / 8));    // block align
        w.Write(bits);
        w.Write(Encoding.ASCII.GetBytes("data"));
        w.Write(dataLen);
    }

    private static void ToneBody(BinaryWriter w, int frames, int rate, short ch,
                                 double[] freqs, double amp, bool sweep, double sweepTo)
    {
        for (int i = 0; i < frames; i++)
        {
            double t = (double)i / rate;
            for (int c = 0; c < ch; c++)
            {
                double f = sweep ? freqs[c] + (sweepTo - freqs[c]) * ((double)i / frames) : freqs[c];
                double v = Math.Sin(2.0 * Math.PI * f * t) * amp;
                w.Write((short)Math.Round(v * 32767.0));
            }
        }
    }

    public static void Tone(string path, double seconds, int rate, short ch, double freq, double amp)
    {
        int frames = (int)(seconds * rate);
        double[] freqs = new double[ch];
        for (int c = 0; c < ch; c++) freqs[c] = freq;
        using (FileStream fs = new FileStream(path, FileMode.Create))
        using (BinaryWriter w = new BinaryWriter(fs))
        {
            Header(w, rate, ch, 16, frames * ch * 2);
            ToneBody(w, frames, rate, ch, freqs, amp, false, freq);
        }
    }

    public static void Sweep(string path, double seconds, int rate, short ch, double from, double to, double amp)
    {
        int frames = (int)(seconds * rate);
        double[] freqs = new double[ch];
        for (int c = 0; c < ch; c++) freqs[c] = from;
        using (FileStream fs = new FileStream(path, FileMode.Create))
        using (BinaryWriter w = new BinaryWriter(fs))
        {
            Header(w, rate, ch, 16, frames * ch * 2);
            ToneBody(w, frames, rate, ch, freqs, amp, true, to);
        }
    }

    public static void PerChannel(string path, double seconds, int rate, short ch, double baseFreq, double step, double amp)
    {
        int frames = (int)(seconds * rate);
        double[] freqs = new double[ch];
        for (int c = 0; c < ch; c++) freqs[c] = baseFreq + step * c;
        using (FileStream fs = new FileStream(path, FileMode.Create))
        using (BinaryWriter w = new BinaryWriter(fs))
        {
            Header(w, rate, ch, 16, frames * ch * 2);
            ToneBody(w, frames, rate, ch, freqs, amp, false, baseFreq);
        }
    }

    public static void Noise(string path, double seconds, int rate, short ch, double amp)
    {
        int frames = (int)(seconds * rate);
        Random rng = new Random(1234);
        using (FileStream fs = new FileStream(path, FileMode.Create))
        using (BinaryWriter w = new BinaryWriter(fs))
        {
            Header(w, rate, ch, 16, frames * ch * 2);
            for (int i = 0; i < frames; i++)
            {
                short s = (short)((rng.NextDouble() * 2.0 - 1.0) * amp * 32767.0);
                for (int c = 0; c < ch; c++) w.Write(s);
            }
        }
    }

    // Fast silence writer (used for the >20-minute rejection file).
    public static void Silence(string path, double seconds, int rate, short ch)
    {
        int frames = (int)(seconds * rate);
        int dataLen = frames * ch * 2;
        using (FileStream fs = new FileStream(path, FileMode.Create))
        using (BinaryWriter w = new BinaryWriter(fs))
        {
            Header(w, rate, ch, 16, dataLen);
            byte[] zeros = new byte[65536];
            int remaining = dataLen;
            while (remaining > 0)
            {
                int n = Math.Min(remaining, zeros.Length);
                w.Write(zeros, 0, n);
                remaining -= n;
            }
        }
    }
}
'@

Write-Host "==> Output: $OutDir"
if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory -Path $OutDir | Out-Null
foreach ($d in 'tones', 'formats', 'themes', 'fixtures') {
    New-Item -ItemType Directory -Path (Join-Path $OutDir $d) | Out-Null
}

$R = 48000
$S = 2

Write-Host '==> Tones (20 x 5 s, 48 kHz stereo) for the RAM budget / 17+ sounds test'
for ($i = 1; $i -le 20; $i++) {
    $freq = 300.0 + ($i - 1) * 60.0
    [WavGen]::Tone((Join-Path $OutDir ('tones\tone-{0:D2}.wav' -f $i)), 5.0, $R, $S, $freq, 0.35)
}

Write-Host '==> Streaming, edge lengths, formats, channels'
[WavGen]::Tone((Join-Path $OutDir 'long-45s.wav'), 45.0, $R, $S, 440.0, 0.35)          # > 30 s: streams, never preloads
[WavGen]::Silence((Join-Path $OutDir 'long-20min.wav'), 1205.0, 8000, 1)              # > 20 min: decoder must reject
[WavGen]::Tone((Join-Path $OutDir 'short-0.3s.wav'), 0.3, $R, $S, 880.0, 0.5)          # very short sound
[WavGen]::Tone((Join-Path $OutDir 'mono-22k.wav'), 5.0, 22050, 1, 523.0, 0.4)          # mono + odd rate: resampler path
[WavGen]::Tone((Join-Path $OutDir 'stereo-44k.wav'), 5.0, 44100, 2, 440.0, 0.4)        # 44.1 kHz: resampler path
[WavGen]::Tone((Join-Path $OutDir 'loop-8s.wav'), 8.0, $R, $S, 440.0, 0.35)            # 440 Hz x 8 s = exact wrap
[WavGen]::Sweep((Join-Path $OutDir 'sweep-5s.wav'), 5.0, $R, $S, 200.0, 3000.0, 0.3)    # audible resample artefacts
[WavGen]::Noise((Join-Path $OutDir 'noise-10s.wav'), 10.0, $R, $S, 0.3)                # dense material for underruns
[WavGen]::Tone((Join-Path $OutDir 'quiet-3s.wav'), 3.0, $R, $S, 440.0, 0.02)           # gain +12 dB boost test
[WavGen]::Tone((Join-Path $OutDir 'loud-3s.wav'), 3.0, $R, $S, 440.0, 0.99)            # gain -12 dB trim / clip test
[WavGen]::PerChannel((Join-Path $OutDir 'surround-6ch.wav'), 3.0, $R, 6, 300.0, 100.0, 0.3) # >2 channels source

Write-Host '==> Broken inputs'
Set-Content -Path (Join-Path $OutDir 'corrupt.wav') -Value 'this is not a wav file, on purpose' -NoNewline
[System.IO.File]::WriteAllBytes((Join-Path $OutDir 'empty.wav'), [byte[]]@())

Write-Host '==> Naming cases (Explorer / truncation / quoting)'
Copy-Item (Join-Path $OutDir 'short-0.3s.wav') (Join-Path $OutDir 'weird name, with spaces & commas.wav')
Copy-Item (Join-Path $OutDir 'short-0.3s.wav') (Join-Path $OutDir 'emoji-tone.wav')
$longBase = ('long-name-' + ('a' * 150))
Copy-Item (Join-Path $OutDir 'short-0.3s.wav') (Join-Path $OutDir ($longBase + '.wav'))

Write-Host '==> Format variants (ffmpeg)'
$ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if ($ffmpeg) {
    $src = Join-Path $OutDir 'tones\tone-01.wav'
    & $ffmpeg -y -loglevel error -i $src (Join-Path $OutDir 'formats\tone.mp3')
    & $ffmpeg -y -loglevel error -i $src (Join-Path $OutDir 'formats\tone.flac')
    & $ffmpeg -y -loglevel error -i $src (Join-Path $OutDir 'formats\tone.ogg')
    & $ffmpeg -y -loglevel error -i $src (Join-Path $OutDir 'formats\tone.m4a')
} else {
    Write-Warning 'ffmpeg not found - format variants skipped (WAVs still generated)'
}

Write-Host '==> Theme files'
$graphite = @'
# Broken Hex

| token | value |
| --- | --- |
| bg | #101014 |
| panel | #17171d |
| text | #e8e8ee |
| accent | banana |
| border | nope |
'@
[System.IO.File]::WriteAllText((Join-Path $OutDir 'themes\broken-hex.md'), $graphite, (New-Object System.Text.UTF8Encoding($false)))

$missing = @'
# Missing Tokens

| token | value |
| --- | --- |
| bg | #101014 |
| text | #e8e8ee |
'@
[System.IO.File]::WriteAllText((Join-Path $OutDir 'themes\missing-tokens.md'), $missing, (New-Object System.Text.UTF8Encoding($false)))

$mine = @'
# Graphite (mine)

| token | value |
| --- | --- |
| bg | #141210 |
| panel | #1d1a16 |
| panel_deep | #17140f |
| card | #221e19 |
| card_hover | #2a251e |
| field | #1b1813 |
| border | #3a332a |
| border_soft | #2a251e |
| text | #f0e9dd |
| muted | #a89c8a |
| faint | #6f6659 |
| accent | #ff9d3c |
| purple | #b98bff |
| amber | #ffc857 |
| red | #ff6b6b |
| light | false
'@
[System.IO.File]::WriteAllText((Join-Path $OutDir 'themes\Graphite.md'), $mine, (New-Object System.Text.UTF8Encoding($false)))

$longTheme = @'
# A Theme With A Very Long Name To Stress The Dropdown Width

| token | value |
| --- | --- |
| bg | #0d1b12 |
| panel | #14251a |
| text | #e6f5ea |
| accent | #43d17c |
'@
[System.IO.File]::WriteAllText((Join-Path $OutDir 'themes\long-theme-name-that-keeps-going-and-going-past-the-dropdown-width.md'), $longTheme, (New-Object System.Text.UTF8Encoding($false)))

Write-Host '==> Config / bundle fixtures'
[System.IO.File]::WriteAllText((Join-Path $OutDir 'fixtures\settings-corrupt.json'), 'this is not json {{{', (New-Object System.Text.UTF8Encoding($false)))
$newer = '{"app":"sounddeck","bundle":99,"settings":{},"libraries":{"version":2,"libraries":[]}}'
[System.IO.File]::WriteAllText((Join-Path $OutDir 'fixtures\bundle-newer.json'), $newer, (New-Object System.Text.UTF8Encoding($false)))

$readme = @'
# SoundDeck test material

Generated by `scripts\make-test-material.ps1` (gitignored; regenerate any time).
All audio is synthetic 16-bit PCM WAV unless noted.

| File | Use |
| --- | --- |
| `tones\tone-01..20.wav` | 20 x 5 s tones: RAM budget caps (16 sounds / 32 MB), LRU, never-evict-playing, "17+ hotkey sounds" |
| `long-45s.wav` | > 30 s: must stream, never preload (B1/B2 messages) |
| `long-20min.wav` | > 20 min: decoder must reject with a clear message |
| `short-0.3s.wav` | very short sound: fades, progress, loop wrap |
| `mono-22k.wav` | mono + 22.05 kHz: resampler mono/rate path |
| `stereo-44k.wav` | 44.1 kHz stereo: resampler rate path |
| `loop-8s.wav` | exact 8 s loop point (440 Hz x 8 s): gapless-ish wrap check |
| `sweep-5s.wav` | audible resampling artefacts if the curve is wrong |
| `noise-10s.wav` | dense material: underrun/back-pressure check while streaming |
| `quiet-3s.wav` | gain dialog boost (+12 dB) audibility |
| `loud-3s.wav` | gain dialog trim (-12 dB) / master clipping |
| `surround-6ch.wav` | >2-channel source: channel policy (L/R used, extras dropped) |
| `corrupt.wav` | decode failure path (not a WAV) |
| `empty.wav` | zero-byte file: decoder rejection |
| `weird name, with spaces & commas.wav` | quoting in dialogs / Show in Explorer |
| `emoji-tone.wav` | non-ASCII filename handling |
| `long-name-<150 a>.wav` | truncation in cards + "Show in Explorer" long-name bug (B3) |
| `formats\tone.{mp3,flac,ogg,m4a}` | format coverage (ffmpeg required to regenerate) |
| `themes\broken-hex.md` | invalid hex fallback + warning |
| `themes\missing-tokens.md` | missing-token fallback + warning |
| `themes\Graphite.md` | user theme named like the built-in: shows alongside (settled) |
| `themes\long-theme-name-...md` | dropdown width stress |
| `fixtures\settings-corrupt.json` | corrupt-config recovery (.corrupt snapshot, defaults, warning) |
| `fixtures\bundle-newer.json` | import rejection: newer bundle version |

Copy the `themes\*.md` files into `<app>\config\themes\` to test; copy a fixture over
`config\settings.json` (with the app closed) to test recovery. Audio files can be added
from anywhere - the app references them, never copies them.
'@
[System.IO.File]::WriteAllText((Join-Path $OutDir 'README.md'), $readme, (New-Object System.Text.UTF8Encoding($false)))

Write-Host ''
Write-Host '==> Summary'
$total = (Get-ChildItem $OutDir -Recurse -File | Measure-Object Length -Sum).Sum
Write-Host ('    {0} files, {1:N1} MB in {2}' -f (Get-ChildItem $OutDir -Recurse -File).Count, ($total / 1MB), $OutDir)
