# Phase 70 — Server-side Mobile Pairing (nginx + Cloudflare + Let's Encrypt) (DESIGN)

> Status: **APPROVED, building** (2026-06-30). Branch: `70-mobile-pairing`
> (off `69-claude-chat`). Nothing pushed to `main`.
> All §3 decisions confirmed by Yossi: §3.1 root-or-NOPASSWD now (defer
> interactive sudo password); §3.2 WebPKI-only (no leaf pinning, fingerprint
> informational); §3.3 accept all-scopes RCE surface with the listed
> controls; §3.4 replace 69.D `devices` with `paired_devices`.

## 1. Motivation

Phase 70 answers the open question Phase 69 deferred (§9-Q4: *how does the
phone reach the daemon's `localhost:7879`?*). The answer Yossi chose:

- **nginx** as a public reverse proxy on the workspace server (`:443`),
- **Let's Encrypt** certs via **DNS-01** using **Cloudflare** as the DNS
  provider (works even when `:80` is firewalled / behind NAT — DNS-01 needs
  no inbound HTTP),
- the daemon stays **localhost-only**; nginx is the only thing exposed,
- **automatic** install of nginx + certbot + cert + config,
- new **pairing** endpoints on the daemon so a phone can be enrolled via a
  QR code and connect over `wss://<domain>`.

```
[mobile app]  ──── wss://<domain>:443 (WebPKI TLS) ────┐
                                                        ▼
                                          [nginx :443]  (public IP, the only
                                           reverse proxy │  exposed surface)
                                                        ▼
                                          [daemon 127.0.0.1:7879]  (localhost
                                           - Insights  (Phase 68)   only; never
                                           - Claude chat (Phase 69)  bound public)
                                           - Pairing   (Phase 70)
```

Yossi's 5 locked decisions: (1) Cloudflare DNS for LE, (2) nginx reverse
proxy, (3) automatic install of nginx+cert, (4) **all scopes always** (no
per-device scoping), (5) **multi-workspace** pairing (one phone, many
servers).

## 2. Dependencies & version reconciliation

- 70.B (pairing) extends the **Phase 69** daemon, so this branch is off
  `69-claude-chat` (daemon `1.1.0`).
- The **v0.3.x-monitor-docker-fix** branch independently took the daemon to
  `1.0.2` + `INSIGHTS_VERSION` 1.0.2. At integration these converge:
  daemon → **1.2.0** (chat + docker-fix + pairing), and
  `INSIGHTS_VERSION` must track it (the drift that caused the Docker-empty
  bug — never let them diverge again).
- This is a build-order note, not a Phase 70 task; flagged so it isn't lost.

## 3. ⚠ Spec corrections / blocking decisions (need Yossi's nod)

These materially change the implementation — confirming them is the gate to
coding.

### 3.1 Root / sudo — the big one
nginx + `apt-get` + certbot all need **root**. The add-on framework
currently has **no sudo execution path at all** — `needs_sudo` is only a UI
flag today (verified: no `sudo` handling in `addons.rs`, no sudo-password
mechanism anywhere). So we must add one. Proposed (round 1):

1. If the SSH user **is root** (`id -u` = 0) → run the installer directly.
   (Common for VPS where you connect as root.)
2. Else try **passwordless sudo** (`sudo -n true`) → prefix the installer
   with `sudo`.
3. Else → **fail with a clear, actionable error**: "nginx install needs
   root — connect this workspace as root, or enable NOPASSWD sudo for the
   user." (Collecting an interactive sudo password + DPAPI is a follow-up,
   70.A.2, not round 1.)

*Recommend: ship 1+2 now, defer interactive-sudo-password.* **Confirm?**

### 3.2 Cert fingerprint pinning vs 90-day renewal
The spec's QR payload pins `fingerprint: sha256-of-cert`. Let's Encrypt
**rotates the leaf every ≤90 days**, so pinning the leaf would break every
phone on renewal. Since LE is a **publicly-trusted CA**, the mobile app
should validate via the **system trust store (WebPKI)** — no pinning needed.

*Recommend:* drop leaf-cert pinning. Keep a `fingerprint` field in the QR
but make it **informational/optional** (a first-connect TOFU check the app
may show, never a hard pin), or pin the **SPKI of the LE intermediate** if
we want defence-in-depth (survives leaf rotation). **Confirm: WebPKI-only
(simplest, correct) — yes?**

