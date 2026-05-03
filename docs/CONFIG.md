# Configuration

File formats and environment variables. All paths use `dirs::config_dir()` on the
Rust side, which is `%APPDATA%` (= `C:\Users\<user>\AppData\Roaming`) on Windows.

## Files

All under `%APPDATA%\winmux\`:

| File | Written by | Read by |
|---|---|---|
| `workspaces.json` | `lib.rs::save_to_disk` (atomic temp+rename, `fsync`) | `lib.rs::load_from_disk` at `setup()` |
| `notes.json` | `notes.rs::save_notes_to_disk` | `notes.rs::load_notes_from_disk` at `setup()` |
| `settings.json` | `settings.rs::save_to_disk` (Phase 9.A) | `settings.rs::load_from_disk` at `setup()` |
| `known_hosts.json` | `lib.rs::save_known_hosts` after host key match/replace | `lib.rs::load_known_hosts` per SSH connect |
| `debug.log` | `lib.rs::dlog` (append-only, all modules) | humans |

Plus, on each SSH-connected remote:

| File | Path | Written by | Read by |
|---|---|---|---|
| `last.env` | `~/.winmux/run/last.env` (mode 0600) | `tunnel::write_remote_env_file` | `cli/main.rs::load_fallback_env_file` |
| `winmux-linux-x64` | `~/.winmux/bin/` | bootstrap SFTP upload | the CLI binary itself; symlink target |
| `winmux` (symlink) | `~/.winmux/bin/` | bootstrap | the user's PATH if they add it |

## `workspaces.json`

The persistent workspaces file.

### Schema

```ts
type WorkspacesFile = {
  version: number;                       // currently 1
  active_workspace_id: string | null;
  workspaces: Workspace[];
};

type Workspace = {
  id: string;                            // "w_<hex_nanos>"
  name: string;
  color?: string;                        // "#7aa2f7"
  cwd?: string;                          // optional starting cwd for local panes
  // legacy field — folded into layout on load if present
  connection?: Connection;
  layout?: LayoutNode;
};

type Connection =
  | { type: "local"; shell?: string }    // shell path; default = pwsh / powershell / cmd
  | { type: "ssh"; host: string; user: string; port: number; key_path?: string };

type LayoutNode =
  | { kind: "pane"; pane_id: string; connection: Connection }
  | {
      kind: "split";
      split_id: string;
      direction: "horizontal" | "vertical";
      first: LayoutNode;
      second: LayoutNode;
      ratio: number;                     // [0.05, 0.95]
    };
```

### Migration

Workspaces written before Phase 4 had a top-level `connection` field and no
`layout`. On load, `lib.rs::load_from_disk` wraps each such workspace's
connection into a single `pane` node with a freshly-generated `pane_id` and
saves the migrated file back. The legacy `connection` field is then `None`
(skipped on serialization).

### Example

```json
{
  "version": 1,
  "active_workspace_id": "w_18abf123abc",
  "workspaces": [
    {
      "id": "w_18abf123abc",
      "name": "Local PowerShell",
      "color": "#7aa2f7",
      "layout": {
        "kind": "pane",
        "pane_id": "p_18abf124def_0",
        "connection": { "type": "local" }
      }
    },
    {
      "id": "w_18abf999999",
      "name": "runner1",
      "color": "#7aa2f7",
      "layout": {
        "kind": "split",
        "split_id": "sp_18abff_0",
        "direction": "horizontal",
        "ratio": 0.5,
        "first": {
          "kind": "pane",
          "pane_id": "p_aaa",
          "connection": {
            "type": "ssh",
            "host": "161.97.93.172",
            "user": "runner",
            "port": 22,
            "key_path": "C:\\Users\\me\\.ssh\\runner_key"
          }
        },
        "second": {
          "kind": "pane",
          "pane_id": "p_bbb",
          "connection": {
            "type": "ssh",
            "host": "161.97.93.172",
            "user": "runner",
            "port": 22
          }
        }
      }
    }
  ]
}
```

### Safety

If the file fails to parse on startup, `LoadState` is set to `Failed` and
`persist()` refuses to write thereafter — so a corrupted file is **not** silently
clobbered with empty state. The user has to fix the file and restart.

## `known_hosts.json`

TOFU host-key store (Phase 6.4 — written by `lib.rs::SshClient::check_server_key`).

### Schema

```ts
type KnownHostsFile = {
  hosts: { [hostPort: string]: KnownHost };  // key = "host:port", e.g. "1.2.3.4:22"
};

