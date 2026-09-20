param([string]$Source = 'models/raner-onnx-build')
$ErrorActionPreference = 'Stop'
$sourceRoot = (Resolve-Path -LiteralPath $Source).Path
$modelParent = Join-Path $env:LOCALAPPDATA 'LocalDesensitization\models'
$destination = Join-Path $modelParent 'raner-v1'
if (Test-Path -LiteralPath $destination) { throw '模型已安装。请关闭应用并将现有 raner-v1 目录备份后再运行，脚本不会覆盖已安装模型。' }
$manifest = Get-Content -LiteralPath (Join-Path $sourceRoot 'manifest.json') -Raw | ConvertFrom-Json
if ($manifest.schema -ne 1) { throw 'Unsupported manifest schema' }
$names = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
foreach ($file in $manifest.files) {
    if ([string]::IsNullOrWhiteSpace($file.name) -or $file.name -match '[\\/:]' -or $file.name -in '.', '..' -or -not $names.Add($file.name)) { throw 'Invalid or duplicate manifest path' }
    $path = Join-Path $sourceRoot $file.name
    if ((Get-Item -LiteralPath $path).Length -ne $file.size) { throw "Size mismatch: $($file.name)" }
    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $file.sha256) { throw "SHA256 mismatch: $($file.name)" }
}
foreach ($required in @('emissions.onnx', 'tokenizer.json', 'crf.json', 'onnxruntime.dll')) {
    if (-not $names.Contains($required)) { throw "Missing $required" }
}
New-Item -ItemType Directory -Force -Path $modelParent | Out-Null
$stage = Join-Path $modelParent ('install-' + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $stage | Out-Null
foreach ($file in $manifest.files) { Copy-Item -LiteralPath (Join-Path $sourceRoot $file.name) -Destination (Join-Path $stage $file.name) }
Copy-Item -LiteralPath (Join-Path $sourceRoot 'manifest.json') -Destination (Join-Path $stage 'manifest.json')
# Same-volume rename exposes a complete directory only after all copies succeed.
Move-Item -LiteralPath $stage -Destination $destination
Write-Output "模型已安装：$destination"
