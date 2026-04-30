# `winmux` CLI

A small client that talks to the running winmux app over JSON-RPC. The same
binary works on Windows (over a per-user named pipe) and on a remote Linux server
(over a reverse SSH tunnel + HMAC handshake — see [PROTOCOLS.md](./PROTOCOLS.md)).

## Synopsis

```
winmux [--raw] [--quiet] <command> [args...]
```

| Global flag | Default | Effect |
|---|---|---|
| `--raw` | off | Print the JSON-RPC result as a single line of compact JSON instead of pretty. |
| `--quiet` | off | On success, print nothing. Errors still go to stderr. |

## Transport selection

| Where | Default transport | Override |
|---|---|---|
| Windows (no `WINMUX_SOCKET_ADDR`) | Named pipe `\\.\pipe\winmux-<USER>` | `WINMUX_PIPE_PATH` |
| Anywhere with `WINMUX_SOCKET_ADDR` set | TCP to that address; HMAC handshake using `WINMUX_TUNNEL_TOKEN` | env vars only |
| Linux (no `WINMUX_SOCKET_ADDR`) | the CLI tries to load `~/.winmux/run/last.env`; if it has the vars, TCP. Otherwise: error. | populate `last.env` (auto-done by SSH connect) or set the env vars manually |

## Exit codes (general)

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | client-side or server-side RPC error (message on stderr) |

`claude-hook` overrides these — see its section.

## Commands

### `list-workspaces`

```
winmux list-workspaces
```

Print the full `WorkspacesFile` (see [CONFIG.md](./CONFIG.md#workspacesjson)).

```
$ winmux list-workspaces
{
  "version": 1,
  "active_workspace_id": "w_aaa",
  "workspaces": [ … ]
}
```

### `select-workspace`

```
winmux select-workspace --id <workspace_id>
```

Sets the active workspace. The app's sidebar and main view update live.

```
$ winmux select-workspace --id w_18abc
{ "ok": true, "active": "w_18abc" }
```

### `new-workspace`

```
winmux new-workspace --name <name>
                     [--type local|ssh] [--shell <path>]
                     [--host <host>] [--user <user>] [--port <port>]
                     [--key-path <path>]
                     [--cwd <path>] [--color <#hex>]
```

For `--type local` (default): pass `--shell` if you don't want the auto-detected
default (pwsh → powershell → cmd).

For `--type ssh`: `--host` and `--user` are required. `--port` defaults to 22.
`--key-path` is optional (the auth chain falls back to ssh-agent / `~/.ssh/id_*`
/ password).

```
$ winmux new-workspace --name "Local CMD" --type local --shell cmd.exe --color "#5cd87f"
{ "id": "w_18ac1...", "name": "Local CMD", … }
```

### `delete-workspace`

```
winmux delete-workspace --id <workspace_id>
```

Kills any running sessions in that workspace and removes it.

### `send`

```
winmux send --pane <pane_id> --data '<text>'
```

Writes raw bytes to a connected pane. The pane must already be connected
(via the GUI Connect button or by wiring up your own automation). Find pane IDs
via `winmux tree` or `winmux list-workspaces`.

```
$ winmux send --pane p_18abc_0 --data 'echo hello'
$ winmux send-key --pane p_18abc_0 --key enter
```

### `send-key`

```
winmux send-key --pane <pane_id> --key <name>
```

Translates a named key to the right escape bytes and writes them. Recognized:
`enter`, `tab`, `ctrl-c`, `ctrl-d`, `ctrl-z`, `ctrl-l`, `esc`, `backspace`, `up`,
`down`, `left`, `right`, `home`, `end`. Unknown keys are sent verbatim.

### `notify`

```
winmux notify --title <title> [--body <body>] [--workspace-id <id>]
```

Show a Windows toast and add to the in-memory notifications list.

```
$ winmux notify --title "build done" --body "all green"
{ "ok": true, "id": 0 }
```

### `tree`

```
winmux tree [--workspace-id <id>]
```

Print the layout tree of one workspace (active by default).

```
$ winmux tree
{
  "workspace_id": "w_aaa",
  "name": "runner1",
  "layout": { "kind": "pane", "pane_id": "p_…", "connection": { … } }
}
```

### `set-status`

```
winmux set-status --pane <pane_id> --text <text>
```

Set a transient status text on the pane's header in the UI.

### `claude-hook`

```
winmux claude-hook <subcommand> [< stdin-json]
```

Designed for use as a hook command from an AI agent (Claude Code, etc).
Reads JSON from stdin, sends a `feed.push` over the tunnel, and waits for the
user's decision (for blocking subcommands) or returns immediately
(for passive ones).

#### Subcommands

| Subcommand | Behavior |
|---|---|
| `tool-permission` | **Blocks** until Allow / Deny / timeout. |
| `pre-tool-use` | **Blocks** until Allow / Deny / timeout. |
| `session-start`, `session-active`, `session-stop`, `session-idle`, `notification`, `prompt-submit`, `session-end` | Passive: emits a feed card + toast; returns immediately. |
| Any other name | Treated as passive. |

#### stdin payload (suggested fields — all optional)

- `tool` (e.g. `"Bash"`)
- `command` (e.g. `"rm -rf /tmp/test"`) — used as the title for tool-permission
- `summary` / `description` / `body` / `reason` — first non-empty becomes the
  card's body
- `wait_timeout_seconds` (1..600) — server-side wait before timing out

If the payload doesn't have any of those, the title is `agent: <subcommand>` and
the summary is the JSON-stringified payload truncated to ~280 chars.

#### Exit codes

| `decision` | Exit | Meaning |
|---|---|---|
| `allow` / `passive` | 0 | proceed |
| `deny` | 1 | abort |
| `timeout` | 2 | timed out without decision |
| anything else / wire error | 3 | look at stderr + `%APPDATA%\winmux\debug.log` |

#### Examples

Block on tool permission with a 5 s timeout:

```bash
echo '{"tool":"Bash","command":"rm -rf /tmp/test","wait_timeout_seconds":5}' \
  | ~/.winmux/bin/winmux claude-hook tool-permission
echo "exit=$?"
```

Passive notification:

```bash
echo '{"summary":"Claude finished thinking"}' \
  | ~/.winmux/bin/winmux claude-hook session-idle
```

## Where each command works

| Command | Local (Windows pipe) | Remote (TCP via tunnel) | Requires connected session? |
|---|---|---|---|
| `list-workspaces`, `tree`, `select-workspace`, `new-workspace`, `delete-workspace`, `notify`, `set-status` | yes | yes | no |
| `send`, `send-key` | yes | yes | **yes** — pane must be connected |
| `claude-hook` | yes (will use the local pipe) | yes (the typical use) | no for passive; for blocking, the app must be running and the user must respond |
