# Upgrading to winmux-server 2.0 (from winmux-insights 1.x)

Phase 77 renamed the daemon `winmux-insights` → **`winmux-server` 2.0.0** and
restructured it internally. The upgrade is **in-place and data-preserving** — the
data directory does not move.

## What the desktop does on upgrade

1. Uploads the new binary to `~/.winmux/bin/winmux-server` and points the
   `winmux-insights` symlink at it (so version probes keep working).
2. Disables + removes the old `winmux-insights` systemd unit; installs and starts
   `winmux-server` (see [DEPLOYMENT.md](DEPLOYMENT.md)).
3. On its first boot the 2.0 daemon **migrates the data dir
   `~/.winmux/insights/` → `~/.winmux/server/`** — an atomic whole-directory
   move (both under `~/.winmux`, same filesystem). `token`, `metrics.db`,
   `chat.db`, `paired_devices` (in `chat.db`), `workspace.db`, and `logs/` all
   carry over. The migration is idempotent + guarded: it runs only when
   `insights/` has a `token` and `server/` doesn't yet, so a re-run or a fresh
   install is a no-op. An explicit `--dir` opts out entirely.

No manual steps are required.

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

The 2.0 daemon moved the data dir to `~/.winmux/server/`. To roll back to a 1.x
release (which reads `~/.winmux/insights/`), move the data back first:

```sh
mv ~/.winmux/server ~/.winmux/insights   # then install the older winmux release
```

A 1.x binary started without this move would create a fresh, empty
`~/.winmux/insights/` (losing the paired devices + token), so do the move before
downgrading. If you never downgrade, nothing is needed — 2.0 owns `server/`.
