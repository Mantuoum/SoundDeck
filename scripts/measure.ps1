#Requires -Version 5.1
<#
.SYNOPSIS
    Runs the SPEC section 8.1 / 8.2 measurement pass against one SoundDeck build.

.DESCRIPTION
    Methodology (SPEC NFR-8.1 "performance budgets", NFR-8.2 "memory"):

      1. Preflight. The measurement needs a quiet machine, and two instances fight
         over the keyboard hook. ANY SoundDeck process or SoundDeck-titled window
         (a review instance, a leftover tray icon) is refused with a list, never
         killed - close it yourself and rerun. (plan.md run-only-one rule.)

      2. Launch `<exe> --stats`. The counter thread appends one `[stats]` line to
         %TEMP%\sounddeck-stats.log every five seconds (stats.rs). The log length
         is recorded before launch, so only lines written by THIS run are read;
         lines are additionally filtered by their `[stats]` prefix.

      3. Warm-up (5 s, not measured): tray icon, font atlas, first config save.

      4. Visible idle phase (60 s). The window opens normally and is left alone -
         no input, no tab switch, no mouse over the window. Measured:
           - CPU: TotalProcessorTime delta / wall time -> % of one core (NFR-8.1.1)
           - Private working set, sampled every 2 s via the PerfProc CIM class
             (NFR-8.2.1; falls back to total working set if the perf counters are
             disabled, and says so instead of pretending)
           - fps / frame time / repaint interval / mixer wakeups / ring underruns
             from the `[stats]` lines (NFR-8.1.3, NFR-8.1.4)

      5. Hidden phase (60 s). The script calls ShowWindow(SW_MINIMIZE) on the main
         window. SoundDeck only enters its hidden state (no painting, 1 s timer)
         when Settings -> Window -> "Minimize to tray" is ON and the tray is
         available; with the toggle off the window is merely minimized and the
         `[stats]` lines keep reporting `hidden=0`. In that case the script prints
         a MANUAL STEP note and reports NFR-8.1.2 as not measured - it does not
         print a minimized-window number as if it were the hidden figure.

      6. Shutdown. The instance this script started is closed with WM_CLOSE
         (`CloseMainWindow`), waited on for 15 s, and force-killed only as a last
         resort. It never leaves a process running.

     7. Results. Prints the per-phase numbers with their NFR id and budget, then a
        compact markdown block to paste into the SPEC 8.1 / 8.2 "measured" columns.

    Deliberately NOT automated: playback CPU (NFR-8.1.5), trigger latency
    (NFR-8.1.7) and the retained-audio caps (NFR-8.2.6) need a library with
    hotkeys and a real trigger; `--diag <file>` covers the audio path by hand.
    This script measures the visible/hidden floor only.

    The run uses whatever config sits beside the executable (the app is portable
    and writes `config/` next to the exe), so a build with a normal library
    measures that library.

.PARAMETER Exe
    Executable to measure. Default: sounddeck\target\release\sounddeck.exe.

.PARAMETER VisibleSeconds
    Length of the visible-idle phase. Default 60.

.PARAMETER HiddenSeconds
    Length of the hidden phase. Default 60.

.PARAMETER WarmupSeconds
    Startup settle time, not measured. Default 5.

.PARAMETER SkipHidden
    Skip the hidden phase entirely (visible idle only).

.EXAMPLE
    .\scripts\measure.ps1

.EXAMPLE
    .\scripts\measure.ps1 -Exe .\review\SoundDeck.exe -SkipHidden
#>
[CmdletBinding()]
param(
    [string]$Exe = '',
    [int]$VisibleSeconds = 60,
    [int]$HiddenSeconds = 60,
    [int]$WarmupSeconds = 5,
    [switch]$SkipHidden
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# SPEC budgets this pass checks (one source for the printed PASS/OVER flags).
$BudgetVisibleCpu = 1.0    # NFR-8.1.1, % of one core
$BudgetHiddenCpu = 0.3     # NFR-8.1.2, % of total CPU
$BudgetIdleFps = 1.0       # NFR-8.1.3, frames per second
$BudgetFrameMs = 1.0       # NFR-8.1.3, ms per idle frame
$BudgetMixerWakeups = 25.0 # NFR-8.1.4, per second
$BudgetPrivateMb = 100.0   # NFR-8.2.1, MB private working set

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $Exe) {
    $Exe = Join-Path $repoRoot 'sounddeck\target\release\sounddeck.exe'
}
$Exe = [System.IO.Path]::GetFullPath($Exe)
$statsLog = Join-Path ([System.IO.Path]::GetTempPath()) 'sounddeck-stats.log'
$invariant = [System.Globalization.CultureInfo]::InvariantCulture

