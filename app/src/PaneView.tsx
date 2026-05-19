import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { Connection, LayoutNode } from "./types";
import { describeConnection } from "./types";
import type { TerminalInstance } from "./terminalInstance";
import { t } from "./i18n";

interface ClaudeSessionInfo {
  session_id: string;
  project_path: string;
  jsonl_path: string;
  mtime_unix: number;
  last_user?: string | null;
  last_assistant?: string | null;
}

export type ConnectOpts = {
  password?: string;
  keyPassphrase?: string;
  acceptUnknownHost?: boolean;
  persistent?: boolean;
  mode?: "default" | "tmux" | "plain" | "cmd" | "claude";
  cwdOverride?: string;
  cmd?: string;
  claudeArgs?: string;
  // Phase 23.F: override the auto-derived tmux session name (picker).
  tmuxSession?: string;
};

export type { TmuxSessionInfo } from "./types";

export type PassphrasePending = { paneId: string; keyPath: string; bad?: boolean };
export type HostTrustPending = {
  paneId: string;
  target: string;
  keyType: string;
  fingerprint: string;
  mismatchOld?: string;
};

interface ClaudePickerProps {
  workspaceId: string;
  onClose: () => void;
  onPick: (sessionId: string) => void;
}

function ClaudeSessionPicker(p: ClaudePickerProps) {
  const [items, setItems] = createSignal<ClaudeSessionInfo[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [err, setErr] = createSignal<string | null>(null);
  onMount(async () => {
    try {
      const list = await invoke<ClaudeSessionInfo[]>("pane_list_claude_sessions", {
        workspaceId: p.workspaceId,
        limit: 30,
      });
      setItems(list);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  });
  const fmtAge = (mt: number) => {
    if (!mt) return "—";
    const sec = Math.max(1, Math.floor(Date.now() / 1000 - mt));
    if (sec < 60) return `${sec}s`;
    if (sec < 3600) return `${Math.floor(sec / 60)}m`;
    if (sec < 86400) return `${Math.floor(sec / 3600)}h`;
    return `${Math.floor(sec / 86400)}d`;
  };
  return (
    <div class="modal-backdrop" onClick={p.onClose}>
      <div class="modal claude-picker" onClick={(e) => e.stopPropagation()}>
        <div class="settings-head">
          <h3>{t("claude_picker.title")}</h3>
          <button class="feed-x" title={t("common.close")} onClick={p.onClose}>×</button>
        </div>
        <div class="claude-picker-body">
          <Show when={loading()}><p class="status-line">{t("claude_picker.loading")}</p></Show>
          <Show when={err()}><p class="status-line err">{err()}</p></Show>
          <Show when={!loading() && !err() && items().length === 0}>
            <p class="status-line">{t("claude_picker.empty")}</p>
          </Show>
          <Show when={items().length > 0}>
            <ul class="claude-list">
              {items().map((it) => (
                <li
                  class="claude-row"
                  onClick={() => p.onPick(it.session_id)}
                  title={it.jsonl_path}
                >
                  <div class="claude-row-head">
                    <code class="claude-id">{it.session_id.slice(0, 8)}</code>
                    <span class="claude-proj">{it.project_path}</span>
                    <span class="claude-age">{fmtAge(it.mtime_unix)}</span>
                  </div>
                  <Show when={it.last_user}>
                    <div class="claude-prev"><b>{t("claude_picker.user_prefix")}</b> {it.last_user}</div>
                  </Show>
                  <Show when={it.last_assistant}>
                    <div class="claude-prev"><b>{t("claude_picker.assistant_prefix")}</b> {it.last_assistant}</div>
                  </Show>
                </li>
              ))}
            </ul>
          </Show>
        </div>
      </div>
    </div>
  );
}

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  // Phase 23.D: the workspace's canonical connection. `isSsh()` falls
  // back to this when the pane itself has no `connection` (FileManager
  // / Browser / ClaudeChat panes, or a fresh Terminal pane). Threaded
  // from App.tsx via LayoutView.
  workspaceConnection?: Connection;
  isActive: boolean;
  isConnected: boolean;
  pendingPasswordFor: string | null;
  pendingPassphrase: PassphrasePending | null;
  pendingHostTrust: HostTrustPending | null;
  status: { msg: string; err: boolean } | undefined;
  statusText?: string;
  // Phase 11.A: when this pane is bound to a tmux session, the name. Used
  // to render the "T" badge and to enable "Kill session" in the menu.
  tmuxSession?: string | null;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
  ensureTerm: (paneId: string) => TerminalInstance;
  onFocus: (paneId: string) => void;
  onConnect: (paneId: string, opts?: ConnectOpts) => void;
  onSplit: (paneId: string, direction: "horizontal" | "vertical") => void;
  onClose: (paneId: string) => void;
  onDisconnect: (paneId: string) => void;
  // Phase 11.A: hard-kill the remote tmux session. No-op for plain panes.
  onKillSession: (paneId: string) => void;
}

export function PaneView(p: Props) {
  let slotRef!: HTMLDivElement;
  let ti: TerminalInstance | null = null;
  const [pwInput, setPwInput] = createSignal("");
  const [passInput, setPassInput] = createSignal("");
  // Phase 7.A: edit mode for title/annotation.
  const [editingMeta, setEditingMeta] = createSignal(false);
  const [titleDraft, setTitleDraft] = createSignal("");
  const [annotDraft, setAnnotDraft] = createSignal("");
  const [showAnnot, setShowAnnot] = createSignal(false);
  // Phase 11.A: dropdown next to the disconnect button.
  const [showDiscMenu, setShowDiscMenu] = createSignal(false);
  // Phase 23.D: workspace dictates connection type. Check pane's own
  // connection first (set on wired Terminal panes), then fall back to
  // the workspace's canonical connection so SSH-only menu items
  // (tmux, claude --resume…) show up from FM / Browser / Chat panes too.
  const isSsh = () =>
    (p.pane.connection?.type ?? p.workspaceConnection?.type) === "ssh";
  const isTmux = () => !!p.tmuxSession;
  // Phase 12.B Smart Connect dropdown + extra modals.
  const [showConnectMenu, setShowConnectMenu] = createSignal(false);
  const [smartModal, setSmartModal] = createSignal<null | "cwd" | "cmd" | "claude_args">(null);
  const [smartInput, setSmartInput] = createSignal("");
  const [showClaudePicker, setShowClaudePicker] = createSignal(false);
  // Phase 23.F: tmux session picker state.
  const [tmuxSessions, setTmuxSessions] = createSignal<import("./types").TmuxSessionInfo[] | null>(null);
  const [tmuxPickerLoading, setTmuxPickerLoading] = createSignal(false);
  const [tmuxPickerErr, setTmuxPickerErr] = createSignal<string | null>(null);
  const openTmuxPicker = async () => {
    setTmuxSessions([]);
    setTmuxPickerLoading(true);
    setTmuxPickerErr(null);
    try {
      const list = await invoke<import("./types").TmuxSessionInfo[]>("pane_list_tmux_sessions", {
        workspaceId: p.workspaceId,
      });
      setTmuxSessions(list);
    } catch (e) {
      setTmuxPickerErr(String(e));
      setTmuxSessions([]);
    } finally {
      setTmuxPickerLoading(false);
    }
  };
  const closeTmuxPicker = () => {
    setTmuxSessions(null);
    setTmuxPickerErr(null);
    setRenameErrors({});
  };
  // Phase 23.G: per-row rename error map (session_name → message).
  // Cleared on each successful rename / picker close.
  const [renameErrors, setRenameErrors] = createSignal<Record<string, string>>({});
  const renameTmuxSession = async (oldName: string) => {
    const next = window.prompt(
      t("tmux_picker.rename_prompt", { name: oldName }),
      oldName,
    );
    if (!next || next === oldName) return;
    if (/[\s.:]/.test(next)) {
      setRenameErrors((prev) => ({ ...prev, [oldName]: t("tmux_picker.invalid_name") }));
      return;
    }
    try {
      await invoke("tmux_rename_session", {
        workspaceId: p.workspaceId,
        oldName,
        newName: next,
      });
      setRenameErrors((prev) => {
        const { [oldName]: _drop, ...rest } = prev;
        return rest;
      });
      // Re-fetch so the row reflects the new name.
      await openTmuxPicker();
    } catch (err) {
      setRenameErrors((prev) => ({ ...prev, [oldName]: String(err) }));
    }
  };
  const closeConnectMenu = () => setShowConnectMenu(false);
  const submitSmartModal = () => {
    const m = smartModal();
    const v = smartInput();
    setSmartModal(null);
    setSmartInput("");
    if (m === "cwd") p.onConnect(p.pane.pane_id, { cwdOverride: v });
    if (m === "cmd") p.onConnect(p.pane.pane_id, { mode: "cmd", cmd: v });
    if (m === "claude_args") p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: v });
  };
  const openMeta = () => {
    setTitleDraft(p.pane.title ?? "");
    setAnnotDraft(p.pane.annotation ?? "");
    setEditingMeta(true);
  };
  const saveMeta = () => {
    const newTitle = titleDraft();
    const newAnnot = annotDraft();
    if ((p.pane.title ?? "") !== newTitle)
      p.onSetTitle(p.pane.pane_id, newTitle);
    if ((p.pane.annotation ?? "") !== newAnnot)
      p.onSetAnnotation(p.pane.pane_id, newAnnot);
    setEditingMeta(false);
  };

  onMount(() => {
    ti = p.ensureTerm(p.pane.pane_id);
    if (ti.container.parentElement !== slotRef) {
      slotRef.appendChild(ti.container);
    }
    ti.container.style.display = "block";
    requestAnimationFrame(() => ti?.fitAndResize());
  });

  onCleanup(() => {
    if (ti && ti.container.parentElement === slotRef) {
      ti.container.parentElement.removeChild(ti.container);
    }
  });

  const passphraseHere = () =>
    p.pendingPassphrase && p.pendingPassphrase.paneId === p.pane.pane_id
      ? p.pendingPassphrase
      : null;

  const hostTrustHere = () =>
    p.pendingHostTrust && p.pendingHostTrust.paneId === p.pane.pane_id
      ? p.pendingHostTrust
      : null;

  return (
    <div
      class={`pane ${p.isActive ? "active" : ""}`}
      onMouseDown={() => p.onFocus(p.pane.pane_id)}
    >
      <div class="pane-header">
        <span class="pane-conn">
          {p.pane.connection ? describeConnection(p.pane.connection) : "—"}
        </span>
        <Show when={p.pane.title}>
          <span class="pane-title" title={p.pane.title!}>· {p.pane.title}</span>
        </Show>
        <Show when={p.pane.annotation}>
          <button
            class="pane-btn"
            title="Show annotation"
            onClick={(e) => {
              e.stopPropagation();
              setShowAnnot(!showAnnot());
            }}
          >
            ⓘ
          </button>
        </Show>
        <Show when={p.statusText}>
          <span class="pane-status-text">{p.statusText}</span>
        </Show>
        <button
          class="pane-btn"
          title="Edit title / annotation"
          onClick={(e) => {
            e.stopPropagation();
            openMeta();
          }}
        >
          ✎
        </button>
        <Show when={isTmux()}>
          <span
            class="pane-tmux-badge"
            title={t("pane.tooltip.tmux_badge")}
          >
            T
          </span>
        </Show>
        <Show when={p.isConnected}>
          <div class="pane-disc-wrap">
            <button
              class="pane-btn"
              title={isTmux() ? t("pane.tooltip.detach") : t("pane.tooltip.disconnect")}
              onClick={() => p.onDisconnect(p.pane.pane_id)}
            >
              ⏻
            </button>
            <button
              class="pane-btn pane-disc-caret"
              title={t("pane.tooltip.kill_session")}
              onClick={(e) => {
                e.stopPropagation();
                setShowDiscMenu(!showDiscMenu());
              }}
            >
              ▾
            </button>
            <Show when={showDiscMenu()}>
              <div
                class="pane-disc-menu"
                onClick={(e) => {
                  e.stopPropagation();
                  setShowDiscMenu(false);
                }}
              >
                <button onClick={() => p.onDisconnect(p.pane.pane_id)}>
                  {isTmux() ? t("common.detach") : t("common.disconnect")}
                </button>
                <Show when={isTmux()}>
                  <button class="danger" onClick={() => p.onKillSession(p.pane.pane_id)}>
                    {t("common.kill_session")}
                  </button>
                </Show>
              </div>
            </Show>
          </div>
        </Show>
        <button class="pane-btn" title="Split right (Ctrl+Shift+D)" onClick={() => p.onSplit(p.pane.pane_id, "horizontal")}>↔</button>
        <button class="pane-btn" title="Split down (Ctrl+Shift+E)" onClick={() => p.onSplit(p.pane.pane_id, "vertical")}>↕</button>
        <button class="pane-btn pane-close" title="Close pane (Ctrl+Shift+W)" onClick={() => p.onClose(p.pane.pane_id)}>×</button>
      </div>
      <Show when={editingMeta()}>
        <div class="pane-meta-editor" onMouseDown={(e) => e.stopPropagation()}>
          <input
            class="pane-meta-title"
            placeholder="title (e.g. trying to find the X bug)"
            maxlength="200"
            value={titleDraft()}
            onInput={(e) => setTitleDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                saveMeta();
              } else if (e.key === "Escape") {
                setEditingMeta(false);
              }
            }}
          />
          <textarea
            class="pane-meta-annot"
            placeholder="annotation (longer free text — context, intent, links)"
            rows="3"
            value={annotDraft()}
            onInput={(e) => setAnnotDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                e.preventDefault();
                saveMeta();
              } else if (e.key === "Escape") {
                setEditingMeta(false);
              }
            }}
          />
          <div class="pane-meta-actions">
            <button class="primary" onClick={saveMeta}>
              Save
            </button>
            <button onClick={() => setEditingMeta(false)}>Cancel</button>
            <span class="pane-meta-hint">
              Enter to save title; Ctrl+Enter to save from annotation; Esc to cancel
            </span>
          </div>
        </div>
      </Show>
      <Show when={showAnnot() && p.pane.annotation}>
        <div class="pane-annotation-bar">{p.pane.annotation}</div>
      </Show>
      <div class="pane-body">
        <Show when={!p.isConnected}>
          <div class="pane-connect">
            {/* Host-trust dialog (unknown host or mismatch) — highest priority */}
            <Show when={hostTrustHere()}>
              <div class={`host-trust ${hostTrustHere()!.mismatchOld ? "danger" : ""}`}>
                <Show
                  when={hostTrustHere()!.mismatchOld}
                  fallback={
                    <h3>First connect to {hostTrustHere()!.target}</h3>
                  }
                >
                  <h3>⚠ HOST KEY CHANGED for {hostTrustHere()!.target}</h3>
                </Show>
                <Show when={hostTrustHere()!.mismatchOld}>
                  <p class="warn">
                    The server's host key is different from the one we trusted before.
                    This may indicate a man-in-the-middle attack — or the server was rekeyed.
                  </p>
                  <p>
                    <span class="label">Old fingerprint:</span>{" "}
                    <code>{hostTrustHere()!.mismatchOld}</code>
                  </p>
                </Show>
                <p>
                  <span class="label">{hostTrustHere()!.keyType} fingerprint:</span>{" "}
                  <code>{hostTrustHere()!.fingerprint}</code>
                </p>
                <div class="trust-buttons">
                  <button
                    class="primary"
                    onClick={() =>
                      p.onConnect(p.pane.pane_id, { acceptUnknownHost: true })
                    }
                  >
                    {hostTrustHere()!.mismatchOld ? "Replace and continue" : "Trust and continue"}
                  </button>
                  <button onClick={() => p.onConnect(p.pane.pane_id, {})}>Cancel</button>
                </div>
              </div>
            </Show>

            {/* Passphrase prompt for encrypted local key */}
            <Show when={!hostTrustHere() && passphraseHere()}>
              <div class="pw-row">
                <span class="pass-hint">
                  Passphrase for {passphraseHere()!.keyPath}
                  {passphraseHere()!.bad ? " (wrong, try again)" : ""}:
                </span>
                <input
                  type="password"
                  placeholder="key passphrase"
                  autofocus
                  value={passInput()}
                  onInput={(e) => setPassInput(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const v = passInput();
                      setPassInput("");
                      p.onConnect(p.pane.pane_id, { keyPassphrase: v });
                    }
                  }}
                />
                <button
                  class="primary"
                  onClick={() => {
                    const v = passInput();
                    setPassInput("");
                    p.onConnect(p.pane.pane_id, { keyPassphrase: v });
                  }}
                >
                  Connect
                </button>
              </div>
            </Show>

            {/* Password prompt (server auth) */}
            <Show when={!hostTrustHere() && !passphraseHere() && p.pendingPasswordFor === p.pane.pane_id}>
              <div class="pw-row">
                <input
                  type="password"
                  placeholder="password"
                  autofocus
                  value={pwInput()}
                  onInput={(e) => setPwInput(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const v = pwInput();
                      setPwInput("");
                      p.onConnect(p.pane.pane_id, { password: v });
                    }
                  }}
                />
                <button
                  class="primary"
                  onClick={() => {
                    const v = pwInput();
                    setPwInput("");
                    p.onConnect(p.pane.pane_id, { password: v });
                  }}
                >
                  Connect
                </button>
              </div>
            </Show>

            {/* Default Connect button when no special prompt */}
            <Show
              when={
                !hostTrustHere() &&
                !passphraseHere() &&
                p.pendingPasswordFor !== p.pane.pane_id
              }
            >
              <div class="connect-buttons">
                {/* Phase 12.B Smart Connect — split button. Main click =
                    plain Connect. Caret opens the menu with tmux / plain /
                    cwd / cmd / claude options. */}
                <div class="connect-split">
                  <button class="primary big" onClick={() => p.onConnect(p.pane.pane_id, {})}>
                    {t("common.connect")}
                  </button>
                  <button
                    class="primary big connect-caret"
                    title="More connect options"
                    onClick={(e) => {
                      e.stopPropagation();
                      setShowConnectMenu(!showConnectMenu());
                    }}
                  >
                    ▾
                  </button>
                  <Show when={showConnectMenu()}>
                    <div
                      class="connect-menu"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <Show when={isSsh()}>
                        {/* Phase 23.F: open picker instead of attaching
                             to the auto-named session. */}
                        <button onClick={() => { closeConnectMenu(); openTmuxPicker(); }}>
                          {t("common.connect_tmux")}
                        </button>
                      </Show>
                      <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "plain" }); }}>
                        Plain shell
                      </button>
                      <hr />
                      <button onClick={() => { closeConnectMenu(); setSmartInput(""); setSmartModal("cwd"); }}>
                        Open in directory…
                      </button>
                      <button onClick={() => { closeConnectMenu(); setSmartInput(""); setSmartModal("cmd"); }}>
                        Run command…
                      </button>
                      <hr />
                      <Show when={isSsh()}>
                        <div class="connect-menu-section">Run Claude Code:</div>
                        <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "claude" }); }}>
                          claude
                        </button>
                        <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: "--continue" }); }}>
                          claude --continue
                        </button>
                        <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: "--resume" }); }}>
                          claude --resume
                        </button>
                        <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: "--dangerously-skip-permissions" }); }}>
                          claude --dangerously-skip-permissions
                        </button>
                        <button onClick={() => { closeConnectMenu(); setShowClaudePicker(true); }}>
                          Resume from list…
                        </button>
                      </Show>
                    </div>
                  </Show>
                </div>
              </div>
            </Show>

            <Show when={p.status}>
              <p class={p.status!.err ? "status-line err" : "status-line"}>
                {p.status!.msg}
              </p>
            </Show>
          </div>
        </Show>
        <div ref={slotRef!} class="pane-terminal-slot" />
      </div>

      {/* Phase 12.B: smart-connect prompt for cwd / cmd / claude args */}
      <Show when={smartModal()}>
        <div class="modal-backdrop" onClick={() => setSmartModal(null)}>
          <div class="modal smart-prompt" onClick={(e) => e.stopPropagation()} onMouseDown={(e) => e.stopPropagation()}>
            <h3>
              {smartModal() === "cwd" && "Open in directory"}
              {smartModal() === "cmd" && "Run command"}
              {smartModal() === "claude_args" && "Claude args"}
            </h3>
            <input
              class="pane-meta-title"
              autofocus
              placeholder={
                smartModal() === "cwd"
                  ? "/home/yossi/projects/foo"
                  : smartModal() === "cmd"
                    ? "npm run dev"
                    : "--resume"
              }
              value={smartInput()}
              onInput={(e) => setSmartInput(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submitSmartModal();
                else if (e.key === "Escape") setSmartModal(null);
              }}
            />
            <div class="modal-buttons">
              <button onClick={() => setSmartModal(null)}>{t("common.cancel")}</button>
              <button class="primary" onClick={submitSmartModal}>{t("common.connect")}</button>
            </div>
          </div>
        </div>
      </Show>

      {/* Phase 12.B: Claude session browser */}
      <Show when={showClaudePicker()}>
        <ClaudeSessionPicker
          workspaceId={p.workspaceId}
          onClose={() => setShowClaudePicker(false)}
          onPick={(sessionId) => {
            setShowClaudePicker(false);
            p.onConnect(p.pane.pane_id, {
              mode: "claude",
              claudeArgs: `--resume ${sessionId}`,
            });
          }}
        />
      </Show>

      {/* Phase 23.F: tmux session picker. */}
      <Show when={tmuxSessions() !== null}>
        <div class="modal-backdrop" onClick={closeTmuxPicker}>
          <div class="modal claude-picker" onClick={(e) => e.stopPropagation()}>
            <div class="settings-head">
              <h3>{t("tmux_picker.title")}</h3>
              <button class="feed-x" title={t("common.close")} onClick={closeTmuxPicker}>×</button>
            </div>
            <div class="claude-picker-body">
              <Show when={tmuxPickerLoading()}>
                <p class="status-line">{t("tmux_picker.loading")}</p>
              </Show>
              <Show when={tmuxPickerErr()}>
                <p class="status-line err">⚠ {tmuxPickerErr()}</p>
              </Show>
              <ul class="claude-list">
                <li
                  class="claude-row"
                  onClick={() => {
                    closeTmuxPicker();
                    p.onConnect(p.pane.pane_id, { mode: "tmux" });
                  }}
                >
                  <div class="claude-row-head">
                    <code class="claude-id">🆕</code>
                    <span class="claude-proj"><b>{t("tmux_picker.new_session")}</b></span>
                    <span class="claude-age">{t("tmux_picker.pane_id_target")}</span>
                  </div>
                  <div class="claude-prev">{t("tmux_picker.new_session_hint")}</div>
                </li>
                {(tmuxSessions() ?? []).map((s) => {
                  const ageOf = (epoch: number) => {
                    if (!epoch) return "—";
                    const sec = Math.max(1, Math.floor(Date.now() / 1000 - epoch));
                    if (sec < 60) return `${sec}s`;
                    if (sec < 3600) return `${Math.floor(sec / 60)}m`;
                    if (sec < 86400) return `${Math.floor(sec / 3600)}h`;
                    return `${Math.floor(sec / 86400)}d`;
                  };
                  const winLabel = s.windows === 1
                    ? t("tmux_picker.window", { n: String(s.windows) })
                    : t("tmux_picker.windows", { n: String(s.windows) });
                  return (
                    <li
                      class="claude-row"
                      onClick={() => {
                        closeTmuxPicker();
                        p.onConnect(p.pane.pane_id, {
                          mode: "tmux",
                          tmuxSession: s.name,
                        });
                      }}
                      title={`Created ${ageOf(s.created)} ago${s.last_attached ? `, last attached ${ageOf(s.last_attached)} ago` : ""}`}
                    >
                      <div class="claude-row-head">
                        <code class="claude-id">{s.name.slice(0, 14)}</code>
                        <span class="claude-proj">
                          {winLabel}
                          {s.attached ? ` · ${t("tmux_picker.attached")}` : ""}
                        </span>
                        <span class="claude-age">{ageOf(s.last_attached || s.created)}</span>
                        {/* Phase 23.G: per-row rename. Stop propagation
                            so clicking the pencil doesn't also attach. */}
                        <button
                          class="tmux-rename-btn"
                          title={t("tmux_picker.rename")}
                          onClick={(e) => {
                            e.stopPropagation();
                            void renameTmuxSession(s.name);
                          }}
                        >
                          ✎
                        </button>
                      </div>
                      <Show when={renameErrors()[s.name]}>
                        <div class="status-line err">⚠ {renameErrors()[s.name]}</div>
                      </Show>
                    </li>
                  );
                })}
              </ul>
              <Show when={!tmuxPickerLoading() && (tmuxSessions()?.length ?? 0) === 0 && !tmuxPickerErr()}>
                <p class="status-line">{t("tmux_picker.empty")}</p>
              </Show>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
