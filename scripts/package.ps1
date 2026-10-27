#Requires -Version 5.1
<#
.SYNOPSIS
    Builds SoundDeck and produces the portable release archive.

.DESCRIPTION
    Runs the test suite, builds the optimized binary, then writes

        dist/SoundDeck-<version>-win64.zip
        dist/SoundDeck-<version>-win64.zip.sha256

    The archive contains SoundDeck.exe, README.md, LICENSE and the bundled
    font licence at its root, so it can be extracted into any folder and run.

    .github/workflows/release.yml calls this same script, so a local run and a
    tagged CI run produce an identical layout. Nothing here talks to GitHub.

.PARAMETER Version
    Version used in the archive name. Defaults to the version in Cargo.toml.

.PARAMETER SkipTests
    Skip `cargo test`. Only for a quick local artifact; CI never sets this.

.PARAMETER SkipBuild
    Reuse the existing target/release/sounddeck.exe instead of rebuilding.

.EXAMPLE
    .\scripts\package.ps1

.EXAMPLE
    .\scripts\package.ps1 -Version 0.2.0 -SkipBuild
#>
[CmdletBinding()]
param(
    [string]$Version = '',
    [switch]$SkipTests,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Write-Step {
    param([string]$Message)
    Write-Host ''
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Invoke-Cargo {
    param([string[]]$Arguments)

    & cargo @Arguments '--manifest-path' $script:manifest
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed (exit code $LASTEXITCODE)"
    }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$crateDir = Join-Path $repoRoot 'sounddeck'
$manifest = Join-Path $crateDir 'Cargo.toml'
$distDir = Join-Path $repoRoot 'dist'
$binary = Join-Path $crateDir 'target\release\sounddeck.exe'

if (-not (Test-Path -LiteralPath $manifest)) {
    throw "Cargo.toml not found at $manifest"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'cargo is not on PATH. Install Rust from https://rustup.rs and reopen the shell.'
}

if (-not $Version) {
    $text = Get-Content -Raw -LiteralPath $manifest
    $match = [regex]::Match($text, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $match.Success) {
        throw 'Could not read [package] version from Cargo.toml. Pass -Version explicitly.'
    }
    $Version = $match.Groups[1].Value
}
if ($Version -notmatch '^\d+\.\d+\.\d+([-.+][0-9A-Za-z.-]+)?$') {
    throw "Version '$Version' is not a valid SemVer string."
}

Write-Host "SoundDeck $Version" -ForegroundColor Green
Write-Host "  crate    $crateDir"
Write-Host "  output   $distDir"

if (-not $SkipTests) {
    Write-Step 'Running tests'
    Invoke-Cargo @('test')
}

if (-not $SkipBuild) {
    Write-Step 'Building release binary'
    Invoke-Cargo @('build', '--release')
}

if (-not (Test-Path -LiteralPath $binary)) {
    throw "Expected binary missing: $binary (run without -SkipBuild)"
}

$stageRoot = Join-Path $distDir 'stage'
$packageDir = Join-Path $stageRoot "SoundDeck-$Version"
if (Test-Path -LiteralPath $stageRoot) {
    Remove-Item -LiteralPath $stageRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $packageDir -Force | Out-Null

Write-Step 'Staging package contents'
Copy-Item -LiteralPath $binary -Destination (Join-Path $packageDir 'SoundDeck.exe')
Copy-Item -LiteralPath (Join-Path $crateDir 'README.md') -Destination (Join-Path $packageDir 'README.md')
Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination (Join-Path $packageDir 'LICENSE')
# The font ships inside the binary under the SIL Open Font License; the licence
# text has to travel with it.
Copy-Item -LiteralPath (Join-Path $crateDir 'assets\Satoshi-FFL.txt') -Destination (Join-Path $packageDir 'Satoshi-FFL.txt')

$entries = Get-ChildItem -LiteralPath $packageDir -File | Sort-Object Name
foreach ($entry in $entries) {
    Write-Host ("    {0,-20} {1,9:N1} KB" -f $entry.Name, ($entry.Length / 1KB))
}

if (-not (Test-Path -LiteralPath $distDir)) {
    New-Item -ItemType Directory -Path $distDir -Force | Out-Null
}

$zipName = "SoundDeck-$Version-win64.zip"
$zipPath = Join-Path $distDir $zipName
if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
}

Write-Step "Writing $zipName"
Compress-Archive -Path (Join-Path $packageDir '*') -DestinationPath $zipPath -CompressionLevel Optimal

$zip = Get-Item -LiteralPath $zipPath
$hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
$hashPath = "$zipPath.sha256"
[System.IO.File]::WriteAllText($hashPath, "$hash  $zipName`n")

Remove-Item -LiteralPath $stageRoot -Recurse -Force

Write-Step 'Done'
Write-Host ("  {0}  {1:N1} MB" -f $zip.FullName, ($zip.Length / 1MB))
Write-Host ("  sha256  {0}" -f $hash)
Write-Host "  $hashPath"
