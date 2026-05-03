# winmux

A Windows-native multi-agent terminal for AI coding workflows over SSH.
Inspired by [cmux](https://github.com/manaflow-ai/cmux).

> Status: Early development — Phases 1–5 complete, Phase 6 in progress. Not yet stable.

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — high-level architecture + ASCII diagram
- [docs/MODULES.md](docs/MODULES.md) — what each Rust module + frontend file owns
- [docs/PROTOCOLS.md](docs/PROTOCOLS.md) — JSON-RPC method catalog, named-pipe + TCP framing, HMAC handshake, agent-hook contract, bootstrap protocol
- [docs/CONFIG.md](docs/CONFIG.md) — `workspaces.json`, `known_hosts.json`, `remote-manifest.json` schemas; environment variables
- [docs/CLI.md](docs/CLI.md) — every `winmux` command with examples and exit codes
- [docs/BUILD.md](docs/BUILD.md) — prerequisites, dev / debug builds, Linux cross-compile, common gotchas
- [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) — recipes for adding RPC methods, agent hooks, pane types; logging + commit conventions

## Why

cmux is a macOS-only terminal optimized for working with AI coding agents
(Claude Code, Codex, Cursor) — vertical tabs, splits, agent notifications,
and SSH workspaces. winmux brings the same model to Windows, with an
opinionated stack that values native feel and developer iteration speed.

## Stack

- **Frontend:** [Tauri 2](https://tauri.app), SolidJS (TS), [xterm.js](https://xtermjs.org/) with WebGL
- **Terminal rendering:** xterm.js + [bidi-js](https://github.com/lojjic/bidi-js) for proper Hebrew/Arabic
- **Backend:** Rust — [portable-pty](https://crates.io/crates/portable-pty) (ConPTY/POSIX), [russh](https://crates.io/crates/russh) (SSH client + ssh-agent), tokio
- **CLI:** standalone `winmux` binary speaking JSON-RPC v2 over Named Pipe (Windows) or TCP (remote Linux)

## Features (Phase 1–5)

- Local PTY (PowerShell / cmd / pwsh) with full xterm.js rendering, GPU-accelerated
- SSH workspaces via russh — ssh-agent (OpenSSH for Windows + Pageant), key files, encrypted keys with passphrase prompt, password fallback
- TOFU host key verification with `known_hosts.json`
- Multiple workspaces in a sidebar with persistence (`%APPDATA%\winmux\workspaces.json`)
- Splits: binary tree of horizontal/vertical splits per workspace, draggable dividers, persisted layout
- Hebrew/RTL: bidi-js handles UAX #9 properly — mixed `Hello שלום world` lines display correctly
- CLI: `winmux.exe` with `list-workspaces`, `select-workspace`, `new-workspace`, `delete-workspace`, `send`, `send-key`, `notify`, `tree`, `set-status`, plus `claude-hook` stub
- Toast notifications via Windows WinRT
- Live event-driven UI updates from CLI mutations
- **Settings panel** with five built-in theme presets (Tokyo Night, Dracula, Solarized Dark/Light, Nord), full color customization, font picker, terminal options, and hooks/notifications/updates toggles. Live theme apply via CSS vars; persisted to `%APPDATA%\winmux\settings.json`. *(Phase 9.A)*
- **Update checker** that polls a manifest URL on startup and surfaces a banner with release notes when a newer version is available. No auto-install — just a heads-up. *(Phase 9.B)*

## Install (release)

The MSI installer ships both the GUI app and the `winmux` CLI in one package.

1. Download `winmux_0.1.0_x64_en-US.msi` (or the matching `winmux_0.1.0_x64-setup.exe` if you prefer the NSIS installer) from the Releases page.
2. Double-click → install. Default location: `C:\Program Files\winmux\`.
3. The GUI is launched from Start Menu → "winmux".
4. The CLI lands at `C:\Program Files\winmux\resources\winmux-cli.exe`. To call it as just `winmux` from any terminal, add that directory to your `PATH`:

   ```powershell
   # PowerShell, current user, persistent
   [Environment]::SetEnvironmentVariable(
     'Path',
     "$([Environment]::GetEnvironmentVariable('Path','User'));C:\Program Files\winmux\resources",
     'User'
   )
   ```

   Restart your terminal. PATH auto-registration via WiX is on the roadmap; this step is manual for now.

To uninstall: Settings → Apps → winmux → Uninstall. All files removed; user data in `%APPDATA%\winmux\` is preserved unless you delete it manually.

## Build

### Prerequisites

- Rust (stable), via [rustup](https://rustup.rs/)
- Node.js 18+
- Microsoft C++ Build Tools (Visual Studio 2022 with `Microsoft.VisualStudio.Workload.VCTools`)
- WebView2 runtime (already present on Win10 21H2+ / Win11)
- For cross-compile to Linux CLI: `rustup target add x86_64-unknown-linux-musl`

### Dev

```cmd
cd app
npm install
npm run tauri dev
```

### Standalone debug build

```cmd
cd app
npm run tauri build -- --debug
```

Output: `app\src-tauri\target\debug\app.exe` and `app\src-tauri\target\debug\winmux.exe`.

## Roadmap

- ✅ Phase 1: Local PTY pipeline (Tauri + xterm.js + portable-pty)
- ✅ Phase 1.5: BiDi for Hebrew/RTL via bidi-js (UAX #9)
- ✅ Phase 2: SSH workspaces via russh
- ✅ Phase 3: Multi-workspace sidebar + persistence
- ✅ Phase 4: Splits (binary tree of panes)
- ✅ Phase 5: CLI + JSON-RPC over Named Pipe + agent hook stub
- 🚧 Phase 6: Remote relay — Linux CLI cross-compile, scp upload, reverse SSH tunnel, blocking agent hooks with UI permission flow
- 🔮 Future: WebView2 panes (embedded browser), aarch64-linux CLI, ARM64 Windows build, MSI installer

## Inspirations

- [cmux](https://github.com/manaflow-ai/cmux) — the macOS reference
- [Warp](https://www.warp.dev) — agentic terminal (now open-source under AGPLv3)
- [Tauri](https://tauri.app), [xterm.js](https://xtermjs.org)

## License

GPL-3.0-or-later. See `LICENSE`.
