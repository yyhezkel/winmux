# Build

How to build winmux from source on Windows. Linux/macOS hosts aren't supported yet
(the Tauri app needs WebView2 + the Windows-only PTY backends).

## Prerequisites

| Tool | Why | Install |
|---|---|---|
| **Rust (stable)** | Backend, CLI | `winget install --id Rustlang.Rustup -e` then `rustup default stable` |
| **Node 18+** | Frontend tooling (Vite, Tauri CLI) | `winget install --id OpenJS.NodeJS.LTS -e` |
| **MSVC C++ Build Tools** | The Rust MSVC toolchain needs `link.exe` and the Windows SDK headers | `winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --quiet --wait --norestart"` |
| **WebView2 runtime** | The Tauri webview | Already present on Win10 21H2+ / Win11. Otherwise: [Microsoft Edge WebView2](https://developer.microsoft.com/microsoft-edge/webview2/) |
| **`x86_64-unknown-linux-musl` target** | Cross-compile the Linux CLI | `rustup target add x86_64-unknown-linux-musl` |

The MSVC step is several gigabytes and slow. It's the biggest one-time cost.

## Verify your toolchain

```cmd
rustc --version
cargo --version
node --version
npm --version
where cl.exe
rustup target list --installed
```

You want `cl.exe` to resolve and the targets list to include both
`x86_64-pc-windows-msvc` and `x86_64-unknown-linux-musl`.

## File layout

```
winmux/
  README.md
  LICENSE
  docs/
  app/
    package.json                  # npm root, scripts wiring vite + tauri
    scripts/
      build-linux-cli.ps1         # cross-compile + manifest update
    src/                          # SolidJS frontend
    index.html
    src-tauri/                    # Tauri root; cargo workspace root
      Cargo.toml                  # workspace + main "app" package
      .cargo/config.toml          # rust-lld linker for musl targets
      build.rs
      tauri.conf.json             # bundle config (resources, window, identifier)
      capabilities/default.json
      icons/
      resources/
        winmux-linux-x64          # static-PIE ELF, bundled into app.exe
        remote-manifest.json      # SHA-256 + metadata
      src/
        main.rs                   # 6-line bin entry
        lib.rs                    # ~1760 lines, the bulk
        rpc_server.rs             # Named-pipe JSON-RPC server
        remote_bootstrap.rs       # SFTP upload of the Linux CLI
        tunnel.rs                 # reverse SSH tunnel + HMAC handshake
      cli/                        # separate workspace member
        Cargo.toml
        src/main.rs               # the standalone winmux CLI
      target/
        debug/
          app.exe                 # Tauri app
          winmux.exe              # Windows CLI
          x86_64-unknown-linux-musl/release/winmux  # Linux CLI source
        release/                  # not used currently
```

## Dev (hot-reload)

```cmd
cd app
npm install
npm run tauri dev
```

This runs Vite on `http://localhost:1420`, then `cargo run` on the Rust side.
Tauri opens a window pointed at the local Vite. Edits to TypeScript hot-reload;
edits to Rust trigger a rebuild + relaunch.

## Standalone debug build

```cmd
cd app
npm run tauri build -- --debug
```

This:
1. Runs `npm run build` first (which `package.json` chains to
   `npm run build:linux-cli && vite build`), so the Linux CLI is freshly cross-compiled
   and the manifest regenerated **before** Tauri bundles resources.
2. Runs `cargo build` (debug profile) for the `app` binary.
3. Patches the exe with bundle metadata and copies `bundle.resources` next to it.

Output:
- `app/src-tauri/target/debug/app.exe` — the standalone app, ~21 MB
- `app/src-tauri/target/debug/winmux.exe` — the Windows CLI, ~3 MB
- `app/src-tauri/target/debug/resources/winmux-linux-x64` — bundled, ~2 MB
- `app/src-tauri/target/debug/resources/remote-manifest.json` — bundled

The MSI bundling step can fail with `error 32: file in use` if you have the app
running. Kill it (`Stop-Process app -Force`) before rebuilding. The MSI is optional
and isn't required for the standalone exe to work.

## Cross-compile the Linux CLI manually

```powershell
powershell -ExecutionPolicy Bypass -File .\app\scripts\build-linux-cli.ps1
```

The script:
1. Ensures `x86_64-unknown-linux-musl` is installed.
2. `cargo build --release --target x86_64-unknown-linux-musl -p winmux`.
3. Copies `target/x86_64-unknown-linux-musl/release/winmux` to
   `src-tauri/resources/winmux-linux-x64`.
4. Writes `remote-manifest.json` next to it. **Without BOM** — the script uses
   `[System.IO.File]::WriteAllText($p, $json, [System.Text.UTF8Encoding]::new($false))`.
   Don't switch back to `Set-Content -Encoding utf8` — that adds a BOM and
   `serde_json::from_str` rejects it.

The script prints `Built winmux-linux-x64: <bytes> bytes, sha256=<hash>` on success.

## Run the standalone

```
.\app\src-tauri\target\debug\app.exe
```

Connect to an SSH workspace. The bootstrap will:
- Detect the remote arch.
- SHA-256 compare against the manifest.
- SFTP-upload the binary to `~/.winmux/bin/winmux-linux-x64` if mismatched.
- Symlink `~/.winmux/bin/winmux` → `winmux-linux-x64`.

Inside the SSH pane, `~/.winmux/bin/winmux list-workspaces` should round-trip back.

## Common gotchas

### Sandbox `%APPDATA%`

If you launch a process from a sandboxed shell (e.g. some test harnesses, or
within an AppContainer like Claude Code's session), `%APPDATA%` may resolve to a
sandbox-redirected path (`%LOCALAPPDATA%\Packages\<sandbox>\LocalCache\Roaming\…`).
The standalone `app.exe` launched from Explorer always uses the **real**
`%APPDATA%` (`C:\Users\<user>\AppData\Roaming\winmux\…`).

When debugging persistence, always use the **literal real path** for inspection,
not `$env:APPDATA`, because the latter resolves to whatever your shell sees.

### BOM in `remote-manifest.json`

If the bootstrap fails with `parse manifest: expected value at line 1 column 1`,
the writer added a UTF-8 BOM. Verify with:

```powershell
$bytes = [System.IO.File]::ReadAllBytes('C:\path\to\remote-manifest.json') | Select-Object -First 4
$bytes | ForEach-Object { '{0:X2}' -f $_ }
```

If you see `EF BB BF`, fix the writer (don't add a leading BOM-strip and call it
done — fix it at the source). The loader (`remote_bootstrap::read_manifest`) does
strip a leading `\u{FEFF}` defensively, but that's only insurance.

### MSVC build tools missing for Rust

If `cargo build` fails with `error: linker 'link.exe' not found`, MSVC isn't
installed. Run the Visual Studio Build Tools installer with the C++ workload.

### `rust-lld` for musl

Cross-compiling to Linux musl from Windows requires a linker. We use the
LLD-based `rust-lld` that ships with rustup. The config in
`src-tauri/.cargo/config.toml` wires it up:

```toml
[target.x86_64-unknown-linux-musl]
linker = "rust-lld"
```

If you see `linker 'cc' not found` errors during musl compile, that file got
removed or the path is wrong.

### Pageant panic noise

The `pageant-0.0.1` crate calls `.unwrap()` on a Windows error when Pageant isn't
running. We wrap the agent probes in `catch_unwind` (Phase 6.2 fix) so this
never reaches the tokio worker. You may still see
`thread 'tokio-rt-worker' panicked …` lines in the console — they're harmless
backtraces from the crate, swallowed by our guard.

### Port 1420 already in use

`tauri dev` runs Vite on 1420. If a previous run left a Node process holding it,
kill it:

```powershell
$conn = Get-NetTCPConnection -LocalPort 1420 -ErrorAction SilentlyContinue
if ($conn) { Stop-Process -Id $conn[0].OwningProcess -Force }
```
