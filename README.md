# winmux

A Windows-native multi-agent terminal for AI coding workflows over SSH.
Inspired by [cmux](https://github.com/manaflow-ai/cmux).

[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPL%203.0%2B-blue.svg)](LICENSE)
[![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%2B-0078D6.svg)](#)
[![Built with: Tauri 2 + SolidJS + Rust](https://img.shields.io/badge/built%20with-Tauri%202%20%2B%20SolidJS%20%2B%20Rust-success.svg)](#stack)

> **Status:** v0.1.0 — first public preview. Daily-driver candidate, not yet stable for production-critical workflows. See the Roadmap below for what's done vs. what's coming.

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

## Features

**Local & SSH terminals**
- Local PTY (PowerShell / cmd / pwsh) with full xterm.js rendering, GPU-accelerated WebGL
- SSH workspaces via [russh](https://crates.io/crates/russh) — OpenSSH agent + Pageant, key files (encrypted keys prompt for passphrase), password fallback
- TOFU host key verification with `known_hosts.json`; clear mismatch warnings
- Splits: binary tree of horizontal/vertical panes per workspace, draggable dividers
- Hebrew/Arabic RTL via [bidi-js](https://github.com/lojjic/bidi-js) (UAX #9) — mixed `Hello שלום world` lines display correctly
- `tmux` persistence on connect (optional): detach instead of disconnect; sessions survive

**AI-agent workflow**
- Claude Code hook integration: agent permission requests + lifecycle events stream to the desktop UI as cards with Allow/Deny buttons (env-gated by `WINMUX_PANE_ID` so unrelated terminals aren't affected)
- Browser pane that can serve `localhost:port` over an auto-managed reverse SSH tunnel
- Bundled `winmux-mcp` server: MCP-aware agents (Claude Code, Cursor, etc.) drive winmux's browser panes natively
- "Smart connect" dropdown: default / tmux / plain / specific cwd / specific command / `claude` / `claude --resume` from a session browser

**Setup automation** *(new in v0.1.0)*
- **Smart connect wizard** — import from `~/.ssh/config`, auto-detect keys with fingerprints, one-click permission auto-fix via `icacls`, in-modal "Test connection" with method/stage/hint diagnostics
- **Server provisioning wizard** — bootstrap a fresh server end-to-end: package update → user creation → key deploy → install Node/Python/Docker → install Claude Code → register hooks. Three built-in profiles (default / hardened / minimal); profile editor on disk. Initial password DPAPI-wrapped per user/machine

**UX**
- Five theme presets (Tokyo Night, Dracula, Solarized Dark/Light, Nord) + per-color customization + live apply
- Font picker (system fonts + custom name + Google-Fonts-style web font URL); live UI + terminal font-size slider
- Localization (en / he / ar / ru) with RTL/LTR live switch
- Toast notifications via Windows WinRT
- Update checker (manifest fetch, no auto-install)

**CLI + RPC**
- Standalone `winmux` binary speaking JSON-RPC v2 over Named Pipe (Windows) or TCP (remote Linux over an HMAC-SHA256-authenticated reverse tunnel)
- `winmux dev` introspection: state snapshot, debug-log tail, console-event tail, bug-report capture
- `winmux settings show/set/preset/export/import` for scripting your config

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

Shipped in v0.1.0 — Phases 1 through 14.A:

- ✅ Local PTY pipeline (Tauri + xterm.js + portable-pty + WebGL)
- ✅ BiDi for Hebrew/RTL via bidi-js (UAX #9)
- ✅ SSH workspaces via russh, agent + key + password
- ✅ Multi-workspace sidebar with persistence + splits (binary tree)
- ✅ CLI + JSON-RPC over Named Pipe + reverse SSH tunnel
- ✅ MSI + NSIS installers with bundled CLI
- ✅ Remote-Linux CLI bootstrap (SFTP upload + symlink)
- ✅ HMAC-SHA256 challenge-response over the reverse tunnel
- ✅ Agent feed + permission cards + Claude Code hook contract
- ✅ Notes / quick-capture
- ✅ Browser panes via iframe + postMessage bridge
- ✅ winmux-mcp standalone MCP server (15 tools, browser automation)
- ✅ Settings panel + 5 theme presets + live theme apply
- ✅ Update checker (manifest fetch)
- ✅ tmux persistence on connect
- ✅ Localization (en / he / ar / ru) + RTL/LTR switch
- ✅ Smart Connect with Claude Code launcher + session browser
- ✅ Smart connect wizard (ssh_config import, key auto-detect, perms fix, test)
- ✅ Server provisioning wizard (fresh-server bootstrap)

Coming next:

- 🔮 PATH auto-registration in the WiX installer
- 🔮 ARM64 Windows build
- 🔮 aarch64-linux CLI
- 🔮 Code-signing for the MSI / NSIS
- 🔮 Auto-update via signed manifest + delta downloads

## Inspirations

- [cmux](https://github.com/manaflow-ai/cmux) — the macOS reference
- [Warp](https://www.warp.dev) — agentic terminal (now open-source under AGPLv3)
- [Tauri](https://tauri.app), [xterm.js](https://xtermjs.org)

## Security

Found a vulnerability? See [SECURITY.md](SECURITY.md) — please report
privately to the email listed there rather than opening a public issue.

## License

GPL-3.0-or-later. See `LICENSE`.
