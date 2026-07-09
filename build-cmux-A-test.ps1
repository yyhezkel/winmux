$ErrorActionPreference = "Stop"
$env:CARGO_TARGET_DIR = "C:\Users\mlastudent371\Documents\programing\winmux-wt-cmux-A\target-cmux-A"
Push-Location "$PSScriptRoot\app"
try {
  npm install --no-audit --no-fund
  npm run tauri build -- --no-bundle
} finally { Pop-Location }
$exe = Get-ChildItem "$env:CARGO_TARGET_DIR\release\winmux.exe"
$dest = "C:\Users\mlastudent371\winmux-beta2-cmux-A-test.exe"
Copy-Item $exe.FullName $dest -Force
Write-Host "Built: $($exe.FullName) ($($exe.Length) bytes)"
Write-Host "Copied: $dest"
