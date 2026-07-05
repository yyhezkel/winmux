# winmux-server

The winmux server daemon (2.0.0) — formerly `winmux-insights`, restructured in
Phase 77 into a module with clean `internal/*` subsystems behind `core`
interfaces. Runs on the remote host the desktop connects to; exposes a
localhost HTTP + WebSocket API behind the winmux tunnel, bearer-token gated.

## Subsystems

| Package | Surface | Notes |
|---|---|---|
| `internal/insights` | metrics / docker / processes / hygiene | desktop Monitor (Rust client); not in the SDK spec |
| `internal/files` | `/api/v2/files/*` | sandboxed filesystem (traversal + symlink-escape rejected) |
| `internal/logs` | `/api/v2/logs/*` | per-client log tree + `server` pseudo-client + SSE tail |
| `internal/workspace` | `/api/v2/workspace/*` + WS `subscribe` | shared-state sessions (8a attach, 8b hook broadcast) |
| `internal/chat` | pairing (`/api/pairing/*`) | Claude session engine kept internal; legacy chat HTTP → 410 |
| `internal/api` | front door | auth, version/health, generated OpenAPI + WS frame specs |

Contract + SDKs: [API.md](API.md), [CLIENTS.md](CLIENTS.md), [../../sdk-gen](../../sdk-gen).

## Run

```
winmux-server [serve] [--port 7879] [--dir ~/.winmux/insights] \
              [--interval 5] [--files-root $HOME]
winmux-server --version     # prints "winmux-server 2.0.0"
winmux-server openapi       # prints the generated OpenAPI spec (for sdk-gen)
```

Data dir (`--dir`, default `~/.winmux/insights`) holds `token`, `metrics.db`,
`chat.db`, `workspace.db`, `logs/`, and the rotating `insights.log`. The bearer
token is generated on first boot (`<dir>/token`).

## Build

CGO-free (`modernc.org/sqlite`), so it cross-compiles cleanly:

```
cd app/src-tauri/server
GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" \
  -o ../resources/winmux-server-linux-x64 ./cmd/winmux-server
GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" \
  -o ../resources/winmux-server-linux-arm64 ./cmd/winmux-server
```

`-trimpath` keeps the build-host username out of the binary. The desktop embeds
these two binaries (`include_bytes!` in `src/addons.rs`) and SFTP-uploads the
right arch to `~/.winmux/bin/winmux-server` on install.

See [DEPLOYMENT.md](DEPLOYMENT.md) for the systemd unit + tunnel, and
[UPGRADE.md](UPGRADE.md) for the 1.x → 2.0 path.
