# Architecture

## The model

winmux organizes terminal work into a tree:

```
window  →  workspace  →  layout (binary tree of splits)  →  pane (leaf)  →  session (PTY or SSH channel)
```

A **workspace** is the persistent unit (named, color-coded, with an SSH/local connection
template). Its **layout** is a binary tree where each node is either a `pane` (leaf — has
its own connection + session) or a `split` (horizontal/vertical divider with a ratio and
two children). A **pane** is the visible cell on screen; its **session** is the running
PTY (local) or SSH channel (remote) that backs it. Sessions are ephemeral — only
workspaces and their layouts are persisted.

## Top-level processes

| Process | Path | Role |
|---|---|---|
| `app.exe` | `src-tauri/target/debug/app.exe` | The Tauri app: WebView2 frontend (SolidJS) + Rust backend (PTY/SSH/RPC). Long-running. |
| `winmux.exe` | same dir | Windows-side CLI client. Talks to the running app over a named pipe. |
| `winmux-linux-x64` | bundled in resources, copied to `~/.winmux/bin/` on the SSH server | Linux CLI client. Talks to the Windows app via a reverse SSH tunnel. |

## Communication channels

```
┌──────────────────────────────────────────┐
│             Windows host                 │
│                                          │
│  ┌────────────┐   Tauri commands +       │
│  │  SolidJS   │   events (in-process)    │
│  │  Frontend  │ ◄══════════════════════╗ │
│  └─────┬──────┘                        ║ │
│        │ xterm.js writes               ║ │
│        ▼                               ║ │
│  ┌────────────┐ pty:data / pty:exit    ║ │
│  │            │ feed:item-added /      ║ │
│  │   Rust     │ resolved / pane:status ║ │
│  │  backend   │ ──events──►            ║ │
│  │ (lib.rs)   │                        ║ │
│  └─┬─┬─┬─┬────┘                        ║ │
│    │ │ │ │                             ║ │
│    │ │ │ └─ portable-pty ──► local shell processes (ConPTY)
│    │ │ │                                 │
│    │ │ └─ Named Pipe RPC ◄── winmux.exe (local CLI)
│    │ │      \\.\pipe\winmux-<user>       │
│    │ │                                   │
│    │ └─ russh ──── SSH ──┐                │
│    │                    │                │
│    └─ tunnel.rs bridge ◄┤                │
│       (forwarded-tcpip) │                │
└─────────────────────────┼────────────────┘
                          │
                          │ SSH
                          │
┌─────────────────────────┼────────────────┐
│            Remote Linux server           │
│                         │                │
│        sshd ────────────┘                │
│         │                                │
│         ├── shell (with WINMUX_*         │
│         │    env vars + last.env file)   │
│         │                                │
│         ├── 127.0.0.1:<remote_port>      │
│         │   (reverse-forwarded to        │
│         │    Windows via russh)          │
│         │     ▲                          │
│         │     │  TCP                     │
│         └─────┴── winmux-linux-x64       │
│                   (~/.winmux/bin/)       │
└──────────────────────────────────────────┘
```

- **Frontend ⇄ backend (in-process):** SolidJS calls Rust via `invoke('cmd_name', {...})`
  for synchronous-style RPC. Backend pushes async updates via `app.emit('event:name', ...)`
  which the frontend subscribes to with `listen()`. PTY data flows backend → frontend
  exclusively as events; user keystrokes flow frontend → backend as `pty_write` invokes.
- **CLI ⇄ app on Windows:** newline-delimited JSON-RPC v2 over a per-user Named Pipe at
  `\\.\pipe\winmux-<user>`. ACL is whatever Windows assigns by default to a pipe
  created by the user's process — effectively user-only. No HMAC needed (the pipe
  ACL is the auth boundary).
- **CLI on remote ⇄ app on Windows:** TCP via a russh **reverse tunnel** opened on
  every SSH workspace connection. The CLI dials `127.0.0.1:<remote_port>`; the SSH
  session forwards that to a russh client channel; our `tunnel.rs` bridges the channel
  to a fresh Named Pipe client connection. Auth is **HMAC-SHA256 challenge-response**
  (Phase 6.4) — the shared token never travels in cleartext.
- **Agent hooks:** the Linux CLI's `claude-hook <subcommand>` builds a `feed.push`
  JSON-RPC request from stdin, sends it through the tunnel, and (for blocking kinds)
  waits for a server-side decision before returning an exit code.

## Persistence

| File | Path | Written by |
|---|---|---|
| `workspaces.json` | `%APPDATA%\winmux\` | `lib.rs` on every mutation; loaded once at `setup()`. |
| `known_hosts.json` | `%APPDATA%\winmux\` | `lib.rs::SshClient::check_server_key` on first/match/replace. |
| `debug.log` | `%APPDATA%\winmux\` | `dlog()` everywhere. Append-only. |
| `last.env` | `~/.winmux/run/` (remote) | Written by tunnel bootstrap before the shell starts; CLI on Linux loads it as a fallback when sshd strips per-channel env vars. |

## Bundled resources

| Resource | Origin |
|---|---|
| `resources/winmux-linux-x64` | Cross-compiled by `scripts/build-linux-cli.ps1` (musl, static-PIE ELF). Bundled into `app.exe` via `tauri.conf.json` `bundle.resources`. Auto-uploaded to remote on first SSH connect by `remote_bootstrap.rs`. |
| `resources/remote-manifest.json` | Generated next to the binary. Carries the SHA-256 + size + build timestamp. The manifest path/hash schema is keyed by triple (`x86_64-linux`; `aarch64-linux` reserved for later). UTF-8 **without** BOM (writer hardened in 6.2). |

## Phase history

- 1: Local PTY pipeline.
- 1.5: BiDi via bidi-js (UAX #9).
- 2: SSH workspaces via russh.
- 3: Multi-workspace sidebar + persistence.
- 4: Splits (binary tree).
- 5: CLI + JSON-RPC over Named Pipe + agent hook stub.
- 6.1: CLI cross-compile to Linux musl.
- 6.2: Remote-bootstrap of the Linux CLI via SFTP.
- 6.3: Reverse SSH tunnel + per-pane TCP↔Pipe relay.
- 6.4: HMAC-SHA256 challenge-response (token never on wire).
- 6.5: Blocking agent hooks with `feed.push` + UI Allow/Deny.
