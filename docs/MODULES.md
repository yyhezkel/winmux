# Modules

Per-file responsibilities. Read this when you need to find where something lives.

## Rust backend (`app/src-tauri/src/`)

### `main.rs` (6 lines)

Tauri's mandatory binary entry point. Sets the Windows subsystem flag in release
(no console window) and calls `app_lib::run()`. Don't put logic here.

### `lib.rs` (~1760 lines)

The backend's "everything else" module. By far the largest. Sections (in order):

- **Session types** — `Session::Local|Ssh`, `LocalSession`, `SshSession`, `SshCmd`.
  PTY/SSH I/O state lives in here.
- **`AppState`** — the single managed Tauri state. Holds:
  `sessions` (`session_id` → `Session`), `pane_sessions` (`pane_id` → `session_id`),
  `workspaces` (the persisted file as in-memory truth), `load_state` (poison flag),
  `notifications`, `pane_status`, `feed`. All fields are `Arc<Mutex<…>>` so the struct
  derives `Clone` and can be handed to the RPC server task.
- **`FeedItem` / `FeedStore` / `FeedItemState`** — Phase 6.5 agent-hook feed.
- **Workspace data model** — `Connection` (`local | ssh`), `LayoutNode` (`pane | split`),
  `Workspace`, `WorkspacesFile`, `CreateInput`. `LayoutNode::Pane` carries its
  `connection` so each leaf can target a different host.
- **ID helpers** — `next_session_id`, `new_pane_id`, `new_split_id`, `new_workspace_id`.
- **Persistence** — `config_dir`, `config_path`, `dlog`, `save_to_disk` (atomic via
  `.tmp` + rename + fsync), `load_from_disk` (with on-the-fly migration of legacy
  `connection` fields into single-pane layouts), `persist` (gates on `LoadState::Loaded`
  to refuse clobbering existing data after a parse failure).
- **Tree ops** — `find_pane_connection`, `collect_panes`, `split_pane_in`,
  `close_pane_in`, `set_split_ratio_in`. Pure functions on `LayoutNode`.
- **PTY plumbing** — `pick_default_shell`, `emit_data` (UTF-8 boundary-safe; buffers
  partial multibyte sequences), `emit_exit`, `emit_pane_status_event`,
  `schedule_status_clear`, `cleanup_session_maps`.
- **`spawn_local_pty`** — opens a ConPTY pair, spawns the shell, starts a thread that
  reads bytes, emits `pty:data`, and removes itself from the maps when the child exits.
- **Known-hosts (TOFU)** — `KnownHost`, `KnownHostsFile`, `load_known_hosts`,
  `save_known_hosts`, `iso_now`, `HostCheckOutcome`.
- **`SshClient` / `Handler`** — implements `russh::client::Handler` with our TOFU
  policy (`check_server_key`) and the Phase 6.3 forwarded-tcpip bridge handoff
  (`server_channel_open_forwarded_tcpip` → `tunnel::spawn_bridge`).
- **`try_authenticate` / `try_agent_auth`** — auth chain: ssh-agent (OpenSSH +
  Pageant, both wrapped in `catch_unwind` to absorb upstream panics) → explicit key
  file (with optional passphrase) → default `~/.ssh/id_*` keys → password.
- **`spawn_ssh`** — connects, authenticates, runs bootstrap (best-effort), opens the
  reverse tunnel via `tcpip_forward(0)`, writes the env file via `tunnel`, opens the
  shell channel with `set_env` for `WINMUX_*`, request_pty, request_shell, spawns the
  channel-pump task.
- **Tauri commands** — every `#[tauri::command]` lives here:
  `workspaces_load`, `workspace_create`, `workspace_rename`, `workspace_delete`,
  `workspace_set_active`, `workspace_split`, `workspace_close_pane`,
  `workspace_set_split_ratio`, `pane_connect`, `pane_disconnect`,
  `pty_write`, `pty_resize`, `notifications_list`, `notifications_clear`,
  `pane_status_get`, `feed_list`, `feed_decide`. The `decide_feed` helper is shared
  between `feed_decide` and the RPC `feed.decide` path.
- **`run()`** — the Tauri builder: registers `AppState`, loads workspaces in `setup()`,
  spawns the named-pipe RPC server, registers the `invoke_handler` list.

### `rpc_server.rs` (~487 lines)

The JSON-RPC v2 server on the user's named pipe. `pipe_name()` derives the path
from `$USERNAME` (or `whoami::username()` fallback). `run()` accepts pipe instances
in a loop and spawns `handle_client` per connection. `handle_client` reads
newline-delimited JSON requests, dispatches via `dispatch()`, and writes a
JSON-RPC response per request. The dispatcher contains the canonical method
catalog: `list-workspaces`, `select-workspace`, `new-workspace`, `delete-workspace`,
`send`, `send-key`, `notify`, `tree`, `set-status`, `feed.push`, `feed.decide`.
`translate_key` maps human-readable key names (`enter`, `ctrl-c`, `up`) to bytes.
`show_toast` spawns a thread that uses `notify_rust` for Windows toasts.

