# Run with Windows PowerShell 5.1. Uses real stdio MCP processes and a registry-free
# NSIS fixture; never installs over the user's app, logs in, or downloads models.
param(
    [Parameter(Mandatory = $true)][string]$McpExecutable,
    [Parameter(Mandatory = $true)][string]$MakeNsis,
    [string]$TauriNsisDirectory
)
$ErrorActionPreference = 'Stop'
$root = Join-Path ([IO.Path]::GetTempPath()) ('sixa-installer-test-' + [Guid]::NewGuid().ToString('N'))
$cleanupScript = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\stop-mcp.ps1'))
$powershell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
$children = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
$checks = 0

function Assert-True($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Passed([string]$Message) {
    $script:checks++
    Write-Host "PASS $Message"
}
function Start-Hidden([string]$Executable, [string]$Arguments) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $Executable
    $info.Arguments = $Arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    $children.Add($process)
    return $process
}
function Wait-Exit($Process, [int]$Timeout = 20000) {
    if (-not $Process.WaitForExit($Timeout)) { throw "Child process timed out (PID $($Process.Id))" }
    return $Process.ExitCode
}
function Start-Cleanup([string]$Directory) {
    return Start-Hidden $powershell ('-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{0}" -InstallDir "{1}"' -f $cleanupScript, $Directory)
}
function Run-Cleanup([string]$Directory) {
    return Wait-Exit (Start-Cleanup $Directory)
}
function Start-Mcp([string]$Directory) {
    $process = Start-Hidden (Join-Path $Directory 'sixa-mcp.exe') 'serve'
    $process.StandardInput.WriteLine('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"installer-regression","version":"1"}}}')
    $read = $process.StandardOutput.ReadLineAsync()
    Assert-True ($read.Wait(5000)) 'MCP initialize timed out'
    $response = $read.Result | ConvertFrom-Json
    Assert-True ($response.result.serverInfo.name -eq 'sixa') 'Installed MCP returned invalid metadata'
    $process.StandardInput.WriteLine('{"jsonrpc":"2.0","method":"notifications/initialized"}')
    return $process
}
function Assert-Tools($Process) {
    $Process.StandardInput.WriteLine('{"jsonrpc":"2.0","id":2,"method":"tools/list"}')
    $read = $Process.StandardOutput.ReadLineAsync()
    Assert-True ($read.Wait(5000)) 'MCP tools/list timed out'
    $response = $read.Result | ConvertFrom-Json
    Assert-True ($response.result.tools.Count -eq 6) 'Installed MCP tool list is incomplete'
}
function Copy-Mcp([string]$Directory) {
    New-Item -ItemType Directory -Path $Directory -Force | Out-Null
    Copy-Item -LiteralPath $McpExecutable -Destination (Join-Path $Directory 'sixa-mcp.exe')
}

