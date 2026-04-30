import { createSignal, onCleanup, onMount, Show } from "solid-js";
import type { LayoutNode } from "./types";
import { describeConnection } from "./types";
import type { TerminalInstance } from "./terminalInstance";

export type ConnectOpts = {
  password?: string;
  keyPassphrase?: string;
  acceptUnknownHost?: boolean;
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
  ensureTerm: (paneId: string) => TerminalInstance;
  onFocus: (paneId: string) => void;
  onConnect: (paneId: string, opts?: ConnectOpts) => void;
  onSplit: (paneId: string, direction: "horizontal" | "vertical") => void;
  onClose: (paneId: string) => void;
  onDisconnect: (paneId: string) => void;
}

export function PaneView(p: Props) {
  let slotRef!: HTMLDivElement;
  let ti: TerminalInstance | null = null;
  const [pwInput, setPwInput] = createSignal("");
  const [passInput, setPassInput] = createSignal("");

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
        <span class="pane-conn">{describeConnection(p.pane.connection)}</span>
        <Show when={p.statusText}>
          <span class="pane-status-text">{p.statusText}</span>
        </Show>
        <Show when={p.isConnected}>
          <button
            class="pane-btn"
            title="Disconnect"
            onClick={() => p.onDisconnect(p.pane.pane_id)}
          >
            ⏻
          </button>
        </Show>
        <button class="pane-btn" title="Split right (Ctrl+Shift+D)" onClick={() => p.onSplit(p.pane.pane_id, "horizontal")}>↔</button>
        <button class="pane-btn" title="Split down (Ctrl+Shift+E)" onClick={() => p.onSplit(p.pane.pane_id, "vertical")}>↕</button>
        <button class="pane-btn pane-close" title="Close pane (Ctrl+Shift+W)" onClick={() => p.onClose(p.pane.pane_id)}>×</button>
      </div>
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
              <button class="primary big" onClick={() => p.onConnect(p.pane.pane_id, {})}>
                Connect
              </button>
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
