$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

$cargo = Get-Content (Join-Path $root 'Cargo.toml') -Raw
$cargoVersion = [regex]::Match($cargo, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
$tauri = Get-Content (Join-Path $root 'src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json
$ui = Get-Content (Join-Path $root 'ui/package.json') -Raw | ConvertFrom-Json

if (-not $cargoVersion) { throw 'Cargo workspace version is missing' }
if ($cargoVersion -ne $tauri.version -or $cargoVersion -ne $ui.version) {
    throw "Version mismatch: Cargo=$cargoVersion Tauri=$($tauri.version) UI=$($ui.version)"
}

$notices = Get-Content (Join-Path $root 'THIRD_PARTY_NOTICES.md') -Raw
if ($notices -match 'MuPDF.*尚未加入') {
    throw 'Third-party notices still describe MuPDF as unimplemented'
}
if ($notices -notmatch 'MuPDF.*AGPL') {
    throw 'Third-party notices do not identify the MuPDF AGPL dependency'
}

$report = [ordered]@{
    schema = 1
    version = $cargoVersion
    generated_at = (Get-Date).ToUniversalTime().ToString('o')
    cargo_lock_sha256 = (Get-FileHash (Join-Path $root 'Cargo.lock') -Algorithm SHA256).Hash.ToLowerInvariant()
    npm_lock_sha256 = (Get-FileHash (Join-Path $root 'ui/package-lock.json') -Algorithm SHA256).Hash.ToLowerInvariant()
    license_sha256 = (Get-FileHash (Join-Path $root 'LICENSE') -Algorithm SHA256).Hash.ToLowerInvariant()
}
$report | ConvertTo-Json | Write-Output
