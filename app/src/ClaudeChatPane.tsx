import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import type {
  ChatMessage,
  ClaudeChatState,
  LayoutNode,
  WorkspacesFile,
} from "./types";

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
  const chat = createMemo<ClaudeChatState>(() => p.pane.chat ?? DEFAULT_STATE);
  const [draft, setDraft] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  let messagesRef: HTMLDivElement | undefined;
  let inputRef: HTMLTextAreaElement | undefined;

  // Autoscroll to bottom whenever the message list grows.
  createEffect(() => {
    const count = chat().messages.length;
    void count;
    // queueMicrotask so the DOM has flushed before we scroll.
    queueMicrotask(() => {
      if (messagesRef) messagesRef.scrollTop = messagesRef.scrollHeight;
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
    // Phase 22.A: no real model yet — show a placeholder badge so the UI
    // looks "live". 22.B replaces this with whatever `claude` actually
    // ran, captured from the first stream event.
    if (m && m.length > 0) return m;
    return "Claude Sonnet 4.6";
  };

  const sessionHint = () => {
    const sid = chat().session_id;
    if (sid) return `· session ${sid.slice(0, 8)}`;
    // Stub indicator until 22.B lands.
    return `· echo stub`;
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