function Write-Step {
    param([string]$Message)
    Write-Host ''
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Note {
    param([string]$Message)
    Write-Host "    $Message"
}

function Format-Number {
    param([double]$Value, [string]$Pattern = '0.0')
    $Value.ToString($Pattern, $invariant)
}

function Format-Check {
    param([double]$Value, [double]$Budget)
    if ($Value -le $Budget) { 'PASS' } else { 'OVER BUDGET' }
}

# ---------------------------------------------------------------------------
# 1. Preflight - nothing else of this app may be alive.
# ---------------------------------------------------------------------------
Write-Step 'Preflight'
if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    throw "Executable not found: $Exe`n       Build it with 'cargo build --release' or pass -Exe <path>."
}

$blockers = New-Object System.Collections.ArrayList
foreach ($candidate in @(Get-Process -ErrorAction SilentlyContinue)) {
    if ($candidate.Id -eq $PID) { continue }
    try {
        $title = [string]$candidate.MainWindowTitle
    } catch {
        $title = ''
    }
    try {
        $name = [string]$candidate.ProcessName
    } catch {
        $name = ''
    }
    $isSoundDeck = ($name -like '*sounddeck*') -or ($title -like '*sounddeck*')
    if ($isSoundDeck) {
        [void]$blockers.Add(('    pid {0,-6} {1,-22} "{2}"' -f $candidate.Id, $name, $title))
    }
}
if ($blockers.Count -gt 0) {
    Write-Host '    Refusing to measure: SoundDeck is already running.' -ForegroundColor Red
    foreach ($line in $blockers) { Write-Host $line }
    throw 'Close every SoundDeck instance (including the tray icon) and rerun. The run-only-one rule exists because two instances contend for the keyboard hook and both the CPU and the memory numbers would be wrong.'
}
Write-Note "exe        $Exe"
Write-Note "stats log  $statsLog"

$startOffset = 0L
if (Test-Path -LiteralPath $statsLog) {
    $startOffset = (Get-Item -LiteralPath $statsLog).Length
}

# ---------------------------------------------------------------------------
# Helpers: Win32 minimize, process sampling, stats-log parsing.
# ---------------------------------------------------------------------------
Add-Type -Namespace SoundDeckMeasure -Name Native -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool ShowWindow(System.IntPtr hWnd, int nCmdShow);
[DllImport("user32.dll")] public static extern bool IsIconic(System.IntPtr hWnd);
'@

function Get-CpuSeconds {
    param([System.Diagnostics.Process]$Process)
    $Process.Refresh()
    return $Process.TotalProcessorTime.TotalSeconds
}

