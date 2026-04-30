# winmux

A Windows-native multi-agent terminal for AI coding workflows over SSH.
Inspired by [cmux](https://github.com/manaflow-ai/cmux).

> Status: Early development — Phases 1–5 complete, Phase 6 in progress. Not yet stable.

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
