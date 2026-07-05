# winmux Native Push Protocol (v1)

Self-hosted push. **No Firebase / FCM / APNs.** The winmux server is the sole
source of truth; the mobile app receives real-time events over a long-lived
WebSocket held open by an Android foreground service. Events that arrive while
the device is offline are queued server-side and replayed on reconnect.

This is the contract the mobile client implements against. Server side lives in
`internal/push/` (`Pusher` replaces the Phase 77 §7 `NoopSender`).

Status: **DESIGN — v0.4.3**. Frame schema versioned via `v` so we can evolve.

---

## 1. Why a foreground-service WebSocket (architecture A)

Hook approvals need to be **interactive and immediate** — the desktop Claude
session is blocked waiting for allow/deny. Periodic polling (WorkManager, ≥15
min minimum interval on modern Android) cannot deliver that. So the client keeps
one WebSocket open inside an Android **foreground service** (a persistent
notification is mandatory on Android 8+). The socket survives Doze because
foreground services + their network are exempted; on network loss the client
reconnects with backoff and drains the offline queue.

Trade-off accepted: one persistent "winmux is connected" notification + modest
battery cost, in exchange for true real-time delivery with zero third-party
push dependency.

---

## 2. Endpoint & authentication

```
GET /api/v2/push/subscribe        (HTTP/1.1 → WebSocket upgrade)
```

Auth — the device's **long-term token** (issued at pairing redeem), supplied
either way (the client SHOULD prefer the header):

- `Authorization: Bearer <long_term_token>` header, **or**
- `?token=<long_term_token>` query parameter (for WS clients that can't set
  headers).

The server resolves the token → `device_id`. On failure it rejects the upgrade
with **HTTP 401** (no WS established). A valid but revoked device is also 401.

Optional query params on connect:

| Param    | Meaning                                                        |
|----------|----------------------------------------------------------------|
| `cursor` | Last `push_seq` the client durably processed (default `0`). Server replays every queued event with `push_seq > cursor` before going live. |

Example:
```
wss://phone.example.com/api/v2/push/subscribe?cursor=4187
Authorization: Bearer 9f3c…（64 hex）
```

There is **one** push connection per device. A new connect for a device that is
already connected replaces the old one; the server closes the stale socket with
code `4409` (`replaced`).

WebSocket subprotocol: none required. Text frames, UTF-8 JSON.

---

## 3. Frame envelopes

### 3.1 Server → client: `event`

Every delivered event is wrapped in a per-device envelope:

```json
{
  "v": 1,
  "type": "event",
  "device_id": "dev_a1b2c3",
  "push_seq": 4188,
  "ts": 1730000000,
  "event": {
    "type": "hook_request",
    "session_id": "sess_…",
    "seq": 512,
    "req_id": "req_…",
    "tool_name": "Bash",
    "title": "Run: rm -rf build",
    "decision_required": true
  }
}
```

- `push_seq` — **per-device**, monotonically increasing across ALL sessions.
  This is the client's durable cursor. (Distinct from `event.seq`, which is the
  per-session log sequence.)
- `event` — a §4.4 stream frame verbatim (`internal/workspace/frames.go`). The
  push channel delivers only the notification-worthy subset:
  `hook_request`, `assistant_text`, `notification`. (High-frequency
  `tool_use` / `tool_result` / `status` frames are NOT pushed; the client sees
  those on the interactive workspace WS when the app is open.)
- The client displays each `event.type` however it likes (data-driven); tapping
  a `hook_request` opens the app, which connects the **workspace** subscribe WS
  and resolves the hook through the existing API. This push channel is
  delivery + wake only — it never carries the user's decision.

### 3.2 Server → client: `hello` (first frame)

```json
{ "v": 1, "type": "hello", "device_id": "dev_a1b2c3", "server_push_seq": 4188, "heartbeat_sec": 30 }
```

Sent once on connect, before any replay. `server_push_seq` is the latest seq the
server has for this device (so the client knows how far behind it is).

### 3.3 Server → client: `error`

```json
{ "v": 1, "type": "error", "code": "unauthorized", "message": "…" }
```
`code` ∈ `unauthorized | rate_limited | server_error | bad_request`. Usually
followed by a close (see §6).

### 3.4 Client → server: `ack`

```json
{ "type": "ack", "push_seq": 4188 }
```
Acknowledges durable processing up to and including `push_seq`. The server
deletes queued events with `push_seq <= ack` for this device. Clients SHOULD ack
promptly (per event or batched every ~1s). Un-acked events are re-delivered on
reconnect — **at-least-once**; dedup on the client (see §5).

