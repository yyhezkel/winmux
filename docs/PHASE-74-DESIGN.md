# Phase 74 (proposed: **Phase 77**) — `winmux-server` as a First-Class Component

> **STATUS: DESIGN / PLANNING ONLY. No code until Yossi approves this doc.**
>
> **⚠ Phase-number collision.** "Phase 74" is already taken — it shipped today as
> the split-QR pairing work (commit `2ebb645`, `feat(mobile): Phase 74 — split-QR
> pairing`). CLAUDE.md forbids reusing phase numbers. This document keeps the
> filename you asked for (`PHASE-74-DESIGN.md`) but I recommend the phase be
> renumbered to **Phase 77** (next free; 73/74/75/76 are all used this week).
> Decision left to Yossi — see Open Questions Q0.

---

## 1. Motivation

The metrics daemon `winmux-insights` has quietly grown from a CPU/RAM sampler
(Phase 68) into a real multi-subsystem server: metrics, **Claude chat sessions**
(Phase 69), **mobile pairing** (Phase 70/74-QR), a **hook policy bridge** (Phase
66/69.C), and **process hygiene** (Phase 76). It is one flat `package main`
(21 `.go` files) with an ad-hoc route scheme (`/current` next to
`/api/claude/...`), a single shared bearer token, and no contract that clients
can generate against.

Desktop and (future) mobile both talk to it, but there is **no stable API
surface** — every client hand-writes request/response types that can silently
drift from the Go structs. As mobile lands, this becomes a correctness and
velocity risk.

**Phase 77 makes the server a first-class component**: clean internal module
boundaries, a normalized versioned HTTP/WS API, an auto-generated OpenAPI spec,
and SDKs (Kotlin + TypeScript) generated from that spec so desktop and mobile
share one source of truth.

### Goals
- One server, **clean module boundaries**, no import cycles.
- **Normalized, versioned API** (`/api/v2/...`) with request/response schemas.
- **Auto-generated OpenAPI** → generated **Kotlin + TypeScript SDKs**, version-locked.
- **Per-client auth scoping** (desktop vs. each paired mobile device).
- **Zero-downtime migration** from `winmux-insights` 1.x with data preserved.
- Monorepo — everything stays in the `winmux` repo.

### Non-goals (explicitly deferred)
- **Splitting into separate repos.** Only if/when iOS lands or the overhead
  justifies it. Monorepo now.
- Rewriting the desktop's Rust ↔ server transport. The desktop keeps talking
  over the existing reverse SSH tunnel + `insights_fetch`/`daemon_curl`; only
  the *paths + payloads* it hits are normalized.
- A general plugin system. The subsystems are known and finite.
- Multi-tenancy / multi-user on one server. One Linux user, one server.

---

## 2. Current state (what exists today, v1.2.7)

```
app/src-tauri/insights/           module winmux-insights, package main (flat)
  main.go        boot, flags, log rotation (Phase 75.1), goroutine wiring
  api.go         http.ServeMux, auth(), metrics handlers
  sampler.go     gopsutil CPU/RAM/disk/net + /current cache (Phase 72.3)
  docker.go      unix-socket docker probe + stats
  store.go       SQLite metrics ring (modernc, CGO-free)
  util.go        tailFile, helpers
  hygiene.go     Phase 76 process hygiene (port-watch dups, orphan claude)
  chat_*.go      Phase 69 Claude chat: session mgr, stream-json parser,
                 hook RPC bridge, pairing (device tokens), chat SQLite store
```

**Routes today** (note the inconsistency — this is what we normalize):

| Subsystem | Routes | Auth |
|---|---|---|
| liveness | `GET /healthz` | none |
| insights | `GET /current` `GET /history` `GET /processes` | bearer |
| docker | `GET /docker` `POST /docker/{id}/action` | bearer |
| logs | `GET /logs?tail=N` | bearer |
| hygiene | `GET /hygiene` `POST /hygiene/kill` | bearer |
| chat | `GET/POST /api/claude/session[s]` `WS /ws/claude/session/{id}` | bearer |
| pairing | `POST /api/pairing/issue` `POST /api/pairing/redeem` `GET/DELETE /api/pairing/devices[/{id}]` | bearer / device token |
| hooks | internal `127.0.0.1:<rand>` TCP RPC (not HTTP) | HMAC |

