<#
.SYNOPSIS
  Measures the CPU use of AIchemist Desktop Buddy on Windows over a window of
  time, from CPU cycle counts.

.DESCRIPTION
  See docs/perf.md. Sums QueryProcessCycleTime over the app process and every
  descendant (the WebView2 browser, renderer and helper processes), takes the
  difference between the start and the end of the window, and converts cycles
  to time at the nominal clock (Win32_Processor.MaxClockSpeed).

  Cycle counts are used because TotalProcessorTime and Task Manager are
  sampled on the 15.6 ms clock tick and badly undercount the short bursts of
  an animation that draws a frame every few hundred ms.

  Reports CPU time per window, % of one core and % of the whole machine.

.PARAMETER ExePath
  Launch this executable, wait -SettleSeconds, measure, then stop it. If
  omitted, the process given by -ProcessId (or the first one named
  -ProcessName) is measured and left running.

.PARAMETER ProcessId
  PID of an already running instance to measure.

.PARAMETER ProcessName
  Process name (without .exe) to look for when neither -ExePath nor
  -ProcessId is given.

.PARAMETER SettleSeconds
  Wait before the window starts (only with -ExePath). Default 30.

.PARAMETER Seconds
  Length of the measurement window. Default 60.

.PARAMETER Json
  Write the result as JSON to this path as well.

.EXAMPLE
  pwsh tools/measure-cpu.ps1 -ExePath src-tauri/target/release/aichemist-desktop-buddy.exe

.EXAMPLE
  # sleeping state: let the buddy fall asleep after 5 s
  $env:AICHEMIST_SLEEP_AFTER_IDLE_SECS = 5
  pwsh tools/measure-cpu.ps1 -ExePath src-tauri/target/release/aichemist-desktop-buddy.exe
#>
[CmdletBinding()]
param(
    [string]$ExePath,
    [int]$ProcessId,
    [string]$ProcessName = 'aichemist-desktop-buddy',
    [int]$SettleSeconds = 30,
    [int]$Seconds = 60,
    [string]$Json
)

$ErrorActionPreference = 'Stop'

Add-Type -Namespace Buddy -Name Cycles -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)]
public static extern System.IntPtr OpenProcess(uint access, bool inherit, uint pid);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool QueryProcessCycleTime(System.IntPtr process, out ulong cycles);
[DllImport("kernel32.dll")]
public static extern bool CloseHandle(System.IntPtr handle);
'@

# The root and every descendant. Windows keeps a dead parent's PID in
# ParentProcessId, and PIDs are reused, so a child must also have started
# after its parent or an unrelated process can be counted.
function Get-Tree([uint32]$root) {
    $all = Get-CimInstance Win32_Process
    $ids = [System.Collections.Generic.List[uint32]]::new()
    $ids.Add($root)
    for ($i = 0; $i -lt $ids.Count; $i++) {
        $parent = $all | Where-Object ProcessId -eq $ids[$i]
        foreach ($c in $all | Where-Object { $_.ParentProcessId -eq $ids[$i] -and $_.ProcessId -ne $ids[$i] -and $_.CreationDate -ge $parent.CreationDate }) {
            if (-not $ids.Contains([uint32]$c.ProcessId)) { $ids.Add([uint32]$c.ProcessId) }
        }
    }
    $ids
}

# Opens a handle per process and keeps it, so cycles of a process that exits
# during the window are still counted.
$handles = @{}
function Read-Cycles([uint32[]]$ids) {
    $result = @{}
    foreach ($id in $ids) {
        if (-not $handles.ContainsKey($id)) {
            $h = [Buddy.Cycles]::OpenProcess(0x1000, $false, $id) # PROCESS_QUERY_LIMITED_INFORMATION
            if ($h -eq [IntPtr]::Zero) { continue }
            $handles[$id] = $h
        }
        $c = [uint64]0
        if ([Buddy.Cycles]::QueryProcessCycleTime($handles[$id], [ref]$c)) { $result[$id] = $c }
    }
    $result
}

$launched = $null
if ($ExePath) {
    $launched = Start-Process -FilePath (Resolve-Path $ExePath) -PassThru
    $rootId = $launched.Id
    Write-Host "Root PID $rootId; settling for $SettleSeconds s..."
    Start-Sleep -Seconds $SettleSeconds
} elseif ($ProcessId) {
    $rootId = $ProcessId
} else {
    $p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $p) { throw "No running process named '$ProcessName'." }
    $rootId = $p.Id
}

try {
    $cpu = Get-CimInstance Win32_Processor
    $hz = ($cpu | Select-Object -First 1).MaxClockSpeed * 1e6
    $logical = ($cpu | Measure-Object NumberOfLogicalProcessors -Sum).Sum

    $tree = Get-Tree $rootId
    $before = Read-Cycles $tree
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $Seconds
    # Processes started during the window count from zero.
    $tree = @($tree) + @(Get-Tree $rootId | Where-Object { $tree -notcontains $_ })
    $after = Read-Cycles $tree
    $wall = $sw.Elapsed.TotalSeconds

    $names = @{}
    foreach ($p in Get-CimInstance Win32_Process) { $names[[uint32]$p.ProcessId] = $p }
    $rows = foreach ($id in $after.Keys) {
        $proc = $names[$id]
        $kind = if ($id -eq $rootId) { 'app' } elseif ($proc.CommandLine -match '--type=([\w-]+)') { $Matches[1] } else { 'browser' }
        $d = $after[$id] - ($before[$id] ?? 0)
        [pscustomobject]@{ PID = $id; Kind = $kind; Megacycles = [math]::Round($d / 1e6, 1); CpuMs = [math]::Round($d / $hz * 1000, 1) }
    }
    $rows | Sort-Object CpuMs -Descending | Format-Table -AutoSize | Out-String | Write-Host

    $ms = ($rows | Measure-Object CpuMs -Sum).Sum
    $total = [pscustomobject]@{
        Seconds        = [math]::Round($wall, 1)
        CpuMs          = [math]::Round($ms, 1)
        PercentOneCore = [math]::Round($ms / ($wall * 1000) * 100, 3)
        PercentMachine = [math]::Round($ms / ($wall * 1000 * $logical) * 100, 4)
        NominalMHz     = $hz / 1e6
    }
    Write-Host ("TOTAL  window={0} s  cpu={1} ms  one_core={2} %  machine={3} % ({4} logical CPUs at {5} MHz)" -f `
        $total.Seconds, $total.CpuMs, $total.PercentOneCore, $total.PercentMachine, $logical, $total.NominalMHz)
    if ($Json) {
        [pscustomobject]@{ total = $total; processes = $rows } | ConvertTo-Json -Depth 4 | Set-Content -Path $Json -Encoding utf8
    }
}
finally {
    foreach ($h in $handles.Values) { [void][Buddy.Cycles]::CloseHandle($h) }
    if ($launched) {
        # Children first; only the app and its WebView2 processes.
        foreach ($id in (Get-Tree $launched.Id | Sort-Object -Descending)) {
            $name = (Get-Process -Id $id -ErrorAction SilentlyContinue).Name
            if ($id -eq $launched.Id -or $name -eq 'msedgewebview2') {
                Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
            }
        }
    }
}