try {
    $McpExecutable = (Resolve-Path -LiteralPath $McpExecutable).Path
    $MakeNsis = (Resolve-Path -LiteralPath $MakeNsis).Path
    New-Item -ItemType Directory -Path $root | Out-Null
    # Build the name using code points to keep this script ASCII-compatible with PS 5.1.
    $directory = Join-Path $root (([char]0x79c1).ToString() + [char]0x5323 + " O'Brien & space")
    $other = Join-Path $root 'another-install'
    Assert-True ((Run-Cleanup $directory) -eq 0) 'Fresh install was blocked'
    Assert-True ((Run-Cleanup 'relative-path') -eq 4) 'Relative installation path accepted'
    Passed 'missing directory and invalid relative path'

    Copy-Mcp $directory
    Copy-Mcp $other
    $otherMcp = Start-Mcp $other
    $first = Start-Mcp $directory
    $native = [regex]::Match([IO.File]::ReadAllText($cleanupScript), "(?s)Add-Type -TypeDefinition @'\r?\n(.*?)\r?\n'@").Groups[1].Value
    Add-Type -TypeDefinition $native
    $wrongUserCount = [SixaInstallerProcesses]::StopMatching((Join-Path $directory 'sixa-mcp.exe'), 'S-1-0-0')
    Assert-True ($wrongUserCount -eq 0 -and -not $first.HasExited) 'Process owner SID check failed'
    Passed 'different owner SID is never terminated'
    $second = Start-Mcp $directory
    $third = Start-Mcp $directory
    Assert-True ((Run-Cleanup $directory.ToUpperInvariant()) -eq 0) 'Multiple MCP cleanup failed'
    foreach ($process in @($first, $second, $third)) {
        Assert-True ($process.WaitForExit(2000)) 'Matching MCP still running'
    }
    Assert-True (-not $otherMcp.HasExited) 'A different installation was terminated'
    Assert-Tools $otherMcp
    Passed 'three instances, Unicode/space/apostrophe path, case-insensitive exact matching, other installation preserved'

    $restarted = Start-Mcp $directory
    $cleanup = Start-Cleanup $directory
    Assert-True ($restarted.WaitForExit(5000)) 'Initial MCP was not stopped'
    $restarted = Start-Mcp $directory
    Assert-True ((Wait-Exit $cleanup) -eq 0) 'One-time client restart was not handled'
    Assert-True ($restarted.WaitForExit(2000)) 'Restarted MCP still holds the file'
    Passed 'client restarts MCP during cleanup'

    $restarted = Start-Mcp $directory
    $cleanup = Start-Cleanup $directory
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $restartBlocked = $false
    while (-not $cleanup.HasExited -and $watch.ElapsedMilliseconds -lt 18000) {
        if ($restarted.HasExited) {
            # The cleanup's exclusive file probe may correctly deny a racing
            # launch. No handshake is needed while deliberately killing/restarting.
            try { $restarted = Start-Hidden (Join-Path $directory 'sixa-mcp.exe') 'serve' }
            catch {
                $failure = $_.Exception
                while ($failure.InnerException) { $failure = $failure.InnerException }
                if ($failure -is [ComponentModel.Win32Exception] -and $failure.NativeErrorCode -eq 32) { $restartBlocked = $true }
                else { throw }
            }
        }
        Start-Sleep -Milliseconds 20
    }
    $restartResult = Wait-Exit $cleanup
    Assert-True ($restartResult -ne 0 -or ($restartBlocked -and $restarted.HasExited)) 'Unbounded client restarts were reported as success'
    if (-not $restarted.HasExited) { $restarted.Kill(); $restarted.WaitForExit() }
    Passed 'continuous client restart is bounded or prevented by the exclusive file guard'

    $exe = Join-Path $directory 'sixa-mcp.exe'
    [IO.File]::SetAttributes($exe, [IO.FileAttributes]::ReadOnly)
    try { Assert-True ((Run-Cleanup $directory) -eq 3) 'Read-only executable must report access denied' }
    finally { [IO.File]::SetAttributes($exe, [IO.FileAttributes]::Normal) }
    Passed 'access-denied file is not treated as absent'

    $installer = Join-Path $root 'fixture-setup.exe'
    $defines = @("/DMCP_BINARY=$McpExecutable", "/DOUTPUT=$installer")
    if ($TauriNsisDirectory) {
        $TauriNsisDirectory = (Resolve-Path -LiteralPath $TauriNsisDirectory).Path
        $pluginDir = Join-Path (Split-Path $MakeNsis) 'Plugins\x86-unicode\additional'
        $defines += @("/DTAURI_NSIS_DIR=$TauriNsisDirectory", "/DTAURI_PLUGIN_DIR=$pluginDir")
    }
    & $MakeNsis '/V2' '/WX' @defines (Join-Path $PSScriptRoot 'fixture.nsi')
    Assert-True ($LASTEXITCODE -eq 0) 'NSIS hooks failed to compile'
    $marker = Join-Path $root 'user-data-preserved.txt'
    [IO.File]::WriteAllText($marker, 'models tasks settings')
    # Simulates an old uninstaller that removed the main exe but left a running MCP.
    $old = Start-Mcp $directory
    Assert-True ((Wait-Exit (Start-Hidden $installer ('/S /D=' + $directory))) -eq 0) 'Upgrade after legacy uninstall failed'
    Assert-True ($old.WaitForExit(2000)) 'Upgrade left the old MCP running'
    Assert-True ((Get-FileHash -LiteralPath $exe).Hash -eq (Get-FileHash -LiteralPath $McpExecutable).Hash) 'Installed MCP hash mismatch'
    $installed = Start-Mcp $directory
    Assert-Tools $installed
    Passed 'silent upgrade releases legacy MCP and installs an executable with a valid MCP handshake'

    if ($TauriNsisDirectory) {
        # Reuse a real executable as a harmless stand-in for the main program.
        # It never launches a GUI, reads app data, or connects to the auth backend.
        $main = Start-Hidden (Join-Path $directory 'sixa-installer-regression.exe') 'serve'
        Assert-True ((Wait-Exit (Start-Hidden $installer ('/S /D=' + $directory))) -eq 0) 'Main + MCP reinstall failed'
        Assert-True ($main.WaitForExit(2000) -and $installed.WaitForExit(2000)) 'Main/MCP still running after reinstall'
        $installed = Start-Mcp $directory
        $main = Start-Hidden (Join-Path $directory 'sixa-installer-regression.exe') 'serve'
        Passed 'Tauri main-process guard and MCP cleanup work together during reinstall'
    }

    # Run with _?= to wait for the actual uninstaller rather than its temp launcher.
    Assert-True ((Wait-Exit (Start-Hidden (Join-Path $directory 'uninstall.exe') ('/S _?=' + $directory))) -eq 0) 'Uninstall failed'
    Assert-True ($installed.WaitForExit(2000)) 'Uninstall left MCP running'
    if ($TauriNsisDirectory) { Assert-True ($main.WaitForExit(2000)) 'Uninstall left main process running' }
    Assert-True (-not [IO.File]::Exists($exe)) 'Uninstall left MCP executable behind'
    Assert-True ([IO.File]::ReadAllText($marker) -eq 'models tasks settings') 'External data changed'
    Assert-True (-not $otherMcp.HasExited) 'Uninstall affected a different installation'
    Passed 'silent uninstall with running MCP, external data and other installation preserved'

    Copy-Mcp $directory
    $lock = [IO.File]::Open($exe, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        Assert-True ((Wait-Exit (Start-Hidden $installer ('/S /D=' + $directory))) -eq 10) 'Locked executable did not fail silent installation'
        Assert-True (-not [IO.File]::Exists((Join-Path $directory 'installed.txt'))) 'Install continued after cleanup failure'
    } finally { $lock.Dispose() }
    Assert-True ((Wait-Exit (Start-Hidden $installer ('/S /D=' + $directory))) -eq 0) 'Retry after release failed'
    Passed 'persistent lock fails silent installation; installation succeeds after releasing the lock'
    Write-Host "$checks installer regression groups passed"
} finally {
    foreach ($process in $children) {
        try { if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit(2000) | Out-Null } } catch { }
        $process.Dispose()
    }
    # Only delete the unique temporary directory created by this test.
    $resolved = [IO.Path]::GetFullPath($root)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        -not ([IO.Path]::GetFileName($resolved)).StartsWith('sixa-installer-test-')) {
        throw 'Unsafe test cleanup path'
    }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
