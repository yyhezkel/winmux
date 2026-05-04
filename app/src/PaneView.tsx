import { createSignal, onCleanup, onMount, Show } from "solid-js";
import type { LayoutNode } from "./types";
import { describeConnection } from "./types";
import type { TerminalInstance } from "./terminalInstance";
import { t } from "./i18n";

export type ConnectOpts = {
  password?: string;
  keyPassphrase?: string;
  acceptUnknownHost?: boolean;
  // Phase 11.A: when true the SSH shell is wrapped in `tmux new-session -A`
  // so a reconnect attaches to the same session.
  persistent?: boolean;
};

export type PassphrasePending = { paneId: string; keyPath: string; bad?: boolean };
export type HostTrustPending = {
  paneId: string;
  target: string;
  keyType: string;
  fingerprint: string;
  mismatchOld?: string;
};

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
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
  const isSsh = () => p.pane.connection?.type === "ssh";
  const isTmux = () => !!p.tmuxSession;
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
                <button class="primary big" onClick={() => p.onConnect(p.pane.pane_id, {})}>
                  {t("common.connect")}
                </button>
                <Show when={isSsh()}>
                  <button
                    class="big connect-tmux"
                    title={t("pane.connect.persistent_hint")}
                    onClick={() => p.onConnect(p.pane.pane_id, { persistent: true })}
                  >
                    {t("common.connect_tmux")}
                  </button>
                </Show>
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
    </div>
  );
}