# Private working set in MB, or $null when the PerfProc class is unavailable.
# `WorkingSetPrivate` on the RawData class is a raw counter in BYTES (the
# formatted class reports the same value), so it divides by 1 MB, not by 1024.
function Get-PrivateWorkingSetMb {
    param([int]$ProcessId)
    try {
        $row = Get-CimInstance -ClassName Win32_PerfRawData_PerfProc_Process `
            -Filter "IDProcess=$ProcessId" -ErrorAction Stop | Select-Object -First 1
        if ($null -ne $row) {
            $property = $row.PSObject.Properties['WorkingSetPrivate']
            if ($null -ne $property) {
                return [double]$property.Value / 1MB
            }
        }
    } catch {
        return $null
    }
    return $null
}

function Measure-Phase {
    param(
        [System.Diagnostics.Process]$Process,
        [string]$Name,
        [int]$Seconds
    )
    $cpuStart = Get-CpuSeconds -Process $Process
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $privateSamples = New-Object System.Collections.Generic.List[double]
    $totalSamples = New-Object System.Collections.Generic.List[double]
    while ($clock.Elapsed.TotalSeconds -lt $Seconds) {
        Start-Sleep -Seconds 2
        if ($Process.HasExited) {
            throw "SoundDeck exited during the $Name phase (exit code $($Process.ExitCode))."
        }
        $private = Get-PrivateWorkingSetMb -ProcessId $Process.Id
        if ($null -ne $private) { [void]$privateSamples.Add($private) }
        $Process.Refresh()
        [void]$totalSamples.Add([double]$Process.WorkingSet64 / 1MB)
    }
    $cpuEnd = Get-CpuSeconds -Process $Process
    $wall = $clock.Elapsed.TotalSeconds
    if ($wall -le 0) { $wall = 0.001 }

    $privateMean = $null
    $privateMax = $null
    if ($privateSamples.Count -gt 0) {
        $privateMean = ($privateSamples | Measure-Object -Average).Average
        $privateMax = ($privateSamples | Measure-Object -Maximum).Maximum
    }
    $totalMean = $null
    if ($totalSamples.Count -gt 0) {
        $totalMean = ($totalSamples | Measure-Object -Average).Average
    }

    return [pscustomobject]@{
        Name          = $Name
        Seconds       = $wall
        CpuPercent    = (($cpuEnd - $cpuStart) / $wall) * 100.0
        PrivateMeanMb = $privateMean
        PrivateMaxMb  = $privateMax
        TotalMeanMb   = $totalMean
        PrivateCount  = $privateSamples.Count
    }
}

function Read-LogSlice {
    param([string]$Path, [long]$Start, [long]$End)
    if (-not (Test-Path -LiteralPath $Path)) { return @() }
    $length = (Get-Item -LiteralPath $Path).Length
    if ($End -gt $length) { $End = $length }
    if ($End -le $Start) { return @() }
    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    try {
        [void]$stream.Seek($Start, [System.IO.SeekOrigin]::Begin)
        $buffer = New-Object byte[] ([int]($End - $Start))
        $read = $stream.Read($buffer, 0, $buffer.Length)
        if ($read -le 0) { return @() }
        $text = [System.Text.Encoding]::UTF8.GetString($buffer, 0, $read)
    } finally {
        $stream.Dispose()
    }
    # A slice can start or end mid-line; the prefix filter drops those fragments.
    return @($text -split "`r?`n" | Where-Object { $_ -match '^\[stats\]' })
}

function ConvertFrom-StatsLines {
    param([string[]]$Lines)
    $samples = New-Object System.Collections.ArrayList
    foreach ($line in $Lines) {
        $fields = @{}
        foreach ($match in [regex]::Matches($line, '(?<k>[a-z][a-z0-9_]*)=(?<v>[-0-9.]+)')) {
            $fields[$match.Groups['k'].Value] =
                [double]::Parse($match.Groups['v'].Value, $invariant)
        }
        foreach ($match in [regex]::Matches($line, '(?<k>pop[0-9]+)=(?<c>[0-9]+)/(?<e>[0-9]+)')) {
            $fields[$match.Groups['k'].Value + '_empty'] =
                [double]::Parse($match.Groups['e'].Value, $invariant)
        }
        [void]$samples.Add($fields)
    }
    return @($samples)
}

function Get-StatMean {
    param([object[]]$Samples, [string]$Key)
    $sum = 0.0
    $count = 0
    foreach ($sample in $Samples) {
        if ($sample.ContainsKey($Key)) {
            $sum += $sample[$Key]
            $count++
        }
    }
    if ($count -eq 0) { return $null }
    return $sum / [double]$count
}

function Get-StatMax {
    param([object[]]$Samples, [string]$Key)
    $max = $null
    foreach ($sample in $Samples) {
        if ($sample.ContainsKey($Key)) {
            if (($null -eq $max) -or ($sample[$Key] -gt $max)) { $max = $sample[$Key] }
        }
    }
    return $max
}

function Format-Optional {
    param($Value, [string]$Pattern = '0.0', [string]$Suffix = '')
    if ($null -eq $Value) { return 'n/a' }
    return (Format-Number -Value $Value -Pattern $Pattern) + $Suffix
}

# ---------------------------------------------------------------------------
# 2. Launch and wait for the window.
# ---------------------------------------------------------------------------
$process = $null
$visiblePhase = $null
$hiddenPhase = $null
$visibleStart = 0L
$visibleEnd = 0L
$hiddenEnd = 0L
$hiddenReached = $false
$exitCode = $null

try {
    Write-Step 'Launch'
    $process = Start-Process -FilePath $Exe -ArgumentList '--stats' -PassThru
    Write-Note "pid $($process.Id)"

    $handle = [IntPtr]::Zero
    $waited = 0.0
    while ($waited -lt 30.0) {
        Start-Sleep -Milliseconds 500
        $waited += 0.5
        if ($process.HasExited) {
            throw "SoundDeck exited during startup (exit code $($process.ExitCode)). Run it by hand to see the error."
        }
        $process.Refresh()
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) {
            $handle = $process.MainWindowHandle
            break
        }
    }
    if ($handle -eq [IntPtr]::Zero) {
        throw 'No main window appeared within 30 s.'
    }

    Write-Step "Warm-up ($WarmupSeconds s, not measured)"
    Start-Sleep -Seconds $WarmupSeconds
    # Startup lines carry the font atlas, device enumeration and the first save;
    # they are not idle numbers, so the visible slice starts after the warm-up.
    $visibleStart = $startOffset
    if (Test-Path -LiteralPath $statsLog) { $visibleStart = (Get-Item -LiteralPath $statsLog).Length }

    Write-Step "Visible idle ($VisibleSeconds s)"
    $visiblePhase = Measure-Phase -Process $process -Name 'visible' -Seconds $VisibleSeconds
    $visibleEnd = (Get-Item -LiteralPath $statsLog).Length

    if (-not $SkipHidden) {
        Write-Step "Hidden ($HiddenSeconds s)"
        # SW_MINIMIZE (6). SoundDeck turns a real minimize into its hidden state
        # when "Minimize to tray" is on; otherwise this is just a minimized
        # window and the stats lines keep hidden=0 (reported as a manual step).
        $process.Refresh()
        $minimized = [SoundDeckMeasure.Native]::ShowWindow($process.MainWindowHandle, 6)
        Start-Sleep -Seconds 2
        $process.Refresh()
        $iconic = [SoundDeckMeasure.Native]::IsIconic($process.MainWindowHandle)
        if (-not $minimized -or -not $iconic) {
            Write-Note 'The window did not report as minimized; the hidden numbers are best-effort.'
        }
        $hiddenPhase = Measure-Phase -Process $process -Name 'hidden' -Seconds $HiddenSeconds
        $hiddenEnd = (Get-Item -LiteralPath $statsLog).Length
    } else {
        $hiddenEnd = $visibleEnd
        Write-Step 'Hidden phase skipped (-SkipHidden)'
    }

    # -----------------------------------------------------------------------
    # 3. Shut the instance down before reading the log.
    # -----------------------------------------------------------------------
    Write-Step 'Shutdown'
    if (-not $process.HasExited) {
        [void]$process.CloseMainWindow()
        if (-not $process.WaitForExit(15000)) {
            Write-Warning 'SoundDeck ignored WM_CLOSE after 15 s; forcing exit.'
            Stop-Process -Id $process.Id -Force
            [void]$process.WaitForExit(5000)
        }
    }
    $process.Refresh()
    if ($process.HasExited) { $exitCode = $process.ExitCode }
    Write-Note "closed (exit code $exitCode)"
} finally {
    # Never leave a measurement instance behind, whatever happened above.
    if ($null -ne $process) {
        try {
            $process.Refresh()
            if (-not $process.HasExited) {
                Write-Warning "Stopping leftover measurement instance (pid $($process.Id))."
                [void]$process.CloseMainWindow()
                if (-not $process.WaitForExit(5000)) {
                    Stop-Process -Id $process.Id -Force
                }
            }
        } catch {
            Write-Warning "Could not confirm the measurement instance exited: $($_.Exception.Message)"
        }
    }
}

# ---------------------------------------------------------------------------
# 4. Parse the stats lines this run produced.
# ---------------------------------------------------------------------------
$visibleSamples = @()
$hiddenSamples = @()
try {
    $visibleSamples = @(ConvertFrom-StatsLines -Lines @(Read-LogSlice -Path $statsLog -Start $visibleStart -End $visibleEnd))
    if (-not $SkipHidden) {
        $hiddenSamples = @(ConvertFrom-StatsLines -Lines @(Read-LogSlice -Path $statsLog -Start $visibleEnd -End $hiddenEnd))
        foreach ($sample in $hiddenSamples) {
            if ($sample.ContainsKey('hidden') -and ($sample['hidden'] -ge 1)) {
                $hiddenReached = $true
                break
            }
        }
    }
} catch {
    Write-Warning "Could not parse ${statsLog}: $($_.Exception.Message)"
}

function Show-PhaseStats {
    param([object[]]$Samples, [string]$Name)
    if ($Samples.Count -eq 0) {
        Write-Note "$Name  no [stats] lines captured (is --stats reaching the log?)"
        return
    }
    Write-Note ("{0}  {1} line(s)" -f $Name, $Samples.Count)
    Write-Note ("  fps            {0}/s   frame avg {1} ms   max {2} ms" -f `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'fps') -Pattern '0.0'), `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'frame_avg_ms') -Pattern '0.00'), `
            (Format-Optional -Value (Get-StatMax -Samples $Samples -Key 'frame_max_ms') -Pattern '0.00'))
    Write-Note ("  repaint        {0} ms" -f `
            (Format-Optional -Value (Get-StatMax -Samples $Samples -Key 'repaint') -Pattern '0'))
    Write-Note ("  mixer          iters {0}/s   renders {1}/s   sleeps {2}/s" -f `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'mixer_iters') -Pattern '0'), `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'mixer_renders') -Pattern '0'), `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'mixer_sleeps') -Pattern '0'))
    Write-Note ("  ring           pop0 {0}/s (empty {1}/s)   pop1 {2}/s (empty {3}/s)" -f `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'pop0') -Pattern '0'), `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'pop0_empty') -Pattern '0'), `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'pop1') -Pattern '0'), `
            (Format-Optional -Value (Get-StatMean -Samples $Samples -Key 'pop1_empty') -Pattern '0'))
    Write-Note ("  stream         underruns {0}   voices {1}   hidden flag max {2}" -f `
            (Format-Optional -Value (Get-StatMax -Samples $Samples -Key 'stream_underruns') -Pattern '0'), `
            (Format-Optional -Value (Get-StatMax -Samples $Samples -Key 'voices') -Pattern '0'), `
            (Format-Optional -Value (Get-StatMax -Samples $Samples -Key 'hidden') -Pattern '0'))
}

$visibleCpu = $null
$visiblePrivate = $null
$visiblePrivateMax = $null
$hiddenCpu = $null
$idleFps = Get-StatMean -Samples $visibleSamples -Key 'fps'
$idleFrameAvg = Get-StatMean -Samples $visibleSamples -Key 'frame_avg_ms'
$idleFrameMax = Get-StatMax -Samples $visibleSamples -Key 'frame_max_ms'
$mixerWakeups = Get-StatMean -Samples $visibleSamples -Key 'mixer_iters'

if ($null -ne $visiblePhase) {
    $visibleCpu = $visiblePhase.CpuPercent
    $visiblePrivate = $visiblePhase.PrivateMeanMb
    $visiblePrivateMax = $visiblePhase.PrivateMaxMb
}
if ($null -ne $hiddenPhase) {
    $hiddenCpu = $hiddenPhase.CpuPercent
}

Write-Step 'Results'
if ($null -ne $visiblePhase) {
    Write-Note ("visible idle  {0} s, {1} working-set samples" -f `
            (Format-Number -Value $visiblePhase.Seconds -Pattern '0.0'), $visiblePhase.PrivateCount)
    if ($null -ne $visibleCpu) {
        Write-Note ("  NFR-8.1.1  CPU {0} % of one core      budget <= {1} %   {2}" -f `
                (Format-Number $visibleCpu '0.00'), (Format-Number $BudgetVisibleCpu '0.0'),
                (Format-Check -Value $visibleCpu -Budget $BudgetVisibleCpu))
    }
    if ($null -ne $visiblePrivate) {
        Write-Note ("  NFR-8.2.1  private WS {0} MB (max {1})  budget <= {2} MB  {3}" -f `
                (Format-Number $visiblePrivate '0.0'), (Format-Number $visiblePrivateMax '0.0'),
                (Format-Number $BudgetPrivateMb '0.0'),
                (Format-Check -Value $visiblePrivate -Budget $BudgetPrivateMb))
    } else {
        Write-Note ("  NFR-8.2.1  private WS n/a (perf counters off); total WS {0} MB" -f `
                (Format-Optional -Value $visiblePhase.TotalMeanMb -Pattern '0.0'))
    }
    if ($null -ne $idleFps) {
        Write-Note ("  NFR-8.1.3  idle fps {0}/s             budget <= {1}/s    {2}" -f `
                (Format-Number $idleFps '0.0'), (Format-Number $BudgetIdleFps '0.0'),
                (Format-Check -Value $idleFps -Budget $BudgetIdleFps))
    }
    if ($null -ne $idleFrameAvg) {
        Write-Note ("  NFR-8.1.3  frame avg {0} ms (max {1})  budget <= {2} ms  {3}" -f `
                (Format-Number $idleFrameAvg '0.00'), (Format-Optional -Value $idleFrameMax -Pattern '0.00'),
                (Format-Number $BudgetFrameMs '0.00'),
                (Format-Check -Value $idleFrameAvg -Budget $BudgetFrameMs))
    }
    if ($null -ne $mixerWakeups) {
        Write-Note ("  NFR-8.1.4  mixer wakeups {0}/s        budget <= {1}/s   {2}" -f `
                (Format-Number $mixerWakeups '0.0'), (Format-Number $BudgetMixerWakeups '0.0'),
                (Format-Check -Value $mixerWakeups -Budget $BudgetMixerWakeups))
    }
    Show-PhaseStats -Samples $visibleSamples -Name '  stats'
}

