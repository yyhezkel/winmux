import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { t } from "./i18n";
import type {
  ChatMessage,
  ClaudeChatState,
  LayoutNode,
  WorkspacesFile,
} from "./types";

// Phase 22.B: backend streaming events. delta is one chunk; the
// frontend appends locally so we don't trigger a full workspace
// re-render on every token.
type TokenEvent = {
  workspace_id: string;
  pane_id: string;
  message_id: string;
  delta: string;
  session_id?: string | null;
};
type DoneEvent = {
  workspace_id: string;
  pane_id: string;
  message_id: string;
  session_id?: string | null;
};
type ErrorEvent = {
  workspace_id: string;
  pane_id: string;
  message_id: string;
  error: string;
};

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  isActive: boolean;
  onFocus: (paneId: string) => void;
  onClose: (paneId: string) => void;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
  // Phase 22.A: the pane state is persisted via workspace_split / chat-send,
  // and the parent updates `file` so re-rendering picks up new messages.
  // ClaudeChatPane reads them off `p.pane.chat`.
  onFileUpdate: (file: WorkspacesFile) => void;
}

const DEFAULT_STATE: ClaudeChatState = { messages: [] };

function fmtTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

export function ClaudeChatPane(p: Props) {
  const persistedChat = createMemo<ClaudeChatState>(
    () => p.pane.chat ?? DEFAULT_STATE
  );
  // Phase 22.B: in-flight token deltas overlay the persisted state.
  // Map of message_id → { extraContent, status }. When a token event
  // fires we accumulate the delta here; when claude:chat:done fires
  // we leave it in place until the next workspaces:changed flush
  // brings in the persisted content (which already includes all the
  // deltas, so the override becomes a no-op).
  const [overrides, setOverrides] = createSignal<
    Record<string, { extra: string; status: "sending" | "done" | "error" }>
  >({});
  const chat = createMemo<ClaudeChatState>(() => {
    const base = persistedChat();
    const ov = overrides();
    // Apply overrides on top of persisted messages.
    const merged = base.messages.map((m) => {
      const o = ov[m.id];
      if (!o) return m;
      return {
        ...m,
        content: m.content + o.extra,
        status: o.status,
      };
    });
    return { ...base, messages: merged };
  });
  const [draft, setDraft] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  let messagesRef: HTMLDivElement | undefined;
  let inputRef: HTMLTextAreaElement | undefined;

  // Autoscroll to bottom whenever the message list grows or content streams in.
  createEffect(() => {
    const count = chat().messages.length;
    const lastContent =
      chat().messages.length > 0
        ? chat().messages[chat().messages.length - 1].content
        : "";
    void count;
    void lastContent;
    queueMicrotask(() => {
      if (messagesRef) messagesRef.scrollTop = messagesRef.scrollHeight;
    });
  });

  // Phase 22.B: subscribe to streaming events from the backend.
  const unlistens: UnlistenFn[] = [];
  onMount(async () => {
    unlistens.push(
      await listen<TokenEvent>("claude:chat:token", (e) => {
        const ev = e.payload;
        if (ev.pane_id !== p.pane.pane_id) return;
        setOverrides((prev) => {
          const cur = prev[ev.message_id] ?? { extra: "", status: "sending" };
          return {
            ...prev,
            [ev.message_id]: {
              extra: cur.extra + ev.delta,
              status: "sending",
            },
          };
        });
      })
    );
    unlistens.push(
      await listen<DoneEvent>("claude:chat:done", (e) => {
        const ev = e.payload;
        if (ev.pane_id !== p.pane.pane_id) return;
        // Mark complete and let the workspaces:changed re-sync drop the override.
        setOverrides((prev) => {
          const cur = prev[ev.message_id];
          if (!cur) return prev;
          return {
            ...prev,
            [ev.message_id]: { ...cur, status: "done" },
          };
        });
        setSending(false);
      })
    );
    unlistens.push(
      await listen<ErrorEvent>("claude:chat:error", (e) => {
        const ev = e.payload;
        if (ev.pane_id !== p.pane.pane_id) return;
        setOverrides((prev) => {
          const cur = prev[ev.message_id] ?? { extra: "", status: "error" };
          return {
            ...prev,
            [ev.message_id]: { ...cur, status: "error" },
          };
        });
        setErr(ev.error);
        setSending(false);
      })
    );
  });
  onCleanup(() => {
    for (const u of unlistens) {
      try {
        u();
      } catch {
        /* ignore */
      }
    }
  });

  // Drop stale overrides whenever the persisted state changes — the
  // backend already merged the deltas into msg.content before
  // emitting workspaces:changed, so the override becomes redundant
  // and would otherwise double-render.
  createEffect(() => {
    const persisted = persistedChat().messages;
    setOverrides((prev) => {
      const next: Record<string, { extra: string; status: "sending" | "done" | "error" }> = {};
      for (const [id, ov] of Object.entries(prev)) {
        const msg = persisted.find((m) => m.id === id);
        if (!msg) {
          next[id] = ov;
          continue;
        }
        // If the persisted content already contains the override
        // delta (i.e. the backend finalized), drop it.
        if (msg.status !== "sending") continue;
        next[id] = ov;
      }
      return next;
    });
  });

  const autoGrow = () => {
    if (!inputRef) return;
    inputRef.style.height = "auto";
    inputRef.style.height = Math.min(inputRef.scrollHeight, 160) + "px";
  };

  const submit = async () => {
    const text = draft().trim();
    if (!text || sending()) return;
    setSending(true);
    setErr(null);
    try {
      const f = await invoke<WorkspacesFile>("claude_chat_send", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        content: text,
      });
      p.onFileUpdate(f);
      setDraft("");
      if (inputRef) {
        inputRef.value = "";
        autoGrow();
      }
    } catch (e) {
      setErr(String(e));
    } finally {
      setSending(false);
    }
  };

  const clearChat = async () => {
    try {
      const f = await invoke<WorkspacesFile>("claude_chat_clear", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        dropSessionId: false,
      });
      p.onFileUpdate(f);
    } catch (e) {
      console.error("chat clear failed", e);
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void submit();
    }
  };

  const modelLabel = () => {
    const m = chat().model;
    if (m && m.length > 0) return m;
    return "Claude (default)";
  };

  const sessionHint = () => {
    const sid = chat().session_id;
    if (sid) return `· session ${sid.slice(0, 8)}`;
    return sending() ? `· streaming…` : `· new session`;
  };

  return (
    <div
      class={`pane ${p.isActive ? "active" : ""}`}
      onClick={() => p.onFocus(p.pane.pane_id)}
    >
      <div class="pane-header">
        <span class="pane-conn">{t("chat.header_prefix")}</span>
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
        <div class="chat-pane">
          <div class="chat-header">
            <div class="chat-header-left">
              <span class="chat-model-badge">{modelLabel()}</span>
              <span class="chat-status-pill">{sessionHint()}</span>
            </div>
            <div class="chat-header-actions">
              <Show when={chat().messages.length > 0}>
                <button onClick={clearChat}>{t("chat.clear")}</button>
              </Show>
            </div>
          </div>

          <div class="chat-messages" ref={(el) => (messagesRef = el)}>
            <Show
              when={chat().messages.length > 0}
              fallback={
                <div class="chat-empty">
                  <h3>{t("chat.empty.title")}</h3>
                  <p>{t("chat.empty.subtitle")}</p>
                </div>
              }
            >
              <For each={chat().messages}>{(m) => <Bubble msg={m} />}</For>
            </Show>
            <Show when={err()}>
              <div class="chat-row system">
                <div class="chat-bubble error">{err()}</div>
              </div>
            </Show>
          </div>

          <div class="chat-input-bar">
            <div class="chat-input-pill">
              <button
                class="chat-attach-btn"
                title={t("chat.attach_button")}
                onClick={(e) => {
                  e.stopPropagation();
                  // Phase 22.C placeholder — file attach UI lands later.
                }}
              >
                +
              </button>
              <textarea
                ref={(el) => (inputRef = el)}
                class="chat-input"
                rows="1"
                dir="auto"
                placeholder={t("chat.placeholder")}
                disabled={sending()}
                onInput={(e) => {
                  setDraft(e.currentTarget.value);
                  autoGrow();
                }}
                onKeyDown={onKeyDown}
              />
              <button
                class="chat-send-btn"
                title={t("chat.send_button")}
                disabled={sending() || draft().trim().length === 0}
                onClick={(e) => {
                  e.stopPropagation();
                  void submit();
                }}
              >
                ↑
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function Bubble(props: { msg: ChatMessage }) {
  const role = () => props.msg.role;
  const status = () => props.msg.status ?? "done";
  return (
    <div class={`chat-row ${role()}`}>
      <div
        class={`chat-bubble ${role()} ${status() === "sending" ? "sending" : ""} ${
          status() === "error" ? "error" : ""
        }`}
        dir="auto"
      >
        {props.msg.content}
        <Show when={role() !== "system" && props.msg.timestamp}>
          <span class="chat-bubble-time">{fmtTime(props.msg.timestamp)}</span>
        </Show>
      </div>
    </div>
  );
}