### 3.3 Public exposure + "all scopes always" threat model
Decision #4 means a device token grants **Insights + Claude chat + hooks**.
Claude chat = spawning Claude sessions that run tools on the server → a
leaked device token is effectively **remote code execution**. That's an
accepted product trade-off, but the compensating controls must be real:
- TLS everywhere (nginx, WebPKI); the daemon never binds public.
- One-shot pairing tokens, short TTL (5 min), single redeem.
- Long-term tokens are **revocable** (admin list + revoke) and **hashed at
  rest** (sha256; Rule #2).
- nginx-level **rate limiting** + optional IP allowlist.
- `last_seen` / `last_ip` per device for an audit trail.

*Recommend:* proceed with all-scopes per the decision, ship the controls
above, and add an **optional** "this device can't approve destructive
hooks" toggle later if desired. **Confirm: accept the all-scopes RCE
surface with these controls?**

### 3.4 Table unification with 69.D
Phase 69.D added a `devices` table (id, token_hash, label, created_at,
revoked_at). Phase 70 needs status/scopes/expiry/last_seen. Since 69 isn't
released, I'll **replace `devices` with `paired_devices`** (superset) and
point 69's auth at it — one device table, not two. **Confirm.**

### 3.5 Multi-workspace = client-side only
Each workspace is a **separate server with its own domain**. "Multi-
workspace pairing" therefore needs **no server-side machinery** — each
daemon is independent; the phone simply stores N paired servers. The
desktop pairs each workspace separately. (So `<workspace>.<domain>` in the
sketch is really "the domain you point at this server"; per-workspace
subdomains are a DNS choice the user makes in Cloudflare, not something the
daemon manages.) Noted so we don't over-build.

## 4. 70.A — add-on `nginx-proxy`

Manifest (new entry in `winmux-addons::builtin_registry`):
- `id: "nginx-proxy"`, `needs_sudo: true`, `dependencies: []`.
- `detect`: `systemctl is-active nginx 2>/dev/null` (prints `active`/empty)
  — and, if active, also report cert presence so the UI can show "cert
  valid until …".
- `install`: a **Builtin** routine (not a raw `Shell`), because it needs:
  parameters (`domain`, `cf_token`), the root/sudo wrapper (§3.1), secret
  handling for the CF token, and idempotency. The routine writes the
  installer script to a temp file on the remote and runs it under the
  resolved privilege escalation.
- `uninstall`: disable+remove the nginx site, `certbot delete` the cert,
  remove `/etc/winmux/cloudflare.ini`. Leave nginx itself installed (other
  things may use it) unless a `purge` flag is set.

Installer script (essentials — Rule #3: the desktop never string-concats
the domain/token into a shell line; they're passed as **positional args**
to a script file, and validated first):
- `apt-get update && apt-get install -y nginx certbot python3-certbot-dns-cloudflare`
- write `/etc/winmux/cloudflare.ini` (mode 600) with the CF token,
- `certbot certonly --dns-cloudflare --dns-cloudflare-credentials … -d $DOMAIN
  --non-interactive --agree-tos -m admin@$DOMAIN` (DNS-01),
- write `/etc/nginx/sites-available/winmux-$DOMAIN` with the WS-aware proxy
  block (Upgrade/Connection headers, `proxy_read_timeout 86400`),
- symlink into `sites-enabled`, `nginx -t && systemctl reload nginx`,
- a renewal hook `/etc/letsencrypt/renewal-hooks/post/reload-nginx.sh`.

Hardening beyond the sketch:
- **delete the CF token file after issuance?** No — certbot's auto-renew
  needs it. Keep it mode-600 root-only; document that it lives there.
- nginx block adds: `ssl_protocols TLSv1.2 TLSv1.3;`, HSTS, a
  `limit_req` zone for basic rate limiting, and a `proxy_set_header
  X-Forwarded-Proto https;` so the daemon can trust it's behind TLS.
- **idempotent**: re-running install detects an existing cert (skip
  certbot if valid) and overwrites the site config cleanly.
- domain validated against a strict regex before it ever reaches the shell.

## 5. 70.B — daemon pairing endpoints

New `chat_pairing.go` (flat `package main`, consistent with the daemon).

```
POST   /api/pairing/issue     (admin)  -> { device_id, one_shot_token, expires_at }
POST   /api/pairing/redeem    (one-shot) -> { device_id, long_term_token }
GET    /api/pairing/devices   (admin)  -> [ PairedDevice ]
DELETE /api/pairing/devices/{id} (admin)
PUT    /api/pairing/devices/{id}/name (admin)
```

- **issue**: admin (desktop shared token) creates a `pending` device with a
  one-shot token (sha256 stored), TTL 5 min, scopes = all.
- **redeem**: the phone exchanges the one-shot for a **long-term token**.
  One-shot is single-use (deleted on redeem) and must be `pending` + not
  expired. Returns the long-term token **once** (hash stored). Marks
  device `active`, records `last_seen`/`last_ip`.
- The long-term token is then the **device bearer** for all REST/WS calls
  (Insights + chat), via the existing 69 auth — now backed by
  `paired_devices`.

Schema (replaces 69.D `devices`):
```sql
CREATE TABLE IF NOT EXISTS paired_devices (
  device_id   TEXT PRIMARY KEY,
  device_name TEXT,
  token_hash  TEXT,          -- sha256 of the long-term token (Rule #2)
  ots_hash    TEXT,          -- sha256 of the one-shot, NULL after redeem
  scopes      TEXT,          -- JSON array; "all" for now (decision #4)
  status      TEXT,          -- pending | active | revoked
  created_at  INTEGER,
  expires_at  INTEGER,       -- one-shot expiry (pending only)
  last_seen   INTEGER,
  last_ip     TEXT
);
```
`last_ip` comes from nginx's `X-Real-IP` / `X-Forwarded-For` (the daemon is
behind the proxy, so `RemoteAddr` is always 127.0.0.1).

