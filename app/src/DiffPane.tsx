// Phase 50 (#2.4): live unified-diff pane.
//
// On mount, we tell the backend the persisted source (or default
// Working) — that (re)starts the per-pane watcher task. The watcher
// emits `diff-pane-updated` events; we filter by pane_id and rerender.
// Source dropdown calls diff_pane_set_source again; Refresh button
// calls diff_pane_refresh for an immediate one-shot.
//
// Diff parsing is in-house — no extra deps. We slice the unified-diff
// text into file-headers and hunks so ↑/↓ can scroll to the next hunk
// and we can render `+`/`-`/` ` lines with separate gutter colours.

import { createSignal, createMemo, For, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { DiffSource } from "./bindings/DiffSource";
import type { LayoutNode } from "./types";
import { t } from "./i18n";
import { keyEq } from "./shortcuts";
import { TechText } from "./TechText";
import { IconGitCompare, IconChevronDown, IconClose } from "./icons";

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  isActive: boolean;
  onFocus: (paneId: string) => void;
  onClose: (paneId: string) => void;
}

type DiffLine =
  | { kind: "context"; text: string }
  | { kind: "add"; text: string }
  | { kind: "del"; text: string }
  | { kind: "hunk"; text: string }       // @@ header
  | { kind: "file"; text: string };      // "diff --git ..." / "+++ ..." / "--- ..."

interface Hunk {
  fileLabel: string;
  headerIdx: number;                      // index into `lines` of the @@ header
  lineSpan: [number, number];             // [start, endExclusive) in `lines`
}

interface ParsedDiff {
  lines: DiffLine[];
  hunks: Hunk[];
}

// Parse a unified-diff blob into renderable lines + hunk index. The
// grammar we honour:
//   - file header runs start with "diff --git " and continue until the
//     first "@@" or another "diff --git"
//   - hunk header lines start with "@@"
//   - everything else inside a hunk is " " context, "+" add, "-" del
function parseDiff(text: string): ParsedDiff {
  const out: DiffLine[] = [];
  const hunks: Hunk[] = [];
  if (!text) return { lines: out, hunks };
  const src = text.split("\n");
  let currentFile = "";
  let inHunk = false;
  let hunkStart = -1;
  const closeHunk = (endExclusive: number) => {
    if (hunkStart >= 0) {
      hunks.push({
        fileLabel: currentFile,
        headerIdx: hunkStart,
        lineSpan: [hunkStart, endExclusive],
      });
    }
    inHunk = false;
    hunkStart = -1;
  };
  for (let i = 0; i < src.length; i++) {
    const raw = src[i];
    if (raw.startsWith("diff --git ")) {
      closeHunk(out.length);
      // Pull the destination path ("b/<path>") out of the header.
      const m = raw.match(/ b\/(\S+)/);
      currentFile = m ? m[1] : raw.slice("diff --git ".length);
      out.push({ kind: "file", text: raw });
      continue;
    }
    if (raw.startsWith("--- ") || raw.startsWith("+++ ") ||
        raw.startsWith("index ") || raw.startsWith("new file mode") ||
        raw.startsWith("deleted file mode") || raw.startsWith("similarity index") ||
        raw.startsWith("rename from ") || raw.startsWith("rename to ")) {
      out.push({ kind: "file", text: raw });
      continue;
    }
    if (raw.startsWith("@@")) {
      closeHunk(out.length);
      hunkStart = out.length;
      inHunk = true;
      out.push({ kind: "hunk", text: raw });
      continue;
    }
    if (!inHunk) {
      // Trailing blank line between files — ignore.
      if (raw.length === 0) continue;
      out.push({ kind: "file", text: raw });
      continue;
    }
    if (raw.startsWith("+")) {
      out.push({ kind: "add", text: raw.slice(1) });
    } else if (raw.startsWith("-")) {
      out.push({ kind: "del", text: raw.slice(1) });
    } else if (raw.startsWith(" ") || raw.length === 0) {
      out.push({ kind: "context", text: raw.length === 0 ? "" : raw.slice(1) });
    } else if (raw === "\\ No newline at end of file") {
      out.push({ kind: "context", text: raw });
    } else {
      // Unknown leading byte (shouldn't happen with --no-color) —
      // render verbatim as context so nothing is silently dropped.
      out.push({ kind: "context", text: raw });
    }
  }
  closeHunk(out.length);
  return { lines: out, hunks };
}

