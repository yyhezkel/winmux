import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { t } from "./i18n";
import type {
  ClaudeLogEntry,
  ClaudeLogSummary,
  ClaudeSyncResult,
  LayoutNode,
  WorkspacesFile,
} from "./types";

// Phase 24.B: HTML-bubble view over locally-synced Claude conversation
// transcripts. Backend (Phase 24.A) handles the SFTP mirror; this
// component is pure FE rendering off the local jsonl files.

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  isActive: boolean;
  onFocus: (paneId: string) => void;
  onClose: (paneId: string) => void;
  onFileUpdate: (f: WorkspacesFile) => void;
}

function fmtAge(epochSecs: number): string {
  if (!epochSecs) return "—";
  const now = Math.floor(Date.now() / 1000);
  const diff = Math.max(0, now - epochSecs);
  if (diff < 60) return `${diff}s`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function ClaudeLogPane(p: Props) {
  // ─── state ──────────────────────────────────────────────────────────────
  const persistedState = createMemo(() => p.pane.claudelog ?? {});
  const sessionId = () => persistedState().session_id ?? null;
  const persistedFilter = () => persistedState().filter ?? "";

  // Local working copy of the filter — debounced-persisted to the
  // backend so every keystroke doesn't trigger a workspace re-save.
  const [filterDraft, setFilterDraft] = createSignal(persistedFilter());
  let filterPersistTimer: number | null = null;

  const [summaries, setSummaries] = createSignal<ClaudeLogSummary[]>([]);
  const [summariesLoaded, setSummariesLoaded] = createSignal(false);
  const [entries, setEntries] = createSignal<ClaudeLogEntry[]>([]);
  const [entriesLoading, setEntriesLoading] = createSignal(false);

  const [syncing, setSyncing] = createSignal(false);
  const [lastSyncAt, setLastSyncAt] = createSignal<number | null>(null);
  const [syncErrors, setSyncErrors] = createSignal<string[]>([]);
  const [errorBanner, setErrorBanner] = createSignal<string | null>(null);

  // ─── data fetchers ──────────────────────────────────────────────────────
  const fetchSummaries = async () => {
    try {
      const list = await invoke<ClaudeLogSummary[]>("claude_log_list", {
        workspaceId: p.workspaceId,
      });
      setSummaries(list ?? []);
      setSummariesLoaded(true);
    } catch (e) {
      setErrorBanner(String(e));
    }
  };

  const fetchEntries = async (sid: string) => {
    setEntriesLoading(true);
    try {
      const list = await invoke<ClaudeLogEntry[]>("claude_log_read", {
        workspaceId: p.workspaceId,
        sessionId: sid,
      });
      setEntries(list ?? []);
      setErrorBanner(null);
    } catch (e) {
      setErrorBanner(String(e));
      setEntries([]);
    } finally {
      setEntriesLoading(false);
    }
  };

  const runSync = async () => {
    if (syncing()) return;
    setSyncing(true);
    setSyncErrors([]);
    try {
      const r = await invoke<ClaudeSyncResult>("claude_log_sync", {
        workspaceId: p.workspaceId,
        sessionId: null,
      });
      setSyncErrors(r.errors ?? []);
      setLastSyncAt(Math.floor(Date.now() / 1000));
      await fetchSummaries();
      // If we have a session open, re-fetch its entries so newly-
      // synced content shows up immediately.
      const sid = sessionId();
      if (sid) {
        await fetchEntries(sid);
      }
    } catch (e) {
      setErrorBanner(String(e));
    } finally {
      setSyncing(false);
    }
  };

  // ─── selection / filter persistence ─────────────────────────────────────
  const selectSession = async (sid: string) => {
    try {
      const f = await invoke<WorkspacesFile>("claude_log_pane_set", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        sessionId: sid,
        filter: null,
      });
      p.onFileUpdate(f);
    } catch (e) {
      setErrorBanner(String(e));
    }
  };

  const clearSession = async () => {
    try {
      const f = await invoke<WorkspacesFile>("claude_log_pane_set", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        sessionId: "",
        filter: null,
      });
      p.onFileUpdate(f);
      setEntries([]);
    } catch (e) {
      setErrorBanner(String(e));
    }
  };

  const persistFilter = (value: string) => {
    if (filterPersistTimer) clearTimeout(filterPersistTimer);
    filterPersistTimer = window.setTimeout(async () => {
      try {
        const f = await invoke<WorkspacesFile>("claude_log_pane_set", {
          workspaceId: p.workspaceId,
          paneId: p.pane.pane_id,
          sessionId: null,
          filter: value,
        });
        p.onFileUpdate(f);
      } catch (e) {
        console.warn("claude_log filter persist failed", e);
      }
    }, 500);
  };

  // ─── lifecycle ──────────────────────────────────────────────────────────
  let pollTimer: number | null = null;
  const unlistens: UnlistenFn[] = [];

  onMount(async () => {
    await fetchSummaries();
    const sid = sessionId();
    if (sid) {
      await fetchEntries(sid);
    }
    // Poll every 5s while pane is visible. Cheap (local dir scan).
    pollTimer = window.setInterval(() => {
      void fetchSummaries();
    }, 5000);
    // workspaces:changed fires when claude_log_pane_set commits — but
    // the parent already calls onFileUpdate, so we only need this for
    // external mutations (other panes touching this workspace).
    unlistens.push(
      await listen("workspaces:changed", () => {
        // No-op for now; future hook point.
      }),
    );
  });

  onCleanup(() => {
    if (pollTimer) clearInterval(pollTimer);
    if (filterPersistTimer) clearTimeout(filterPersistTimer);
    for (const u of unlistens) {
      try {
        u();
      } catch {
        /* ignore */
      }
    }
  });

  // When the persisted session_id changes (from selectSession / external
  // mutation), refetch entries.
  createEffect(() => {
    const sid = sessionId();
    if (sid) {
      void fetchEntries(sid);
    } else {
      setEntries([]);
    }
  });

  // Keep filterDraft in sync if the persisted value changes externally.
  createEffect(() => {
    const pf = persistedFilter();
    if (pf !== filterDraft()) setFilterDraft(pf);
  });

  // ─── derived rendering ──────────────────────────────────────────────────
  const filteredEntries = createMemo(() => {
    const q = filterDraft().trim().toLowerCase();
    if (!q) return entries();
    return entries().filter((e) => e.text.toLowerCase().includes(q));
  });

  const currentSummary = createMemo(() => {
    const sid = sessionId();
    if (!sid) return null;
    return summaries().find((s) => s.session_id === sid) ?? null;
  });

  const syncLabel = () => {
    const at = lastSyncAt();
    if (at === null) return t("cl.never_synced");
    return t("cl.last_synced", { age: fmtAge(at) });
  };

  // ─── render helpers ─────────────────────────────────────────────────────
  const renderBubble = (entry: ClaudeLogEntry) => {
    switch (entry.entry_type) {
      case "user":
        return (
          <div class="cl-bubble cl-bubble-user" dir="auto">
            {entry.text}
            <Show when={entry.timestamp}>
              <span class="cl-timestamp">{entry.timestamp}</span>
            </Show>
          </div>
        );
      case "assistant":
        return (
          <div class="cl-bubble cl-bubble-assistant" dir="auto">
            {entry.text}
            <Show when={entry.timestamp}>
              <span class="cl-timestamp">{entry.timestamp}</span>
            </Show>
          </div>
        );
      case "tool_use":
        return (
          <div class="cl-tool" dir="auto">
            {t("cl.tool_use", { tool: entry.tool_name ?? "?" })}
            <Show when={entry.text}>
              <div class="cl-tool-args">{entry.text}</div>
            </Show>
          </div>
        );
      case "tool_result":
        return (
          <div class="cl-tool cl-tool-result" dir="auto">
            {t("cl.tool_result", { tool: entry.tool_name ?? "?" })}
            <Show when={entry.text}>
              <div class="cl-tool-args">{entry.text}</div>
            </Show>
          </div>
        );
      case "system":
      case "summary":
        return (
          <div class="cl-summary" dir="auto">
            {entry.text}
          </div>
        );
      default:
        return null;
    }
  };

  return (
    <div
      class={`pane ${p.isActive ? "active" : ""}`}
      onMouseDown={() => p.onFocus(p.pane.pane_id)}
    >
      <div class="pane-header">
        <span class="pane-conn">🗒 {t("cl.title")}</span>
        <button
          class="pane-btn pane-close"
          title={t("common.close")}
          onClick={(e) => {
            e.stopPropagation();
            p.onClose(p.pane.pane_id);
          }}
        >
          ×
        </button>
      </div>
      <div class="pane-body">
        <div class="cl-pane">
          {/* ── toolbar ── */}
          <div class="cl-head">
            <button
              class="ws-header-btn"
              disabled={syncing()}
              onClick={(e) => {
                e.stopPropagation();
                void runSync();
              }}
            >
              {syncing() ? "⟳ …" : `⟳ ${t("cl.sync_now")}`}
            </button>
            <span class="cl-sync-info">{syncLabel()}</span>
            <Show when={sessionId()}>
              <button
                class="ws-header-btn"
                onClick={(e) => {
                  e.stopPropagation();
                  void clearSession();
                }}
                title={t("cl.pick_session")}
              >
                ↩
              </button>
            </Show>
            <Show when={sessionId()}>
              <input
                class="cl-filter"
                type="text"
                dir="auto"
                placeholder={t("cl.filter_placeholder")}
                value={filterDraft()}
                onInput={(e) => {
                  const v = e.currentTarget.value;
                  setFilterDraft(v);
                  persistFilter(v);
                }}
              />
            </Show>
          </div>

          {/* ── error banner ── */}
          <Show when={errorBanner()}>
            <div class="cl-error">
              ⚠ {errorBanner()}
              <button
                class="cl-error-x"
                onClick={(e) => {
                  e.stopPropagation();
                  setErrorBanner(null);
                }}
              >
                ×
              </button>
            </div>
          </Show>
          <Show when={syncErrors().length > 0}>
            <div class="cl-error">
              ⚠ {syncErrors().length} file(s) failed to sync
            </div>
          </Show>

          {/* ── content ── */}
          <Show when={sessionId() === null}>
            {/* Session picker */}
            <Show
              when={summariesLoaded() && summaries().length === 0}
              fallback={
                <div class="cl-list-wrap">
                  <h4 class="cl-section-title">{t("cl.pick_session")}</h4>
                  <ul class="claude-list">
                    <For each={summaries()}>
                      {(s) => (
                        <li
                          class="claude-row"
                          onClick={() => void selectSession(s.session_id)}
                          title={s.project_path ?? ""}
                        >
                          <div class="claude-row-head">
                            <code class="claude-id">
                              {s.session_id.slice(0, 8)}
                            </code>
                            <span class="claude-proj">
                              {s.project_path ?? "—"}
                            </span>
                            <span class="claude-age">
                              {fmtAge(s.local_mtime)} · {s.message_count} msg ·{" "}
                              {fmtSize(s.file_size)}
                            </span>
                          </div>
                          <Show when={s.first_user}>
                            <div class="claude-prev" dir="auto">
                              <b>{t("claude_picker.user_prefix")}</b>{" "}
                              {s.first_user}
                            </div>
                          </Show>
                          <Show when={s.last_assistant}>
                            <div class="claude-prev" dir="auto">
                              <b>{t("claude_picker.assistant_prefix")}</b>{" "}
                              {s.last_assistant}
                            </div>
                          </Show>
                        </li>
                      )}
                    </For>
                  </ul>
                </div>
              }
            >
              <div class="cl-empty">{t("cl.empty")}</div>
            </Show>
          </Show>

          <Show when={sessionId() !== null}>
            {/* Bubble stream */}
            <Show when={currentSummary()}>
              <div class="cl-meta">
                <code>{sessionId()!.slice(0, 8)}</code> ·{" "}
                {currentSummary()!.project_path ?? "—"} ·{" "}
                {currentSummary()!.message_count} msg ·{" "}
                {fmtSize(currentSummary()!.file_size)}
              </div>
            </Show>
            <Show when={entriesLoading()}>
              <div class="cl-empty">…</div>
            </Show>
            <div class="cl-stream">
              <For each={filteredEntries()}>{(e) => renderBubble(e)}</For>
              <Show
                when={
                  !entriesLoading() &&
                  filteredEntries().length === 0 &&
                  entries().length > 0
                }
              >
                <div class="cl-empty">no matches for filter</div>
              </Show>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}