type KnownHost = {
  type: string;            // ssh-key algorithm name, e.g. "ssh-ed25519"
  fingerprint: string;     // "SHA256:..." (server pubkey fingerprint)
  first_seen: string;      // RFC 3339 UTC, e.g. "2026-04-30T13:04:40Z"
  last_seen: string;       // RFC 3339 UTC
};
```

### Behavior

- First connect to a `host:port` — if `accept_unknown_host=true` was passed
  (= the user clicked Trust on the dialog), record the entry and proceed.
  Otherwise the connection is rejected with `UNKNOWN_HOST:<target>:<keytype>:<fingerprint>`.
- Subsequent connects with matching fingerprint — silent, just bumps `last_seen`.
- Mismatched fingerprint — connection rejected with
  `HOST_KEY_MISMATCH:<target>:<keytype>:<old_fingerprint>:<new_fingerprint>` unless
  the user explicitly clicked Replace, which sets `accept_unknown_host=true` and
  causes the new fingerprint to overwrite.

## `remote-manifest.json`

Bundled resource describing the cross-compiled CLI binaries available for upload.

### Schema

```ts
type RemoteManifest = {
  [triple: string]: {                    // e.g. "x86_64-linux"
    path: string;                        // relative to resources/, e.g. "winmux-linux-x64"
    sha256: string;                      // lowercase hex
    size: number;                        // bytes
    built_at: string;                    // ISO 8601 UTC
  };
};
```

Currently only `x86_64-linux` is shipped. `aarch64-linux` is reserved.

### Encoding

UTF-8 **without** BOM. The writer (`scripts/build-linux-cli.ps1`) uses
`[System.IO.File]::WriteAllText($path, $json, [System.Text.UTF8Encoding]::new($false))`
because Windows PowerShell 5.1's `Set-Content -Encoding utf8` adds a BOM and
`serde_json::from_str` rejects it with `"expected value at line 1 column 1"`.
The reader (`remote_bootstrap::read_manifest`) also strips a leading `\u{FEFF}`
defensively, so a future regression in the writer doesn't silently break
bootstrap again.

## `last.env` (remote)

Plain `KEY=value` per line, one variable per line, written via SSH heredoc to
`~/.winmux/run/last.env` with mode 0600. Loaded by the Linux CLI's
`load_fallback_env_file` if `WINMUX_SOCKET_ADDR` isn't already set.

```
WINMUX_SOCKET_ADDR=127.0.0.1:23456
WINMUX_TUNNEL_TOKEN=A1B2C3...32 alphanum chars total
WINMUX_PANE_ID=p_18abc_1
```

## `settings.json`  *(Phase 9.A)*

Persistent app preferences — theme, fonts, terminal behavior, hooks, notifications, and the update-checker. Loaded at `setup()`, defaults written on first run, atomically saved on every change. Mutations emit `settings:changed` so the frontend re-applies the theme live.

### Schema

```ts
type Settings = {
  version: 1;
  theme: {
    preset: "tokyo-night" | "dracula" | "solarized-dark" | "nord" | "solarized-light" | "custom";
    accent: string;          // "#7aa2f7"
    background: string;
    surface: string;
    border: string;
    text_primary: string;
    text_secondary: string;
    success: string;
    warning: string;
    error: string;
    ansi: {                  // 16 xterm colors used by xterm.js
      black: string; red: string; green: string; yellow: string;
      blue: string; magenta: string; cyan: string; white: string;
      bright_black: string; bright_red: string; bright_green: string; bright_yellow: string;
      bright_blue: string; bright_magenta: string; bright_cyan: string; bright_white: string;
    };
  };
  font: {
    ui_family: string;       // "system-ui"
    ui_size_pt: number;
    terminal_family: string; // "Cascadia Mono"
    terminal_size_pt: number;
  };
  terminal: {
    cursor_style: "block" | "bar" | "underline";
    scrollback_lines: number;
    bidi_enabled: boolean;
    allow_proposed_api: boolean;
  };
  hooks: {
    enabled: boolean;
    agents: string[];        // ["claude"]
    policy_preset: "paranoid" | "default" | "relaxed" | "auto";
  };
  notifications: {
    toast_enabled: boolean;
    sound_enabled: boolean;
  };
  updates: {
    check_on_startup: boolean;
    auto_download: boolean;       // currently always false (no signing keys yet)
    manifest_url?: string;        // defaults to a placeholder until repo goes public
    last_check_iso?: string;
    last_seen_version?: string;
  };
};
```

### Theme presets

Built-in presets are returned by `settings.get-presets` (RPC) or `winmux settings presets` (CLI). Selecting one overwrites all theme fields; manual color edits flip `theme.preset` to `"custom"`.

- `tokyo-night` (default)
- `dracula`
- `solarized-dark`
- `nord`
- `solarized-light`

### Live theme apply

The frontend reads `settings.theme` on startup and writes the colors as CSS custom properties on `<html>` (`--w-bg`, `--w-accent`, etc.) — `App.css` references all colors through these vars, so a theme change re-tints the entire UI without reload. Subscribed to the `settings:changed` event so updates from the CLI reflect live.

## Environment variables

### Read by the CLI

| Var | Default | Effect |
|---|---|---|
| `WINMUX_SOCKET_ADDR` | unset | If set (anywhere), CLI uses TCP transport with this `host:port`. Required on Linux. |
| `WINMUX_TUNNEL_TOKEN` | unset | If TCP transport is selected, used as the HMAC key for the challenge-response handshake. Required on Linux. |
| `WINMUX_PIPE_PATH` | `\\.\pipe\winmux-<USER>` | Override the default named-pipe path (Windows only). |
| `WINMUX_PANE_ID` | unset | Stamped on `feed.push` so the agent feed card knows which pane it belongs to. |
| `HOME` | OS-provided | Used to find `~/.winmux/run/last.env` for the fallback env load. |
| `USERNAME` | OS-provided | Used in the default Windows pipe name if `WINMUX_PIPE_PATH` is unset. |

### Read by the Windows app

| Var | Default | Effect |
|---|---|---|
| `USERNAME` | OS-provided | Builds the default pipe name. |
| `USERPROFILE` / `HOME` | OS-provided | Used by `try_authenticate` to find default `~/.ssh/id_*` keys. |

### Written into the remote shell

These are set by `set_env` (best-effort) on the SSH shell channel **and** mirrored
into `~/.winmux/run/last.env` so the CLI works either way:

- `WINMUX_SOCKET_ADDR=127.0.0.1:<remote_port>` (the port `tcpip_forward` returned)
- `WINMUX_TUNNEL_TOKEN=<32-char alphanumeric>`
- `WINMUX_PANE_ID=<the workspace's pane>`

Common sshd setups filter unknown env vars via `AcceptEnv`. The file fallback
exists precisely for that case.
