<#
.SYNOPSIS
  Measures the idle private memory of AIchemist Desktop Buddy on Windows.

.DESCRIPTION
  Metric (owner decision 2026-11-16, see docs/perf.md): memory the app uses on
  its own, i.e. private (unshared) memory summed over the app process and all
  of its WebView2 descendants (msedgewebview2.exe processes whose parent chain
  leads to the app). Shared memory is excluded.

  Two private figures are reported per process:
    PrivateWS  - private working set ("Memory" column in Task Manager). This
                 is the headline number.
    PrivateMB  - Private Bytes / commit charge (pages the process has
                 committed, resident or not). Always >= PrivateWS in practice;
                 reported as the conservative upper bound.
  WorkingSet (includes shared pages) is printed for reference only.

.PARAMETER ExePath
  Launch this executable, wait, measure, then stop it. If omitted, an already
  running instance (matched by -ProcessName) is measured and left running.

.PARAMETER ProcessName
  Process name (without .exe) to look for when -ExePath is not given.

.PARAMETER ProcessId
  Measure the running instance with this PID (and leave it running). Use it
  when more than one instance runs, e.g. side by side builds with different
  identifiers.

.PARAMETER SettleSeconds
  Idle time before measuring. Default 30.

.PARAMETER Json
  Write the result as JSON to this path as well.

.EXAMPLE
  pwsh tools/measure-memory.ps1 -ExePath src-tauri/target/release/aichemist-desktop-buddy.exe
#>
[CmdletBinding()]
param(
    [string]$ExePath,
    [string]$ProcessName = 'aichemist-desktop-buddy',
    [int]$ProcessId = 0,
    [int]$SettleSeconds = 30,
    [string]$Json
)

$ErrorActionPreference = 'Stop'

$launched = $null
if ($ExePath) {
    $launched = Start-Process -FilePath (Resolve-Path $ExePath) -PassThru
    $rootId = $launched.Id
} elseif ($ProcessId -gt 0) {
    $rootId = $ProcessId
} else {
    $p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $p) { throw "No running process named '$ProcessName'." }
    $rootId = $p.Id
}

try {
    Write-Host "Root PID $rootId; settling for $SettleSeconds s..."
    Start-Sleep -Seconds $SettleSeconds

    $all = Get-CimInstance Win32_Process
    if (-not ($all | Where-Object ProcessId -eq $rootId)) {
        throw "Process $rootId exited before measurement."
    }

    # Collect the root and every descendant (WebView2 browser, renderer,
    # GPU, utility and crashpad processes).
    $ids = [System.Collections.Generic.List[uint32]]::new()
    $ids.Add([uint32]$rootId)
    for ($i = 0; $i -lt $ids.Count; $i++) {
        foreach ($c in $all | Where-Object { $_.ParentProcessId -eq $ids[$i] -and $_.ProcessId -ne $ids[$i] }) {
            if (-not $ids.Contains([uint32]$c.ProcessId)) { $ids.Add([uint32]$c.ProcessId) }
        }
    }

    if (-not ($all | Where-Object { $ids.Contains([uint32]$_.ProcessId) -and $_.Name -eq 'msedgewebview2.exe' })) {
        Write-Warning ('No msedgewebview2.exe descendants found. Another instance may own the ' +
            'shared WebView2 browser process for this user data folder; close it and re-run.')
    }

    # Private working set via WMI perf data (locale independent, unlike
    # Get-Counter paths).
    $perf = @{}
    foreach ($row in Get-CimInstance Win32_PerfRawData_PerfProc_Process) {
        $perf[[uint32]$row.IDProcess] = $row
    }

    $mb = 1MB
    $rows = foreach ($id in $ids) {
        $proc = $all | Where-Object ProcessId -eq $id
        $kind = ''
        if ($proc.CommandLine -match '--type=([\w-]+)') {
            $kind = $Matches[1]
            if ($proc.CommandLine -match '--utility-sub-type=([\w.-]+)') { $kind += " ($($Matches[1]))" }
        } elseif ($id -eq $rootId) {
            $kind = 'app'
        } elseif ($proc.Name -eq 'msedgewebview2.exe') {
            $kind = 'browser'
        }
        [pscustomobject]@{
            PID        = $id
            Name       = $proc.Name
            Kind       = $kind
            PrivateWS  = [math]::Round($perf[$id].WorkingSetPrivate / $mb, 1)
            PrivateMB  = [math]::Round($proc.PrivatePageCount / $mb, 1)
            WorkingSet = [math]::Round($proc.WorkingSetSize / $mb, 1)
        }
    }

    $rows | Format-Table -AutoSize | Out-String | Write-Host

    $total = [pscustomobject]@{
        Processes      = $rows.Count
        PrivateWS_MB   = [math]::Round(($rows | Measure-Object PrivateWS -Sum).Sum, 1)
        PrivateBytes_MB = [math]::Round(($rows | Measure-Object PrivateMB -Sum).Sum, 1)
        WorkingSet_MB  = [math]::Round(($rows | Measure-Object WorkingSet -Sum).Sum, 1)
    }
    Write-Host ("TOTAL  processes={0}  private_ws={1} MB  private_bytes={2} MB  (working_set incl. shared={3} MB)" -f `
        $total.Processes, $total.PrivateWS_MB, $total.PrivateBytes_MB, $total.WorkingSet_MB)

    if ($Json) {
        [pscustomobject]@{ total = $total; processes = $rows } |
            ConvertTo-Json -Depth 4 | Set-Content -Path $Json -Encoding utf8
    }
}
finally {
    if ($launched) {
        # Stop the WebView2 tree too, children first.
        foreach ($id in ($ids | Sort-Object -Descending)) {
            Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
        }
        Stop-Process -Id $launched.Id -Force -ErrorAction SilentlyContinue
    }
}