### `remote_bootstrap.rs` (~285 lines)

Phase 6.2: ensures the remote has the right `winmux-linux-x64` binary at
`~/.winmux/bin/`. Reads `resources/remote-manifest.json` (with BOM strip — defensive),
detects the remote arch via `uname -s -m`, hashes any existing remote binary, and if
mismatched/missing uploads via SFTP (russh-sftp), then chmod 0755 and refreshes the
`~/.winmux/bin/winmux` symlink. Heavy `dlog()` instrumentation throughout — that
visibility is what unlocked the BOM diagnosis.

### `tunnel.rs` (~223 lines)

Phase 6.3 + 6.4: the reverse-tunnel relay. `bridge_to_pipe` is the per-channel
worker — it owns one `BufStream` over a russh forwarded-tcpip channel and runs
the **HMAC-SHA256 challenge-response** (`perform_handshake` — sends 32-byte hex
nonce, expects HMAC of nonce keyed with the shared token, verifies in
constant-time via `Hmac::verify_slice`). After OK, opens a fresh Named Pipe
client and runs `tokio::io::copy_bidirectional` until either side closes.
`generate_token` produces a 32-char alphanumeric token per SSH connection.
`write_remote_env_file` is the SFTP-less fallback: heredoc-write
`~/.winmux/run/last.env` via `cat > ... <<'__WINMUX_EOF__'` so the CLI on the
remote can pick up `WINMUX_SOCKET_ADDR`/`WINMUX_TUNNEL_TOKEN`/`WINMUX_PANE_ID`
even on sshd setups that strip per-channel env vars.

## Rust CLI (`app/src-tauri/cli/src/main.rs` — ~571 lines)

A standalone binary, separate Cargo workspace member. **Does not** depend on Tauri,
russh, portable-pty — only `clap`, `tokio` (subset features), `serde_json`, `whoami`,
`hmac`, `sha2`. This keeps the binary small (~2 MB static-musl on Linux) and lets
the same source compile cleanly for both Windows MSVC and Linux musl.

- `Cli` / `Cmd` — clap derive types. Subcommands match the RPC method names
  (dash-cased): `list-workspaces`, `select-workspace`, etc.
- `default_pipe_name()` (Windows-only) — `\\.\pipe\winmux-<USER>`.
- `rpc_call` — picks transport: TCP if `WINMUX_SOCKET_ADDR` is set
  (also runs `load_fallback_env_file` first), else named pipe on Windows.
- `rpc_via` — generic over the stream. For TCP: runs the HMAC handshake first.
  Then writes a single newline-delimited JSON-RPC request, reads one response.
- `perform_handshake` (client side) — reads `WINMUX-CHALLENGE <hex>`, computes
  HMAC-SHA256, writes `WINMUX-RESPONSE <hex>`, expects `WINMUX-OK` or fails.
- `derive_hook_title` / `derive_hook_summary` — heuristic title/summary for
  the agent feed cards from the stdin JSON.
- `Cmd::ClaudeHook` — the only complex command. Reads stdin, parses JSON,
  picks blocking vs passive based on subcommand name, sends `feed.push`,
  blocks on the response (server holds the connection during the wait),
  returns exit code 0/1/2/3 based on `decision`.

## Frontend (`app/src/`)

### `index.tsx` (5 lines)

SolidJS render entry; mounts `<App />` into `#root`.

### `App.tsx` (~619 lines)

Top-level orchestrator. Holds the state, wires the IPC, renders the layout.

- State signals: `file` (the `WorkspacesFile`), `activePaneId`, `pendingPwFor`,
  `pendingPassphraseFor`, `pendingHostTrust`, `paneStatus`, `paneStatusText`,
  `feedItems`, plus a `tick` signal used as a poor-man's reactivity nudge
  for non-reactive Maps.
- Maps (non-reactive): `terms` (`pane_id` → `TerminalInstance`),
  `paneToSession` and `sessionToPane` (bidirectional).
- `connectPane` — the meatiest path: invokes `pane_connect`, parses error strings
  for `KEY_PASSPHRASE_REQUIRED:`, `KEY_PASSPHRASE_BAD:`, `UNKNOWN_HOST:`,
  `HOST_KEY_MISMATCH:`, sets the right pending state. Retries are user-driven
  (passphrase prompt, host-trust dialog).