**Known structural constraint (important):** the daemon is deliberately *flat*.
Phase 69's DECISIONS note records that a naive `internal/claude/` split created a
**WS ↔ session ↔ hookRPC import cycle**. Any module refactor MUST solve this by
dependency inversion (interfaces in a leaf package), not by wishful directory
moves. See §4.1.

---

## 3. Target architecture

### 3.1 Module layout (monorepo, under `app/src-tauri/server/`)

```
app/src-tauri/server/                module winmux-server, v2.0.0
  cmd/winmux-server/main.go          thin: parse flags, build deps, run api.Server
  internal/
    core/          # LEAF package: shared types + interfaces ONLY (no logic,
                   # no imports of siblings) — breaks the cycle. e.g.
                   #   type Sampler interface { Current() Snapshot }
                   #   type SessionManager interface { ... }
                   #   type HookBridge interface { ... }
    config/        # persistent state: opens the single SQLite db, migrations,
                   # token store; depends only on core
    auth/          # bearer + per-client (device) scoping middleware; core
    insights/      # metrics sampler + docker + store  (impl of core.Sampler)
    chat/          # Claude sessions + stream-json + WS  (impl of core.SessionManager)
    pairing/       # QR device pairing + device tokens
    hooks/         # policy engine bridge (the 127.0.0.1 RPC listener)
    files/         # NEW: directory listing + shared-folder up/download
    logs/          # NEW: per-client log storage (list/read)
    workspace/     # NEW: cross-client shared state + event bus (WS)
    api/           # HTTP routes, version prefix, OpenAPI wiring; imports the
                   # subsystem packages THROUGH core interfaces where cycles
                   # would otherwise form
  sdk/
    kotlin/        # generated — do not hand-edit
    typescript/    # generated — do not hand-edit
  openapi.json     # generated spec (checked in for diff review + SDK gen)
```

### 3.2 Dependency direction (ASCII)

```
                         cmd/winmux-server
                                │  builds concrete impls, injects into api
                                ▼
        ┌──────────────────── api ─────────────────────┐
        │   routes + version prefix + OpenAPI + auth mw │
        └───┬───────┬───────┬───────┬───────┬───────┬───┘
            │       │       │       │       │       │        (all depend on core
            ▼       ▼       ▼       ▼       ▼       ▼         interfaces, never on
        insights  chat   pairing  hooks  files   logs  ...   each other directly)
            │       │       │       │       │       │
            └───────┴───────┴───┬───┴───────┴───────┘
                                ▼
                              core          (leaf: types + interfaces, zero deps)
                                ▲
                              config, auth   (also leaf-ish; depend on core only)
```

**Rule that keeps it acyclic:** cross-subsystem needs (chat needs the hook
bridge; workspace broadcasts to WS clients) are expressed as **`core`
interfaces**. `chat` imports `core.HookBridge`, not `hooks`. `cmd` wires the
concrete `hooks.Bridge` into `chat` at startup. No sibling imports a sibling.

### 3.3 Transport

- HTTP/1.1 + JSON for request/response.
- WebSocket for streaming (chat stream-json today; workspace events new).
- Everything stays behind the reverse SSH tunnel for desktop; behind
  nginx+TLS+device-token for mobile (Phase 70). No transport change.

---

## 4. API surface (v2)

All routes move under a **version prefix `/api/v2/`** and a consistent
subsystem segment. `GET /healthz` stays unversioned (liveness for probes).
`GET /api/version` returns `{name, version, api_versions:[2], min_client}` so a
client can negotiate.

### 4.1 Existing subsystems (renamed, behavior unchanged)

