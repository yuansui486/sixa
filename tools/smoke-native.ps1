param([string]$Executable = 'target/release/sixa.exe')
$ErrorActionPreference = 'Stop'
$path = (Resolve-Path -LiteralPath $Executable).Path
$process = Start-Process -FilePath $path -WindowStyle Hidden -PassThru
try {
    Start-Sleep -Seconds 8
    $process.Refresh()
    if ($process.HasExited) { throw "Desktop exited early: $($process.ExitCode)" }
    $listeners = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object OwningProcess -eq $process.Id)
    if ($listeners.Count -gt 0) { throw 'Desktop opened a TCP listener' }
    [pscustomobject]@{ ProcessId=$process.Id; Alive=$true; TcpListeners=$listeners.Count; WorkingSetBytes=$process.WorkingSet64; PeakWorkingSetBytes=$process.PeakWorkingSet64; WindowTitle=$process.MainWindowTitle } | ConvertTo-Json
} finally {
    $process.Refresh()
    if (-not $process.HasExited) { Stop-Process -Id $process.Id }
}
