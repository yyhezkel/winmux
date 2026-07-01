# Upgrading to winmux-server 2.0 (from winmux-insights 1.x)

Phase 77 renamed the daemon `winmux-insights` → **`winmux-server` 2.0.0** and
restructured it internally. The upgrade is **in-place and data-preserving** — the
data directory does not move.

## What the desktop does on upgrade

1. Uploads the new binary to `~/.winmux/bin/winmux-server` and points the
   `winmux-insights` symlink at it (so version probes keep working).
2. Disables + removes the old `winmux-insights` systemd unit; installs and starts
   `winmux-server` (see [DEPLOYMENT.md](DEPLOYMENT.md)).
3. Leaves the data dir **`~/.winmux/insights/`** exactly where it is — `token`,
   `metrics.db`, `chat.db`, `paired_devices` (in `chat.db`), `workspace.db`, and
   `logs/` all carry over untouched.

No manual migration is required.

## Behavioural changes to know

- **Legacy mobile chat HTTP is retired.** `/api/claude/*` and `/ws/claude/*` now
  return **410 Gone**. There were no production mobile devices, so this is a
  clean break — clients drive Claude sessions through `/api/v2/workspace/*`
  (frame contract in [CLIENTS.md](CLIENTS.md)). **Pairing and Insights are
  unchanged** (desktop-facing, full backward compat).
- **`/api/v2/*` is the versioned surface.** Insights legacy paths (`/current`,
  `/docker`, …) still work for the desktop Monitor.
- The API contract is published + generated: `GET /api/openapi.json`,
  `/api/asyncapi.json`, `/api/frames.schema.json`.

## Rollback

The 2.0 binary reads the same data dir as 1.x, so a rollback is just
re-installing the previous version (Settings → Updates → install an earlier
release) — no data conversion happened, so nothing needs undoing. Keep a copy of
the old `winmux-insights` binary if you want a manual fallback:
`cp ~/.winmux/bin/winmux-server ~/.winmux/bin/winmux-server.bak` before upgrading.

## Data-directory name

The data dir is still called `insights/` (not `server/`). Renaming it is a
cosmetic, deployment-coupled change deferred intentionally — see DECISIONS.md.
Nothing functional depends on the name.