| v2 route | was | notes |
|---|---|---|
| `GET /api/v2/insights/current` | `/current` | cached snapshot (Phase 72.3) |
| `GET /api/v2/insights/history?metric=&since=` | `/history` | |
| `GET /api/v2/insights/processes?limit=` | `/processes` | |
| `GET /api/v2/insights/docker` | `/docker` | |
| `POST /api/v2/insights/docker/{id}/action` | `/docker/{id}/action` | `{cmd}` |
| `GET /api/v2/insights/hygiene` | `/hygiene` | Phase 76 |
| `POST /api/v2/insights/hygiene/kill` | `/hygiene/kill` | `{pids}` |
| `GET /api/v2/logs/daemon?tail=N` | `/logs` | server's own log (rename to avoid clash with new per-client logs) |
| `POST /api/v2/chat/sessions` … | `/api/claude/session[s]` | |
| `WS  /api/v2/chat/sessions/{id}/stream` | `/ws/claude/session/{id}` | |
| `POST /api/v2/pairing/issue` | `/api/pairing/issue` | |
| `POST /api/v2/pairing/redeem` | `/api/pairing/redeem` | Phase 74 QR-split payload v2 |
| `GET/DELETE /api/v2/pairing/devices[/{id}]` | `/api/pairing/devices` | |

### 4.2 New subsystems

