$ErrorActionPreference = 'Stop'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    & (Join-Path $PSScriptRoot 'verify-release-metadata.ps1') | Out-Null
    cargo build -p sixa-mcp
    if ($LASTEXITCODE -ne 0) { throw 'MCP sidecar build failed' }
    $sidecarDirectory = Join-Path $PWD 'src-tauri/binaries'
    New-Item -ItemType Directory -Force -Path $sidecarDirectory | Out-Null
    Copy-Item `
        -LiteralPath (Join-Path $PWD 'target/debug/sixa-mcp.exe') `
        -Destination (Join-Path $sidecarDirectory 'sixa-mcp-x86_64-pc-windows-msvc.exe') `
        -Force
    cargo fmt --all --check
    if ($LASTEXITCODE -ne 0) { throw 'Rust formatting failed' }
    cargo test --workspace --exclude sixa --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }
    cargo clippy --workspace --all-targets --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Rust clippy failed' }
    npm.cmd --prefix ui run build
    if ($LASTEXITCODE -ne 0) { throw 'UI build failed' }
    npm.cmd --prefix ui test
    if ($LASTEXITCODE -ne 0) { throw 'UI tests failed' }
    npm.cmd --prefix ui run test:e2e
    if ($LASTEXITCODE -ne 0) { throw 'UI end-to-end tests failed' }
    cargo check -p sixa --locked
    if ($LASTEXITCODE -ne 0) { throw 'Tauri check failed' }
} finally { Pop-Location }