if ($null -ne $hiddenPhase) {
    Write-Note ''
    Write-Note ("hidden        {0} s, hidden flag observed: {1}" -f `
            (Format-Number -Value $hiddenPhase.Seconds -Pattern '0.0'), $hiddenReached)
    if ($hiddenReached -and ($null -ne $hiddenCpu)) {
        Write-Note ("  NFR-8.1.2  CPU {0} % of total CPU    budget <= {1} %   {2}" -f `
                (Format-Number $hiddenCpu '0.00'), (Format-Number $BudgetHiddenCpu '0.0'),
                (Format-Check -Value $hiddenCpu -Budget $BudgetHiddenCpu))
    } else {
        Write-Note '  NFR-8.1.2  not measured - hidden=1 never appeared in the stats log.'
        Write-Host ''
        Write-Host '    MANUAL STEP for NFR-8.1.2 (hidden CPU):' -ForegroundColor Yellow
        Write-Host '      SoundDeck only enters the hidden state (no painting, idle timer) when'
        Write-Host '      Settings -> Window -> "Minimize to tray" is ON and the tray icon exists.'
        Write-Host '      Turn it on (it saves immediately) and rerun, or let the run start and'
        Write-Host '      minimize the window by hand; the script then reads the hidden=1 lines.'
        Write-Host '      No hidden CPU figure is reported for this run.'
    }
    Show-PhaseStats -Samples $hiddenSamples -Name '  stats'
}

