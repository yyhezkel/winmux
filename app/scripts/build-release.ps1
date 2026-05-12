#!/usr/bin/env pwsh
# Phase 13.A pre-public hardening: a wrapper for `npm run tauri build` that
# guarantees the developer-path scrub is applied to the app.exe inside the
# MSI / NSIS. `build-linux-cli.ps1` sets RUSTFLAGS for the CLI bundles, but
# its env-var lifetime ends when the script returns — `npm run tauri build`
# spawns a fresh cargo that doesn't inherit it. This script sets RUSTFLAGS
# in the parent process, then forwards every CLI arg to `npm run tauri build`.
$ErrorActionPreference = "Stop"

$cargoHome   = if ($env:CARGO_HOME)   { $env:CARGO_HOME }   else { Join-Path $env:USERPROFILE ".cargo" }
$rustupHome  = if ($env:RUSTUP_HOME)  { $env:RUSTUP_HOME }  else { Join-Path $env:USERPROFILE ".rustup" }
$cargoFwd    = $cargoHome    -replace "\\", "/"
$rustupFwd   = $rustupHome   -replace "\\", "/"
$homeBack    = $env:USERPROFILE
$homeFwd     = $homeBack     -replace "\\", "/"

$flags = @(
    "--remap-path-prefix=$cargoHome=cargo"
    "--remap-path-prefix=$cargoFwd=cargo"
    "--remap-path-prefix=$rustupHome=rustup"
    "--remap-path-prefix=$rustupFwd=rustup"
    "--remap-path-prefix=$homeBack=user"
    "--remap-path-prefix=$homeFwd=user"
) -join " "

$env:RUSTFLAGS = $flags
Write-Host "RUSTFLAGS = $flags"
Write-Host "Forwarding to: npm run tauri build $($args -join ' ')"

# Force the app.exe to rebuild so the new remap is applied — Cargo's
# incremental cache otherwise reuses the previous compilation that
# baked in the unscrubbed paths.
$appExe = Join-Path $PSScriptRoot "..\src-tauri\target\release\app.exe"
if (Test-Path $appExe) { Remove-Item $appExe -Force }
Get-ChildItem (Join-Path $PSScriptRoot "..\src-tauri\target\release\deps\") -Filter "app-*" -ErrorAction SilentlyContinue | Remove-Item -Force

& npm run tauri build -- @args
if ($LASTEXITCODE -ne 0) { throw "npm run tauri build failed (exit $LASTEXITCODE)" }
