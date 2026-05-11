#!/usr/bin/env pwsh
# Stages CLI binaries into src-tauri/resources/ for the Tauri bundler.
#  - winmux-linux-x64 (cross-compiled, static-musl) — uploaded to remote SSH servers
#    by `remote_bootstrap`
#  - winmux-cli.exe (Windows release build) — bundled in the MSI alongside the app
#    so installing winmux gets you both the GUI and the CLI in one shot
# Also (re)writes remote-manifest.json (UTF-8 without BOM).
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$tauriDir = Join-Path $root "src-tauri"
$resourcesDir = Join-Path $tauriDir "resources"
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path $cargoBin) { $env:Path = "$cargoBin;$env:Path" }

# Pre-public hardening: scrub absolute developer paths (incl. Windows
# username) from compiled-in strings. `file!()` macros and panic
# locations in dependencies bake the build machine's $CARGO_HOME +
# $RUSTUP_HOME into .rodata; `strip = "symbols"` cannot remove them.
# We force every release build through this script to apply consistent
# --remap-path-prefix flags so the resulting binary is byte-identical
# regardless of who built it.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE ".rustup" }
$cargoHomeFwd = $cargoHome -replace "\\", "/"
$rustupHomeFwd = $rustupHome -replace "\\", "/"
$userHomeFwd = $env:USERPROFILE -replace "\\", "/"
$env:RUSTFLAGS = "--remap-path-prefix=$cargoHome=cargo --remap-path-prefix=$cargoHomeFwd=cargo --remap-path-prefix=$rustupHome=rustup --remap-path-prefix=$rustupHomeFwd=rustup --remap-path-prefix=$env:USERPROFILE=user --remap-path-prefix=$userHomeFwd=user"
Write-Host "RUSTFLAGS scrub: \$CARGO_HOME=$cargoHome \$RUSTUP_HOME=$rustupHome \$HOME=$env:USERPROFILE"

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

# Also build the Windows release of the CLI and stage it for the MSI bundler.
Push-Location $tauriDir
try {
    & cargo build --release -p winmux
    if ($LASTEXITCODE -ne 0) { throw "cargo build winmux (Windows release) failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}
$srcWin = Join-Path $tauriDir "target\release\winmux.exe"
$dstWin = Join-Path $resourcesDir "winmux-cli.exe"
Copy-Item -Path $srcWin -Destination $dstWin -Force
$winSize = (Get-Item $dstWin).Length
Write-Host "Staged winmux-cli.exe: $winSize bytes"

# Stage the LICENSE next to src-tauri so Tauri's MSI bundler picks it up via the
# relative `licenseFile` setting. We don't commit this copy — the repo's canonical
# LICENSE is at the project root.
$projectLicense = Join-Path $root "..\LICENSE"
$tauriLicense = Join-Path $tauriDir "LICENSE"
Copy-Item -Path $projectLicense -Destination $tauriLicense -Force
Write-Host "Staged LICENSE for bundler"