**Files** (directory picker + shared folder). Root-scoped, traversal-safe.
```
GET  /api/v2/files/list?path=/abs/or/rel
     → { path, entries:[{name,is_dir,is_link,size,modified,perms}] }
POST /api/v2/files/upload         (multipart or chunked; body=bytes, ?path=)
     → { path, bytes }
GET  /api/v2/files/download?path= (streamed; Range supported)
     → octet-stream
```
- **Security:** a configured *shared-folder root*; every `path` is cleaned and
  MUST stay within root (reject `..` escapes). List/read outside root = 403.
  (Mirrors the desktop's existing `file_manager.rs` guarantees, server-side.)

**Logs** (per-client). Each client (desktop + each device) writes/reads its own
log bucket so support can see one client's activity.
```
GET /api/v2/logs/list                → { clients:[{client_id, size, updated}] }
GET /api/v2/logs/read?client_id=X&tail=N → { client_id, lines:[...] }
```
- Backed by `internal/logs` under `~/.winmux/server/logs/<client_id>.log`,
  size-capped + age-pruned (reuse Phase 75/75.1 janitor logic).

**Workspace** (cross-client shared state + events). The least-defined piece —
see Open Questions Q3.
```
GET  /api/v2/workspace/state             → current shared doc (JSON)
PUT  /api/v2/workspace/state             → replace/patch (optimistic version)
WS   /api/v2/workspace/events            → stream {type, payload, client_id, ts}
```
- v1 semantics proposal: a versioned JSON document + an append-only event
  stream; last-writer-wins with a `version` guard; no CRDT yet.

**Meta**
```
GET /healthz                 (unauthed) → { ok, version }
GET /api/version             → { name:"winmux-server", version, api_versions, min_client }
GET /api/openapi.json        → generated OpenAPI 3.1 spec
```

### 4.3 Auth & per-client scoping (`internal/auth`)
- **Desktop** presents the daemon bearer token (as today; read from the token
  file over the tunnel).
- **Mobile devices** present their **paired device token** (Phase 70). Each
  request is scoped to a `client_id` (device id, or `"desktop"`).
- Scopes gate subsystems: e.g. a device may be allowed `insights:read` +
  `chat:*` but not `hygiene:kill` or `files:*` unless granted at pairing time.
  (This is where the deferred Phase-74-QR "scopes checkboxes" finally land —
  server-enforced, not cosmetic.)

---

## 5. Data & config (`internal/config`)
- Consolidate the currently-separate SQLite files (`metrics.db`, `chat.db`)
  under `~/.winmux/server/` with one opener + a migration runner. Keep them as
  separate DBs (different lifecycles/retention) but behind one config package.
- Device tokens, workspace state, per-client log index live here too.
- Startup runs forward-only migrations keyed by a `schema_version` table.

---

## 6. Client SDK generation (74.C)

**Recommended approach: OpenAPI-first via a typed Go framework.**
- Define handlers with typed input/output structs; emit **OpenAPI 3.1**
  automatically. Candidate libs (Open Question Q4):
  - **`huma`** (danielgtaylor/huma) — typed handlers, validation, OpenAPI 3.1
    out of the box, stdlib-compatible. *Recommended.*
  - `swaggo/swag` — annotation-based; more boilerplate, less type-safety.
- Generate SDKs from `openapi.json` with `openapi-generator-cli`:
  - Kotlin → `sdk/kotlin/` (retrofit/moshi or ktor client).
  - TypeScript → `sdk/typescript/` (fetch-based, tree-shakeable).
- **Build integration:** a `make sdk` / npm script regenerates both on server
  change; CI fails if `openapi.json` or the SDKs are stale (drift guard).
- **Version-lock:** SDK package version == server version; a client refuses a
  server whose `api_versions` doesn't include the SDK's.

> Reality check: the **desktop** today calls the server through Rust
> (`insights_fetch` over SSH), not the TS SDK directly. The generated TS SDK is
> primarily for a future web/mobile-web client and for **contract tests**; the
> desktop can keep its Rust calls but they get validated against the same
> OpenAPI in CI. Mobile (Kotlin) is the primary SDK consumer. (Open Q5.)

---

## 7. Migration path & backward compatibility (74.D)

**Binary rename with alias.** Installer lays down `winmux-server`; keeps a
`winmux-insights` **symlink → winmux-server** so anything referencing the old
name still resolves. `winmux-server --version` prints `2.x`; the old
`winmux-insights --version` path (symlink) prints the same.

**Systemd unit.** New `winmux-server.service`; installer `disable --now` the old
`winmux-insights.service`, removes its unit, installs + starts the new one
(reuse the Phase 72.1 `sg docker` launch logic).

**Data preserved.** `~/.winmux/insights/` → migrated/renamed to
`~/.winmux/server/` on first 2.x boot: move `metrics.db`, `chat.db`, `token`,
`paired_devices`. Idempotent; keeps the same device tokens so **already-paired
phones keep working**.

**Client version detection.** Desktop reads `GET /api/version`:
- `1.x` (or `/healthz` only) → speak the **legacy** paths (`/current`, …).
- `2.x` → speak `/api/v2/...`.
The desktop ships a tiny adapter that picks the path set by detected version.

**Compat window.** The 2.x server keeps **legacy route aliases** (`/current`
→ `/api/v2/insights/current`, etc.) for **≥ 3 minor versions** so an old desktop
against a new server still works during rollout. Aliases are thin redirects to
the v2 handlers, logged as deprecated.

**Version lockstep.** `INSIGHTS_VERSION` (winmux-addons) becomes
`SERVER_VERSION`, tracks the Go `Version` const (now `2.x`). The desktop's
update-available check compares remote `winmux-server --version` to this.

---

## 8. Client contracts (desktop + mobile)

- **Desktop (Rust):** unchanged transport (reverse SSH tunnel). Gains a
  version-adapter for path selection. Validated against OpenAPI in CI (contract
  test), not forced onto the generated TS SDK.
- **Mobile (Kotlin):** consumes the generated Kotlin SDK exclusively; talks to
  the server via nginx+TLS with its device token; scoped by pairing grants.
- **Contract stability guarantee:** within a major (`2.x`) no breaking changes
  to existing v2 endpoints — only additive. Breaking changes ⇒ `/api/v3` + a
  new major + a compat window.

---

## 9. Testing strategy (74.F)
- **Unit** per `internal/*` package (pure logic: markDuplicates-style, parsers,
  path-scoping, token scoping). Runs on the Windows dev box (CGO-free).
- **Integration**: spawn `winmux-server` on a random port with a temp data dir,
  hit real endpoints (auth, files traversal rejection, hygiene kill safety,
  chat happy-path with a fake `claude`).
- **Contract**: generate SDKs, point them at the spawned server, assert the
  generated client + server agree (this is the drift guard with teeth).
- **E2E**: real desktop build + a mock mobile client → server, over a local
  tunnel; the pairing → chat → insights round-trip.
- CI gates: `go test ./...`, `openapi.json` up-to-date, SDKs up-to-date,
  `cargo`/`tsc` for the desktop adapter.

---

## 10. Version cadence
- `winmux-server` **2.0.0** at the refactor cut; SemVer thereafter.
- **PATCH** = server-internal fix, no API change (desktop needn't update).
- **MINOR** = additive endpoints/fields; SDKs regenerate; clients optional-update.
- **MAJOR** = breaking API ⇒ new `/api/vN` + compat window.
- Server version and SDK versions are locked equal.

---

## 11. Phased implementation plan + estimates

> A "sprint-day" = one focused day of implementation on this repo. Ranges
> reflect the biggest unknowns (module-cycle surgery in S1, workspace semantics
> in S3, SDK toolchain in S4). Estimates assume no scope growth.

| Sprint | Scope | Deliverable | Est. |
|---|---|---|---|
| **S1** | Rename + module boundaries | `server/` monorepo dir, `core` interface layer breaking the WS↔session↔RPC cycle, all existing subsystems moved behind interfaces, **all current tests green**, binary+systemd rename with `winmux-insights` alias, legacy route aliases, version detection. No new features. | **3–4** |
| **S2** | Files API + Logs API | `/api/v2/files/*` (root-scoped, traversal-safe) + `/api/v2/logs/*` (per-client, janitor-bounded) + unit/integration tests. | **3** |
| **S3** | Workspace API + shared state | `/api/v2/workspace/state` + WS event bus + versioned last-writer-wins; needs Q3 answered first. | **4–5** |
| **S4** | SDK generation | huma/OpenAPI wiring, `openapi.json`, Kotlin + TS generators, build integration + CI drift guard, version-lock. | **3–4** |
| **S5** | Migration + polish | data move `insights/`→`server/`, 2.x auto-upgrade on install, desktop version-adapter, E2E + contract tests, docs (README/API/CLIENTS). | **3–4** |
| — | **Total** | | **~16–21 sprint-days** (≈ 3–4 focused weeks) |

Recommended ordering note: **S1 must fully land and ship (as 2.0.0 with feature
parity + compat aliases) before S2+**, so we de-risk the rename/migration
independently of the new features. Each later sprint ships as a 2.x MINOR.

---

## 12. Open questions for Yossi (blockers marked ⛔)

- **Q0 — Phase number.** Renumber to **Phase 77** (recommended, avoids clash
  with the shipped split-QR "Phase 74")? Or keep 74 and accept the collision?
- **Q1 ⛔ — Directory location.** New tree `app/src-tauri/server/` inside the
  Tauri app (matches today), or a top-level `server/` in the repo root? The
  latter reads more "first-class" but changes embedding/build paths.
- **Q2 — Files API scope.** Is the "shared folder" a single configured root, or
  should it expose the whole filesystem like the desktop file manager (with the
  same guards)? Single root is safer; full-FS is more powerful for mobile.
- **Q3 ⛔ — Workspace shared state.** What is it concretely? Candidates: shared
  cursor/presence, shared notes, a shared task queue, mirrored terminal state?
  This defines S3 entirely — needs a one-paragraph spec before S3.
- **Q4 — OpenAPI toolchain.** OK to adopt **huma** (typed Go → OpenAPI 3.1) as
  the handler framework, or prefer annotation-based `swaggo` to avoid a handler
  rewrite? huma is cleaner long-term but touches every handler in S1/S4.
- **Q5 — Desktop SDK.** Keep the desktop on its Rust-over-SSH calls (validated
  by contract tests), or eventually move it to the generated TS SDK? Affects how
  much of the desktop transport we touch.
- **Q6 — Scopes model.** Confirm the per-device scope set (e.g. `insights:read`,
  `chat`, `files:read`, `files:write`, `hygiene:kill`, `workspace`). This is the
  home for the deferred QR "scopes checkboxes."
- **Q7 — Hooks over HTTP?** The hook bridge is a separate localhost TCP RPC
  today. Fold it under `/api/v2/hooks/*` for uniformity, or leave it as-is
  (it's desktop↔server-local, never mobile)? Leaving it is less work.
- **Q8 — Compat window length.** "≥3 minor versions" of legacy aliases — good,
  or do you want a hard cutoff date?

---

## 13. Relationship to the parked work

The `72-docker-group` branch currently holds a large **unmerged** stack
(72.3 `/current`, addon uninstall/reinstall, mobile RTL + CF docs, Phase 73
tagged logs, nginx `limit_req_zone` fix, Phase 74 QR-split, Phase 75/75.1 log
hygiene, Phase 76/76.1 process hygiene, FM logging + streaming download,
port-watch reap). **Recommendation:** land that stack as **v0.4.2 first**
(it's tested and self-contained), *then* start Phase 77 on a fresh branch off
the updated `main`. Doing the refactor on top of an unmerged stack multiplies
merge risk. Confirm in Q-review.