- `onMount` listeners: `pty:data`, `pty:exit`, `feed:item-added`,
  `feed:item-resolved`, `pane:status`, `workspaces:changed`. Plus a global
  keydown for Ctrl+Shift+D/E/W (split right / split down / close pane).

### `Sidebar.tsx` (91 lines)

The left rail. Renders the workspace list with a color dot, name, and a `L`/`S`/N
badge derived from the workspace's layout (single-pane local, single-pane SSH,
or N-pane split). Right-click opens an inline menu (Rename / Disconnect / Delete).

### `LayoutView.tsx` (97 lines)

Recursive renderer for the layout tree. A node is either a `<PaneView>` (leaf) or
a `<SplitView>` (a flex container with two `LayoutView` children + a `<Divider>`).
Everything passes through to `PaneView` props is forwarded by spread (`...s.all`)
to keep deeply-nested calls readable.

### `PaneView.tsx` (~223 lines)

A single pane. Header has connection info, optional status text,
disconnect/split-right/split-down/close buttons. Body is either:
- the `<host-trust>` dialog (unknown host or mismatch — shows fingerprint, with
  Trust / Cancel),
- the passphrase prompt (when SSH key is encrypted),
- the password prompt (when key auth fails),
- the default Connect button,
- or the terminal slot — a `<div>` we move the matching `TerminalInstance.container`
  into via `appendChild` on `onMount` and remove from on `onCleanup`. The
  `TerminalInstance` itself lives in `App.tsx`'s `terms` map and is preserved
  across PaneView mounts (so split/close doesn't lose scrollback).

### `Divider.tsx` (60 lines)

The draggable splitter. `mousedown` → `mousemove` (rAF-throttled) emitting `onDrag`
with the current ratio → `mouseup` emitting `onCommit`. App.tsx commits the ratio
via `workspace_set_split_ratio`.

### `CreateWorkspaceModal.tsx` (~151 lines)

Simple modal with name / type / shell or host+user+port+key path / color.
Calls back to `App.tsx::handleCreate` which invokes `workspace_create`.

### `FeedPanel.tsx` (62 lines)

Phase 6.5 agent feed cards. Top-right floating stack. Each card has a kind badge,
title, summary, dismiss `×`, and (for `pending && blocking` items) Allow/Deny
buttons. Resolved items get a verdict badge (`ALLOWED`/`DENIED`/`TIMEDOUT`/`PASSIVE`)
and auto-fade after 3 s (the auto-dismiss is scheduled in `App.tsx`).

### `terminalInstance.ts` (94 lines)

`TerminalInstance` class. Owns one xterm.js `Terminal` plus a `FitAddon` and
optional `WebglAddon`. Holds a detached `<div>` (`container`) that callers move
between PaneView slots. `attach(sessionId)` wires `term.onData` → `pty_write`
and starts a `ResizeObserver` that calls `pty_resize`. `writeData` runs the
incoming string through `bidi.ts` before writing to xterm.

### `bidi.ts` (48 lines)

UAX #9 wrapper around `bidi-js`. Splits the chunk on ANSI escapes, runs
`getEmbeddingLevels` + `getReorderSegments` + `getMirroredCharactersMap` per text
segment, applies mirrors then reorder flips. Result: mixed `Hello שלום world` lines
display correctly without xterm.js needing native BiDi.

### `types.ts` (72 lines)

Shared frontend types: `Connection`, `SplitDirection`, `LayoutNode`, `Workspace`,
`WorkspacesFile`, `PtyDataEvent`, `PtyExitEvent`, `FeedItem`, `FeedItemState`,
`FeedResolvedEvent`. Plus `collectPanes`, `findPane`, `describeConnection` helpers.

## Build glue

### `app/scripts/build-linux-cli.ps1`

Cross-compiles the CLI for `x86_64-unknown-linux-musl` via `cargo build --release`,
copies the binary to `src-tauri/resources/winmux-linux-x64`, computes its
SHA-256, and writes `remote-manifest.json` using
`[System.IO.File]::WriteAllText` with `UTF8Encoding(emit_bom: false)` —
**not** `Set-Content -Encoding utf8`, which on Windows PowerShell 5.1 would add a
BOM and break `serde_json::from_str`.

### `app/src-tauri/.cargo/config.toml`

Sets `linker = "rust-lld"` for the two musl targets so we don't need a separate
GNU linker for cross-compile from Windows.

### `app/src-tauri/Cargo.toml`

Workspace root: members `.` (the Tauri lib + `app` bin) and `cli` (the standalone
`winmux` bin). Phase 6 split the CLI out specifically because Tauri/webview2-com
deps don't compile for Linux.

### `app/src-tauri/tauri.conf.json`

`bundle.resources` lists `resources/winmux-linux-x64` and `resources/remote-manifest.json`
so they ship next to `app.exe`. Window starts at 1100×700.
