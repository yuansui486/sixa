# Windows MCP installation lifecycle

`hooks.nsh` is included through Tauri's `installerHooks` configuration. Both
`NSIS_HOOK_PREINSTALL` and `NSIS_HOOK_PREUNINSTALL` first invoke Tauri's normal
main-process guard, then run the embedded `stop-mcp.ps1` from `$PLUGINSDIR`.
The install hook also handles MCP instances left behind by older uninstallers.

The script runs hidden under Windows PowerShell 5.1. It compares the normalized
executable path and current Windows user SID, queries and terminates through the
same process handle to avoid PID reuse, and checks that the executable can be
opened exclusively for read/write without modifying it. It waits at most 10
seconds, including a short quiet period to catch immediate client restarts.
The NSIS subprocess has a 15-second timeout. No app data, models, credentials or
client configurations are removed. PowerShell/C# is used only during installation,
so no background watcher or additional application startup cost is introduced.

Script exit codes: `0` released/absent, `2` still occupied, `3` inaccessible,
`4` invalid input or script failure. NSIS maps failures to an interactive
retry/cancel dialog, or exit code `10` in silent/passive mode. PowerShell blocked
by an enterprise policy is an explicit failure, not permission to skip the check.

## Regression checks

From the repository root, using Windows PowerShell:

```powershell
cargo build --locked -p sixa-mcp
& src-tauri/installer/tests/run.ps1 `
  -McpExecutable target/debug/sixa-mcp.exe `
  -MakeNsis "$env:LOCALAPPDATA\tauri\NSIS\makensis.exe" `
  -TauriNsisDirectory target/release/nsis/x64
```

The optional `TauriNsisDirectory` selects the generated Tauri helpers and plugin
for the main-process guard. Omit it before the first Tauri package build; that
variant exercises the production MCP hooks with a no-op main guard. Standalone
NSIS installations can use their own `makensis.exe` for this variant.

The registry-free fixture installer runs only in a unique temporary directory.
It uses real MCP executables with open stdio connections, verifies initialize and
tools/list after replacement, and covers multiple clients, different installation
paths, SID mismatch, Unicode/spaces/apostrophes, one-time and repeated restarts,
read-only files, persistent locks, silent failure, retry and uninstall. With Tauri
helpers it also tests main + MCP termination together, using an MCP binary under
a harmless test main-process name. It never opens the real app or changes an
existing installation. Interactive retry/cancel wording requires manual UI review.

`quality.yml` runs the independent Windows regression; `release.yml` reruns it
against the Tauri helpers and MCP executable produced by the package build.
Neither test downloads models or contacts OSS.