### 3.5 Heartbeat

The server sends a WebSocket **ping** control frame every `heartbeat_sec` (30s).
The client MUST answer with a **pong** (most WS libraries auto-pong). If the
server sees no pong for `2 × heartbeat_sec` (60s) it drops the connection
(`4408 timeout`). The client MAY also send an app-level `{"type":"ping"}` and
will get `{"v":1,"type":"pong"}` back — useful where control frames are opaque.

---

## 4. Delivery model (server side)

On every `Publish` of a notification-worthy event on a session with **no live
interactive (workspace-WS) subscriber**, the server routes the event to each
active paired device:

1. Assign the device its next `push_seq`, persist the envelope in
   `pending_events(device_id, push_seq, ts, event_json)`.
2. If the device has a **live push WS** → send the envelope immediately.
3. Otherwise it waits in the queue until the device (re)connects.

`ack` prunes the queue. A sweeper prunes anything older than the retention TTL
(§7) regardless of ack, so a permanently-gone device can't grow the DB.

If a session HAS a live workspace subscriber (app open + foreground on that
session), no push is generated — the user already sees it.

---

## 5. Reconnect, replay & dedup

- **Reconnect:** on any drop, the client reconnects with exponential backoff:
  `1s, 2s, 4s, 8s, … capped at 30s`, ±20% jitter. Reset the backoff after a
  connection stays up > 60s.
- **Cursor:** reconnect with `?cursor=<last_acked_push_seq>`. The server replays
  every queued envelope with `push_seq > cursor` (ordered), then goes live.
- **Dedup:** delivery is at-least-once, so the client MUST dedup. Two keys:
  - `push_seq` — ignore any envelope whose `push_seq <=` the highest processed.
  - `event.req_id` (hooks) — the hook side is already idempotent; a repeated
    `req_id` that's already resolved is a no-op / just refreshes UI.

---

## 6. Close codes

| Code   | Name          | Meaning                                            |
|--------|---------------|----------------------------------------------------|
| `1000` | normal        | Clean shutdown (client or server).                 |
| `1012` | restart       | Server restarting; client should reconnect w/ backoff. |
| `4401` | unauthorized  | Token invalid/revoked mid-session. Do NOT auto-retry without re-pairing. |
| `4408` | timeout       | Missed heartbeat; reconnect.                       |
| `4409` | replaced      | Another connection for this device took over.      |
| `4429` | rate_limited  | Too many connects; back off harder.                |

Pre-upgrade auth failure is a plain **HTTP 401** (no WS, no close code).

---

## 7. Retention & limits

- `pending_events` TTL: **24h** (config `PUSH_QUEUE_TTL_HOURS`, default 24). A
  sweeper runs at boot + hourly.
- Per-device queue cap: 1000 envelopes (drop-oldest beyond, logged as metadata
  only — never the content).
- Connect rate limit: ≤ 5 connects / 10s per device → `4429`.

---

## 8. Android background-execution requirements (client)

- Hold the WS inside a **foreground service** (`FOREGROUND_SERVICE` +
  `FOREGROUND_SERVICE_DATA_SYNC` on Android 14+) with a low-priority persistent
  notification ("winmux connected").
- `POST_NOTIFICATIONS` runtime permission (Android 13+) for the hook alerts.
- A **partial wake-lock** held only while processing an incoming event (release
  immediately after ack) — never a persistent wake-lock.
- Reconnect on `ConnectivityManager` network-available callbacks; also rely on
  the §5 backoff loop.
- `START_STICKY` service; restart the socket in `onStartCommand`.
- Battery-optimization exemption is NOT required for the foreground-service +
  WS approach, but the app should offer to request it for users on aggressive
  OEM skins (Xiaomi/Huawei) where FGS are still killed.

---

## 9. Versioning

`v` is the envelope version (currently `1`). New optional fields may be added
without bumping `v`; a breaking change bumps it and the server negotiates via
`hello`. Unknown frame `type`s MUST be ignored by the client (forward-compat).

---

## 10. Open items (tracked, not blocking mobile start)

- Multi-device fan-out policy when several phones are paired: currently ALL
  active devices are queued. A future per-device "mute" / scope (`hook:approve`)
  gate can suppress delivery — see the scopes work.
- An optional desktop-tray / email fallback when NO device has connected within
  N minutes (out of scope for v0.4.3).
