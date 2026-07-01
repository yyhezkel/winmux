# Phase 77 smoke test — winmux-server 2.0.0 (app v0.4.3)

Manual checklist for validating the Phase 77 branch (`77-winmux-server`) before
merge. Run with `winmux-DEBUG-test.exe` (embeds the 2.0.0 daemon + the new data
dir). Two remote hosts recommended: one **fresh**, one with an **existing 1.x
install** to exercise the migration.

Legend: ☐ = to verify.

## 1. Fresh install (host with no prior winmux daemon)

- ☐ Connect to the remote in a workspace; open **Monitor**.
- ☐ Settings → Add-ons → **Insights → Install** succeeds.
- ☐ On the remote: `~/.winmux/bin/winmux-server --version` prints
  `winmux-server 2.0.0`; `~/.winmux/bin/winmux-insights` symlink resolves to it.
- ☐ Data dir **`~/.winmux/server/`** exists (not `insights/`) and contains
  `token`, `metrics.db`, `workspace.db`, `insights.log`.
- ☐ systemd: `systemctl --user status winmux-server` is **active**; no
  `winmux-insights.service` present.
- ☐ Monitor shows live metrics (CPU/mem/disk). `curl -s localhost:7879/healthz`
  (via the tunnel) returns `{"ok":true,"version":"2.0.0","uptime_seconds":…}`.
- ☐ `GET /api/version` → `api_versions:[2]`, `frame_version:2`.

## 2. Upgrade path (host with an existing `~/.winmux/insights/` from 1.x)

> Precondition: a 1.x daemon previously installed, with a paired mobile device
> (so `chat.db`/`paired_devices` + `token` exist under `insights/`).

- ☐ Note the old token: `cat ~/.winmux/insights/token`.
- ☐ Install Insights from the new desktop (Add-ons → Insights → Install).
- ☐ After first boot: **`~/.winmux/server/` exists, `~/.winmux/insights/` is
  gone** (atomic move). Log shows `migrated data dir … → …/server`.
- ☐ Token preserved: `cat ~/.winmux/server/token` **equals** the noted value.
- ☐ `chat.db` present under `server/`; **previously paired devices still work**
  (Mobile tab shows them; a paired phone can still reach the server).
- ☐ Idempotent: restart the daemon → no second migration, no data loss.

## 3. Existing features (regression)

- ☐ **Insights Monitor** — metrics + history render.
- ☐ **Docker section** — containers list (or the clean "unavailable" panel).
- ☐ **Hygiene / Cleanup tab** — port-watchers + orphan sessions list, kill works.
- ☐ **Mobile pairing UI** — QR generates; a device can pair (split-QR + token).
- ☐ **Claude Code hooks** — a hook prompt round-trips (approve/deny) on the
  desktop. (Legacy mobile chat `/api/claude/*` now returns **410** by design.)
- ☐ **Logs tab** — per-client logs + the `server` pseudo-client stream.

## 4. Version

- ☐ Settings → **Updates** shows the app at **v0.4.3**.
- ☐ `app.exe` title/about reflects 0.4.3.

## 5. Contract (optional, dev machine)

- ☐ `cd sdk-gen && npm ci && node ci-check.mjs` → "SDKs are in sync".
- ☐ `cd sdk/typescript && node test/contract.mjs` → contract OK against a locally
  built `winmux-server`.

---

**Rollback rehearsal (optional):** on the upgraded host, `mv ~/.winmux/server
~/.winmux/insights`, install an older winmux, confirm the 1.x daemon comes back
with the same token/devices. (See `docs/winmux-server/UPGRADE.md`.)
