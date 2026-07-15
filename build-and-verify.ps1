# beta.3-lh-insights: one-shot build + verify + copy exe. Everything logged.
# Run: pwsh -ExecutionPolicy Bypass -File .\build-and-verify.ps1
# Log:  .\build-verify.log
$ErrorActionPreference = "Continue"
$LOG = Join-Path $PSScriptRoot "build-verify.log"
"" | Out-File $LOG -Encoding utf8

function Section($name) {
  "`n==================== $name ====================" | Tee-Object -FilePath $LOG -Append
  Write-Host "==== $name ====" -ForegroundColor Cyan
}
function Run($cmd, $args) {
  "`n>>> $cmd $args" | Tee-Object -FilePath $LOG -Append
  Write-Host ">>> $cmd $args" -ForegroundColor Yellow
  & $cmd $args 2>&1 | Tee-Object -FilePath $LOG -Append
  return $LASTEXITCODE
}

Set-Location $PSScriptRoot

Section "Unlock stale git locks (from interrupted worktree add)"
$repoRoot = (git rev-parse --git-common-dir).Trim()
$wtDir = Join-Path $repoRoot "worktrees\winmux-wt-lh-insights"
foreach ($f in @("index.lock", "index.lock.bak", "locked", "locked.bak")) {
  $p = Join-Path $wtDir $f
  if (Test-Path $p) {
    Remove-Item $p -Force -ErrorAction SilentlyContinue
    "removed $p" | Tee-Object -FilePath $LOG -Append
  }
}

Section "git status"
Run "git" @("status", "--short")
Run "git" @("branch", "--show-current")

Section "cargo check --workspace (app/src-tauri)"
Set-Location (Join-Path $PSScriptRoot "app\src-tauri")
$cargoOk = Run "cargo" @("check", "--workspace", "--all-targets")

Section "cargo test insights_local (app crate)"
$testOk = Run "cargo" @("test", "-p", "app", "insights_local", "--", "--nocapture")

Section "npx tsc --noEmit (frontend)"
Set-Location (Join-Path $PSScriptRoot "app")
$tscOk = Run "npx" @("tsc", "--noEmit")

if ($cargoOk -ne 0 -or $tscOk -ne 0) {
  Section "STOPPING — checks failed"
  "cargo check exit=$cargoOk, cargo test exit=$testOk, tsc exit=$tscOk" | Tee-Object -FilePath $LOG -Append
  exit 1
}

Section "npm run tauri build -- --debug"
Set-Location $PSScriptRoot
$buildOk = Run "npm" @("run", "tauri", "build", "--", "--debug")
if ($buildOk -ne 0) {
  Section "STOPPING — build failed"
  exit 1
}

Section "copy exe"
$src = Join-Path $PSScriptRoot "app\src-tauri\target\debug\app.exe"
$dst = "C:\Users\mlastudent371\winmux-beta3-lh-insights-test.exe"
if (Test-Path $src) {
  Copy-Item $src $dst -Force
  $sizeMB = [math]::Round((Get-Item $dst).Length / 1MB, 2)
  "copied $src -> $dst ($sizeMB MB)" | Tee-Object -FilePath $LOG -Append
} else {
  "MISSING: $src" | Tee-Object -FilePath $LOG -Append
}

Section "git commit"
Set-Location $PSScriptRoot
Run "git" @("add", "-A")
Run "git" @("commit", "-m", "feat(insights): native local Insights for Local workspaces`n`nbeta.3-lh-insights: adds insights_local.rs (sysinfo + bollard on Windows`nnamed-pipe for Docker Desktop) so the Monitor panel works for Local`nworkspaces, not just SSH.`n`n- new module app/src-tauri/src/insights_local.rs`n- insights_fetch/docker_action/hygiene_kill route local -> in-proc`n- hasServer(w) now returns true (native local server present)`n- deps: sysinfo 0.32, bollard 0.17 (Windows named pipe), futures-util 0.3`n`nDocker: bollard 0.17 auto-includes hyper-named-pipe on cfg(windows),`nreachable at \\.\pipe\docker_engine. Falls back to empty container`nlist when Docker Desktop isn't running.`n`nTests: parse_query_u32, pct_u32 clamp, hygiene empty, route unknown`npath, and a smoke test that snapshot() reports non-zero mem_total.")
Run "git" @("log", "--oneline", "-3")

"`nDONE. Log: $LOG" | Tee-Object -FilePath $LOG -Append