# ---------------------------------------------------------------------------
# 5. Paste block for SPEC 8.1 / 8.2.
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host '--- paste into SPEC NFR tables -------------------------------------------'
Write-Host '| ID | Measured |'
Write-Host '| --- | --- |'
if ($null -ne $visibleCpu) {
    Write-Host ('| NFR-8.1.1 | **{0} %** of one core |' -f (Format-Number $visibleCpu '0.00'))
}
if (($null -ne $hiddenCpu) -and $hiddenReached) {
    Write-Host ('| NFR-8.1.2 | **{0} %** of total CPU |' -f (Format-Number $hiddenCpu '0.00'))
} elseif ($null -ne $hiddenPhase) {
    Write-Host '| NFR-8.1.2 | not measured - hidden state not reached (Minimize to tray off) |'
}
if ($null -ne $idleFrameAvg) {
    Write-Host ('| NFR-8.1.3 | **{0} ms/frame**, {1} fps idle |' -f `
            (Format-Number $idleFrameAvg '0.00'), (Format-Number $idleFps '0.0'))
}
if ($null -ne $mixerWakeups) {
    Write-Host ('| NFR-8.1.4 | **{0}/s** mixer wakeups |' -f (Format-Number $mixerWakeups '0.0'))
}
if ($null -ne $visiblePrivate) {
    Write-Host ('| NFR-8.2.1 | **{0} MB** private working set (max {1} MB) |' -f `
            (Format-Number $visiblePrivate '0.0'), (Format-Number $visiblePrivateMax '0.0'))
}
Write-Host ('| machine | {0} |' -f $env:COMPUTERNAME)
Write-Host ('| build | {0} |' -f $Exe)
Write-Host '-------------------------------------------------------------------------'
Write-Host ''
Write-Host 'Measured values are per-phase means; "max" columns come from the per-sample peak.' -ForegroundColor DarkGray
