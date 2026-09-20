param([switch]$DebugBuild)
$ErrorActionPreference = 'Stop'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    $profile = if ($DebugBuild) { 'debug' } else { 'release' }
    $cargoArguments = @('build', '-p', 'sixa-mcp')
    if (-not $DebugBuild) { $cargoArguments += '--release' }
    & cargo @cargoArguments
    if ($LASTEXITCODE -ne 0) { throw 'MCP sidecar build failed' }

    $sidecarDirectory = Join-Path $PWD 'src-tauri/binaries'
    New-Item -ItemType Directory -Force -Path $sidecarDirectory | Out-Null
    Copy-Item `
        -LiteralPath (Join-Path $PWD "target/$profile/sixa-mcp.exe") `
        -Destination (Join-Path $sidecarDirectory 'sixa-mcp-x86_64-pc-windows-msvc.exe') `
        -Force

    $arguments = @('build', '--bundles', 'nsis')
    if ($DebugBuild) { $arguments += '--debug' }
    & ./ui/node_modules/.bin/tauri.cmd @arguments
    if ($LASTEXITCODE -ne 0) { throw 'Tauri NSIS build failed' }
} finally { Pop-Location }
