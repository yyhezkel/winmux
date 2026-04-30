#!/usr/bin/env pwsh
# Cross-compiles the winmux CLI for Linux (x86_64-musl, static) and copies the
# artifact into src-tauri/resources/, refreshing remote-manifest.json with its sha256.
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$tauriDir = Join-Path $root "src-tauri"
$resourcesDir = Join-Path $tauriDir "resources"
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path $cargoBin) { $env:Path = "$cargoBin;$env:Path" }

# Ensure target installed.
$targets = & rustup target list --installed
if (-not ($targets -contains "x86_64-unknown-linux-musl")) {
    Write-Host "Installing x86_64-unknown-linux-musl target..."
    & rustup target add x86_64-unknown-linux-musl
}

Push-Location $tauriDir
try {
    & cargo build --release --target x86_64-unknown-linux-musl -p winmux
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}

$src = Join-Path $tauriDir "target\x86_64-unknown-linux-musl\release\winmux"
$dst = Join-Path $resourcesDir "winmux-linux-x64"
New-Item -ItemType Directory -Path $resourcesDir -Force | Out-Null
Copy-Item -Path $src -Destination $dst -Force

$hash = (Get-FileHash $dst -Algorithm SHA256).Hash.ToLower()
$size = (Get-Item $dst).Length
$iso = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")

$manifestPath = Join-Path $resourcesDir "remote-manifest.json"
$manifest = @{ "x86_64-linux" = @{ path = "winmux-linux-x64"; sha256 = $hash; size = $size; built_at = $iso } } |
    ConvertTo-Json -Depth 10
# Write UTF-8 WITHOUT BOM (Windows PowerShell 5.1 `Set-Content -Encoding utf8` adds BOM,
# which serde_json refuses with "expected value at line 1 column 1").
[System.IO.File]::WriteAllText($manifestPath, $manifest, [System.Text.UTF8Encoding]::new($false))

Write-Host "Built winmux-linux-x64: $size bytes, sha256=$hash"
