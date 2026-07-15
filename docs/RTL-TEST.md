# RTL per-line direction — test matrix (v0.4.4, Approach C)

## TUI-owned BiDi (Claude Code visual-order output) — 2026-07-15

Claude Code (>= 2.1.74, verified on 2.1.210 via a live ConPTY probe) writes
RTL text to the PTY **pre-reordered into visual order** — typing
`מה קורה בסדר?` into its input box emits `?רדסב הרוק המ`. Our per-row
`dir="rtl"` then bidi's it a *second* time: letters double-reverse back to
looking correct, but neutrals (`?`, `.`, brackets, the `>` prompt) resolve
against the RTL paragraph and land at the wrong end of the line.

Fix (`nextTuiOwnsBidi` in `textDirection.ts` + `tuiOwnsBidi` in
`terminalInstance.ts`): while Claude holds a pane's foreground, that pane's
rows render plain LTR (what Windows Terminal shows), arrow-mirroring stands
down, and the legacy `bidi_reorder` write pipeline is bypassed. Detection is
terminal-title driven — ON on a title containing "claude" (set at startup),
OFF on an empty title (Claude's exit cleanup), hold on anything else
(Claude's auto topic titles can be any text).

Smoke test:
1. Local pane → run `claude` → type `מה קורה בסדר?` → the `?` sits at the
   line **end** (left of the Hebrew), letters read correctly, `>` prompt is
   not mirrored to `‹`.
2. Exit Claude (double Ctrl+C) → `echo "שרת רץ על port 4200"` in the shell →
   the line is right-aligned RTL again (state released).
3. `debug.log` shows `tui-owns-bidi on/off pane=…` transitions (metadata
   only — titles are never logged).

Known limits: remote panes need the bundled tmux config to forward titles
(`set-titles`) — not wired yet, see FOLLOWUPS. Claude scrollback rendered
*after* Claude exits flips back to per-line RTL (state is temporal, rows
carry no provenance).

winmux gives every **visible** terminal row an explicit `dir` computed from its
text by `detectDirection()` (`app/src/textDirection.ts`), replacing xterm.js's
`dir="auto"` ("first strong directional character wins"), which mis-rendered a
mixed Hebrew+Latin line that happened to start with Latin.

**Rule (Yossi):** a line with **any** Hebrew/Arabic char → **RTL** (mixed OR
pure); a **pure-Latin** line → **LTR**; digits / symbols / whitespace only →
**LTR** (safe default).

Only affects the `auto_per_line` RTL mode (the default). Gated by
**Settings → Terminal → "Auto-direction per line"** (default ON).

## Unit tests

`app/src/textDirection.test.ts` — 23 cases (`node:test`). Run:

```
cd app && node --experimental-strip-types --test src/textDirection.test.ts
```

## Detection matrix

| Input | Expected | Why |
|-------|----------|-----|
| `1. Hello world` | **LTR** | pure Latin |
| `1. שלום עולם` | **RTL** | pure Hebrew |
| `1. שלום world` | **RTL** | mixed → RTL |
| `/opt/wa/.shared.env` | **LTR** | pure ASCII path |
| `שרת רץ על port 4200` | **RTL** | mixed Hebrew + latin/digits |
| `` (empty) | **LTR** | safe default |
| `12345` | **LTR** | digits only |
| `→ ← ↑ ↓` | **LTR** | arrows/symbols only |
| `$ ls -la /home` | **LTR** | shell prompt + command |
| `git commit -m 'תיקון'` | **RTL** | Latin command with Hebrew arg |
| `ERROR: קובץ לא נמצא` | **RTL** | Latin word then Hebrew |
| `مرحبا بالعالم` | **RTL** | pure Arabic |
| `run مرحبا now` | **RTL** | mixed Arabic + Latin |
| `הפורט 4200 פתוח` | **RTL** | Hebrew wrapping a number |

Within an RTL line, embedded Latin runs (paths, `port 4200`) keep their natural
LTR order via the browser's BiDi algorithm — the row's paragraph direction is
RTL, the runs are not reversed.

## Visual smoke test (real-world)

1. RTL mode ON (Hebrew UI). Connect a shell; `printf` a numbered list where one
   item is a pure ASCII path and the others start with Hebrew → the path line
   sits LTR, the Hebrew/mixed lines sit RTL, list markers align on the right.
2. `echo "שרת רץ על port 4200"` → whole line RTL; "port 4200" reads L-to-R
   inside it.
3. `cat` a source file (pure Latin) → unchanged LTR, no flips.
4. Toggle **Settings → Terminal → Auto-direction per line = OFF** → every row
   renders LTR (classic terminal). Toggle back ON → per-line detection resumes
   live (no reconnect).

## Performance

- Only **visible** rows carry DOM nodes, so scrollback size (up to millions of
  lines) is irrelevant — the pass touches ~24–50 rows max.
- Row mutations are coalesced to **one pass per animation frame**
  (`requestAnimationFrame`).
- A per-row text cache (`WeakMap<Element,string>`) skips any row whose text is
  unchanged since the last pass.

## Cursor interaction (PARKED "RTL caret", 2026-06-26)

`isCurrentLineRtl()` (the Left/Right arrow-mirroring gate) now uses the **same**
`detectDirection()` rule, so the caret/arrow behaviour matches the visual
direction on mixed lines. Candidate fix for the parked caret item — **verify
live** before marking it resolved.