## 6. 70.C — Desktop UI: Monitor → 📱 Mobile tab

A new tab in the Monitor (or a dedicated window). Three sections:

1. **Setup**: Domain field (regex + optional DNS-resolves check), Cloudflare
   API Token field (masked) with a **Test** button (validates the token via
   a CF API ping over SSH), and an **Install nginx + cert** button →
   `mobile_pairing_init`. Status row: nginx active? cert valid-until? domain
   resolves to this server's IP?
2. **Pairing**: **+ Pair New Device** → modal with a device-name field
   (scopes shown as "all", per decision #4), **Generate QR** →
   `mobile_pairing_generate_qr`, a large QR + a 5-min countdown. Paired-
   device list with rename / revoke / last-seen.
3. **Activity**: recent connections per device (from `last_seen`/`last_ip`).

QR payload (corrected per §3.2 — fingerprint informational, WebPKI is the
trust anchor):
```json
{
  "version": 1,
  "host": "<domain>",
  "port": 443,
  "tls": true,
  "device_id": "dev_xxx",
  "token": "<one-shot>",
  "expires_at": "<iso>"
}
```

## 7. 70.D — Tauri commands

```rust
mobile_pairing_init(workspace_id, domain, cf_token)   -> PairingStatus
mobile_pairing_generate_qr(workspace_id, device_name) -> QrPayload
mobile_pairing_list_devices(workspace_id)             -> Vec<PairedDevice>
mobile_pairing_revoke(workspace_id, device_id)        -> ()
mobile_pairing_rename(workspace_id, device_id, name)  -> ()
```
All `Result<_, String>` (Rule #6). `mobile_pairing_init` runs the
`nginx-proxy` add-on install with the params.

**CF token handling (Rule #2):** the token is collected in the UI, passed to
the Tauri command, used to write the remote `cloudflare.ini`, and **zeroized
in memory** after the install command returns (wrap in a `Zeroizing<String>`
/ explicit zeroize; never persisted desktop-side, never logged — Rule #8
discipline). It DOES persist remote-side in `/etc/winmux/cloudflare.ini`
(mode 600, root) because certbot's auto-renew needs it; documented.

## 8. Security considerations (consolidated)
- Daemon stays localhost-only; nginx is the sole public surface (§1).
- TLS via WebPKI (LE), TLS1.2+; HSTS; nginx rate limiting; optional IP
  allowlist.
- One-shot tokens: 5-min TTL, single redeem, hashed at rest.
- Long-term tokens: hashed at rest, revocable, audit via last_seen/last_ip.
- CF token: zeroized desktop-side after use; mode-600 root-only remote;
  never logged (Rule #2/#8).
- All shell params validated + passed as script args, never concatenated
  (Rule #3).
- Accepted residual risk: all-scopes device token ≈ RCE (§3.3) — mitigated
  by TLS + revocation + expiry + audit.

## 9. Build order
1. **70.A** — `nginx-proxy` add-on + installer routine + the root/sudo
   wrapper (§3.1). Detect/install/uninstall. Test on a root VPS.
2. **70.B** — pairing endpoints + `paired_devices` schema (replacing 69.D
   `devices`); issue/redeem/list/revoke/rename; unit tests (issue→redeem→
   auth→revoke, expiry, single-use).
3. **70.C** — Mobile tab UI (setup / pairing / activity) + QR rendering.
4. **70.D** — Tauri commands + DPAPI/zeroize for the CF token.
5. **integration** — pair → phone connects over wss → status updates →
   revoke. Reconcile daemon version → 1.2.0 (§2).

## 10. Open questions / confirmations
1. §3.1 sudo: ship root-or-NOPASSWD now, defer interactive sudo password? 
2. §3.2 cert trust: WebPKI-only, drop leaf pinning? 
3. §3.3: accept all-scopes RCE surface with the listed controls? 
4. §3.4: replace 69.D `devices` with `paired_devices`? 
5. QR/transport: confirm the phone validates via system trust store (no
   pinning) and connects `wss://<domain>:443`.
6. Renewal: keep `cloudflare.ini` on the server for auto-renew (required) —
   OK that the CF token persists there mode-600 root?

---
*On approval I'll build 70.A→D on this branch, each its own commit, with the
daemon pairing tests as the acceptance gate (a Go client replicating the
issue→redeem→authenticated-call flow), exactly as Phase 69. Nothing to
`main`.*