function describeSource(s: DiffSource): string {
  switch (s.kind) {
    case "working": return t("diff.pane.source.working");
    case "head": return t("diff.pane.source.head");
    case "ref": return t("diff.pane.source.ref");
  }
}

export function DiffPane(p: Props) {
  let bodyRef!: HTMLDivElement;
  const initialSource: DiffSource =
    (p.pane.diff_source as DiffSource | null) ?? { kind: "working" };
  const [source, setSource] = createSignal<DiffSource>(initialSource);
  const [diffText, setDiffText] = createSignal<string>("");
  const [isGitRepo, setIsGitRepo] = createSignal<boolean>(true);
  const [busy, setBusy] = createSignal<boolean>(false);
  const [hunkIdx, setHunkIdx] = createSignal<number>(0);
  const [refDraft, setRefDraft] = createSignal<string>(
    initialSource.kind === "ref" ? initialSource.git_ref : "",
  );
  const [refEditing, setRefEditing] = createSignal<boolean>(false);
  const [menuOpen, setMenuOpen] = createSignal<boolean>(false);

  const parsed = createMemo(() => parseDiff(diffText()));
  const isEmpty = () => isGitRepo() && diffText().trim().length === 0;

  const apply = async (next: DiffSource) => {
    setSource(next);
    setBusy(true);
    try {
      await invoke("diff_pane_set_source", {
        paneId: p.pane.pane_id,
        source: next,
      });
    } catch (e) {
      console.error("diff_pane_set_source failed", e);
    } finally {
      setBusy(false);
    }
  };

  const refresh = async () => {
    setBusy(true);
    try {
      await invoke("diff_pane_refresh", { paneId: p.pane.pane_id });
    } catch {
      // diff_pane_refresh already emits an error event with
      // is_git_repo: false; ignore the rejected promise.
    } finally {
      setBusy(false);
    }
  };

  // ↑/↓ jump between hunks. j/k aliases match the muscle memory of
  // less / git's pager.
  const onBodyKey = (e: KeyboardEvent) => {
    const total = parsed().hunks.length;
    if (total === 0) return;
    let delta = 0;
    // Phase 62.B (item G): keyEq for the vim-style j/k so they work on a
    // Hebrew layout; arrows are layout-independent already.
    if (e.key === "ArrowDown" || keyEq(e, "j")) delta = 1;
    else if (e.key === "ArrowUp" || keyEq(e, "k")) delta = -1;
    else return;
    e.preventDefault();
    const next = Math.max(0, Math.min(total - 1, hunkIdx() + delta));
    setHunkIdx(next);
    // Scroll the hunk's header line into view.
    const headerLine = parsed().hunks[next].headerIdx;
    const el = bodyRef.querySelector(
      `[data-line-idx="${headerLine}"]`,
    ) as HTMLElement | null;
    if (el) el.scrollIntoView({ block: "center", behavior: "smooth" });
  };

  onMount(() => {
    // Tell the backend to (re)start the watcher with whatever source
    // we believe is current. This also covers the cold-start case for
    // a Diff pane loaded from workspaces.json.
    void apply(source());
    let unlisten: UnlistenFn | undefined;
    void (async () => {
      try {
        unlisten = await listen<{
          pane_id: string;
          diff_text: string;
          is_git_repo: boolean;
        }>("diff-pane-updated", (event) => {
          if (event.payload.pane_id !== p.pane.pane_id) return;
          setDiffText(event.payload.diff_text);
          setIsGitRepo(event.payload.is_git_repo);
          // Clamp hunk index if the diff shrank.
          const total = parseDiff(event.payload.diff_text).hunks.length;
          if (hunkIdx() >= total) setHunkIdx(0);
        });
      } catch (e) {
        console.warn("diff_pane: listen failed", e);
      }
    })();
    onCleanup(() => {
      try { unlisten?.(); } catch {}
    });
  });

  return (
    <div
      class={`pane diff-pane ${p.isActive ? "active" : ""}`}
      onMouseDown={() => p.onFocus(p.pane.pane_id)}
    >
      <div class="pane-header">
        <span class="pane-conn"><IconGitCompare size={14} /> diff</span>
        <div class="diff-pane-source">
          <button
            class="ws-header-btn"
            disabled={busy()}
            onClick={() => setMenuOpen(!menuOpen())}
          >
            {describeSource(source())}
            <Show when={source().kind === "ref"}>
              {" "}<TechText text={(source() as { kind: "ref"; git_ref: string }).git_ref} />
            </Show>
            {" "}<IconChevronDown size={13} />
          </button>
          <Show when={menuOpen()}>
            <div class="diff-pane-menu">
              <button onClick={() => { setMenuOpen(false); void apply({ kind: "working" }); }}>
                {t("diff.pane.source.working")}
              </button>
              <button onClick={() => { setMenuOpen(false); void apply({ kind: "head" }); }}>
                {t("diff.pane.source.head")}
              </button>
              <button onClick={() => { setMenuOpen(false); setRefEditing(true); }}>
                {t("diff.pane.source.ref")}…
              </button>
            </div>
          </Show>
        </div>
        <button
          class="ws-header-btn"
          disabled={busy()}
          onClick={() => void refresh()}
        >
          {t("diff.pane.refresh")}
        </button>
        <button
          class="pane-btn pane-close"
          title={t("common.close")}
          onClick={(e) => { e.stopPropagation(); p.onClose(p.pane.pane_id); }}
        >
          <IconClose size={14} />
        </button>
      </div>

      <Show when={refEditing()}>
        <div class="diff-pane-ref-row">
          <input
            type="text"
            value={refDraft()}
            placeholder="HEAD~1 / main / abc123"
            onInput={(e) => setRefDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                const v = refDraft().trim();
                if (v) {
                  setRefEditing(false);
                  void apply({ kind: "ref", git_ref: v });
                }
              } else if (e.key === "Escape") {
                setRefEditing(false);
              }
            }}
          />
          <button onClick={() => {
            const v = refDraft().trim();
            if (v) { setRefEditing(false); void apply({ kind: "ref", git_ref: v }); }
          }}>
            {t("common.save")}
          </button>
          <button onClick={() => setRefEditing(false)}>
            {t("common.cancel")}
          </button>
        </div>
      </Show>

      <div
        class="pane-body diff-pane-body"
        ref={(el) => (bodyRef = el)}
        tabIndex={0}
        onKeyDown={onBodyKey}
      >
        <Show when={!isGitRepo()}>
          <p class="diff-pane-msg">{t("diff.pane.notGitRepo")}</p>
        </Show>
        <Show when={isGitRepo() && isEmpty()}>
          <p class="diff-pane-msg">{t("diff.pane.empty")}</p>
        </Show>
        <Show when={isGitRepo() && !isEmpty()}>
          <pre class="diff-pane-pre">
            <For each={parsed().lines}>
              {(line, idx) => (
                <div class={`dl dl-${line.kind}`} data-line-idx={idx()}>
                  <span class="dl-gutter">
                    {line.kind === "add" ? "+" :
                     line.kind === "del" ? "-" :
                     line.kind === "hunk" ? "@" :
                     line.kind === "file" ? "·" : " "}
                  </span>
                  <span class="dl-text">{line.text}</span>
                </div>
              )}
            </For>
          </pre>
        </Show>
      </div>
    </div>
  );
}
