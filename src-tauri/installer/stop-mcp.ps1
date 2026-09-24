# Windows PowerShell 5.1. Runs only from the installer/uninstaller, never at app startup.
# Exit codes: 0 = released, 2 = still locked, 3 = access denied, 4 = invalid request/script failure.
param([Parameter(Mandatory = $true)][string]$InstallDir)

$ErrorActionPreference = 'Stop'
$deadline = [Diagnostics.Stopwatch]::StartNew()
try {
    if ($InstallDir -notmatch '^(?:[a-zA-Z]:[\\/]|\\\\[^\\]+\\[^\\]+)') {
        throw 'An absolute installation directory is required.'
    }
    $executable = [IO.Path]::GetFullPath([IO.Path]::Combine($InstallDir, 'sixa-mcp.exe'))
    $userSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value

    # Query identity and terminate through the SAME process handle. A reused PID
    # must never cause us to terminate a process outside the requested directory.
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;

public static class SixaInstallerProcesses {
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, int id);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder path, ref uint size);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll")]
    static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);

    public static int StopMatching(string executable, string userSid) {
        int stopped = 0;
        foreach (Process candidate in Process.GetProcessesByName("sixa-mcp")) {
            using (candidate) {
                // QUERY_LIMITED_INFORMATION | TERMINATE | SYNCHRONIZE
                IntPtr process = OpenProcess(0x00101001, false, candidate.Id);
                if (process == IntPtr.Zero) continue;
                try {
                    StringBuilder path = new StringBuilder(32768);
                    uint size = (uint)path.Capacity;
                    if (!QueryFullProcessImageName(process, 0, path, ref size)) continue;
                    if (!String.Equals(Path.GetFullPath(path.ToString()), executable, StringComparison.OrdinalIgnoreCase)) continue;
                    IntPtr token;
                    if (!OpenProcessToken(process, 8, out token)) continue;
                    bool sameUser;
                    try {
                        using (WindowsIdentity identity = new WindowsIdentity(token)) {
                            sameUser = identity.User != null && identity.User.Value == userSid;
                        }
                    } finally { CloseHandle(token); }
                    if (!sameUser || WaitForSingleObject(process, 0) == 0) continue;
                    if (!TerminateProcess(process, 0) && WaitForSingleObject(process, 0) != 0) {
                        throw new Win32Exception(Marshal.GetLastWin32Error());
                    }
                    stopped++;
                } finally { CloseHandle(process); }
            }
        }
        return stopped;
    }
}
'@

    $releasedChecks = 0
    $failureCode = 2
    while ($deadline.ElapsedMilliseconds -lt 10000) {
        try {
            $stopped = [SixaInstallerProcesses]::StopMatching($executable, $userSid)
            if ($stopped -gt 0) { $releasedChecks = 0 }
            # Opening for write as well as read detects executable image mappings;
            # no bytes are written. Do not use Test-Path: it can hide access errors.
            try {
                $probe = [IO.File]::Open($executable, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
                $probe.Dispose()
            } catch [IO.FileNotFoundException] {
                # A previous uninstaller may already have removed the file.
            } catch [IO.DirectoryNotFoundException] {
                # First installation, or an already removed installation.
            }
            $releasedChecks++
            # Observe a short quiet period instead of proceeding immediately after
            # TerminateProcess. AI clients may restart their stdio server once.
            if ($releasedChecks -ge 3) {
                Write-Output 'MCP_RELEASED'
                exit 0
            }
        } catch {
            $releasedChecks = 0
            $failureCode = 2
            $cause = $_.Exception
            while ($cause.InnerException) { $cause = $cause.InnerException }
            if ($cause -is [UnauthorizedAccessException] -or
                ($cause -is [ComponentModel.Win32Exception] -and $cause.NativeErrorCode -eq 5)) {
                $failureCode = 3
            }
        }
        Start-Sleep -Milliseconds 200
    }
    Write-Output 'MCP_NOT_RELEASED'
    exit $failureCode
} catch {
    Write-Output 'MCP_CLEANUP_FAILED'
    exit 4
}
