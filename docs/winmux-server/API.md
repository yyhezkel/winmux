# winmux-server API reference

`winmux-server` (2.0.0) exposes a localhost HTTP+WS API behind the winmux tunnel,
bearer-token gated. Two audiences:

1. **Client-SDK surface** (mobile / web / contract tests) — generated OpenAPI +
   the WS frame contract. This is what `sdk/` wraps.
2. **Desktop-internal surface** — the Insights metrics API, consumed by the
   desktop Monitor's Rust client. Not part of the generated SDK.

## Discovery

| Endpoint | Auth | What |
|---|---|---|
| `GET /healthz` | none | liveness `{ok, version}` |
| `GET /api/version` | none | negotiation `{name, version, api_versions:[2], frame_version}` |
| `GET /api/openapi.json` | none | REST contract (generated from huma handlers) |
| `GET /api/asyncapi.json` | none | WS frame contract (AsyncAPI 2.6) |
| `GET /api/frames.schema.json` | none | WS frames as JSON-Schema 2020-12 (SDK source) |

## Client-SDK surface (bearer, generated into the SDKs)

Auth on `/api/v2/*` accepts **the shared desktop token OR a paired device's
long-term token** (from `pairing/redeem`), so a phone uses the same surface.

**Pairing** — `POST /api/pairing/redeem` (public; one-shot token in body is the
credential) → `{device_id, long_term_token, default_workspace_id}`.

**Files** — sandboxed to the server's files root (`$HOME` by default):
`GET files/list?path=&depth=1|2`, `GET files/read?path=&max_bytes=` (raw bytes +
`X-Winmux-Truncated`), `POST files/upload?path=` (multipart `file`),
`GET files/download?path=` (attachment), `DELETE files/delete?path=`.

**Logs** — per-client log tree + the `server` pseudo-client:
`GET logs/list`, `GET logs/read?client_id=&file=&tail=`,
`GET logs/stream?client_id=&file=` (SSE `event: line`).

**Workspace** — `GET /api/v2/workspace/list`,
`POST /api/v2/workspace/{id}/sessions` (`{kind}` → `{session_id, kind}`),
`GET /api/v2/workspace/{id}/session/{sid}`, plus the WS stream
`GET workspace/{id}/session/{sid}/subscribe?cursor=&client_id=&token=`
(frames in [CLIENTS.md](CLIENTS.md) + AsyncAPI).

> Desktop-only workspace admin (`POST /create`, `GET /{id}`, `DELETE /{id}`,
> `GET /{id}/sessions`) stays on raw handlers — outside the SDK spec.

Full schemas: the served `openapi.json` (REST) + `frames.schema.json` (WS).

## Desktop-internal surface (not in the SDK spec)

Insights metrics, served at both legacy paths and `/api/v2/insights/*`:
`current`, `history`, `hygiene[/kill]`, `docker[/…]`, `processes`, plus
`/api/v2/logs/daemon`. Dynamic JSON (metric/docker/process maps) consumed by the
desktop over SSH; intentionally kept on raw stdlib handlers and out of the
generated OpenAPI (PHASE-77-DESIGN §6).

## Pairing (desktop-facing)

`/api/pairing/*` — the desktop Monitor issues QR + device tokens. The legacy
mobile Claude-chat HTTP surface (`/api/claude/*`, `/ws/claude/*`) was retired in
Phase 77 → **410 Gone**; clients use `/api/v2/workspace/*`.
