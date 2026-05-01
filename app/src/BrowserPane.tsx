import { createEffect, createSignal, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { LayoutNode, SplitDirection } from "./types";

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  isActive: boolean;
  onFocus: (paneId: string) => void;
  onSplit: (paneId: string, direction: SplitDirection) => void;
  onClose: (paneId: string) => void;
  onNavigate: (paneId: string, url: string) => void;
  onGoBack: (paneId: string) => void;
  onGoHome: (paneId: string) => void;
  onSetForward: (paneId: string, forward: boolean) => void;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
}

export function BrowserPane(p: Props) {
  const browser = () =>
    p.pane.browser ?? {
      url: "",
      home_url: undefined,
      history: [] as string[],
      forward_localhost: true,
    };
  const forwardOn = () => browser().forward_localhost ?? true;
  const [urlDraft, setUrlDraft] = createSignal(browser().url);
  // Phase 8.B: the URL the iframe actually loads. Differs from browser().url
  // when localhost forwarding rewrites it to 127.0.0.1:<local_forward_port>.
  const [resolvedUrl, setResolvedUrl] = createSignal(browser().url);
  const [resolveErr, setResolveErr] = createSignal<string | null>(null);
  let iframeRef: HTMLIFrameElement | undefined;

  // Whenever the persisted URL changes (user nav, back, home, CLI), refresh
  // both the address-bar draft and the iframe's resolved src.
  let lastUrl = browser().url;
  let lastForward = forwardOn();
  createEffect(() => {
    const u = browser().url;
    const f = forwardOn();
    if (u !== lastUrl) {
      lastUrl = u;
      setUrlDraft(u);
    }
    if (u !== resolvedUrl() || f !== lastForward) {
      lastForward = f;
      setResolveErr(null);
      if (!u) {
        setResolvedUrl("");
        return;
      }
      invoke<string>("pane_browser_resolve_url", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        url: u,
      })
        .then((rewritten) => setResolvedUrl(rewritten))
        .catch((err) => {
          // If forwarding fails (e.g. no SSH session), fall back to the raw URL
          // and surface the error in the chrome.
          setResolvedUrl(u);
          setResolveErr(String(err));
        });
    }
  });

  const submitUrl = () => {
    let v = urlDraft().trim();
    if (!v) return;
    // Auto-prepend http:// for localhost (dev servers usually are http) and
    // https:// for everything else missing a scheme.
    if (!/^[a-z][a-z0-9+\-.]*:/i.test(v)) {
      const isLocal = /^(localhost|127\.0\.0\.1)(:|$|\/)/i.test(v);
      v = (isLocal ? "http://" : "https://") + v;
    }
    p.onNavigate(p.pane.pane_id, v);
  };

  const reload = () => {
    if (iframeRef) {
      iframeRef.src = resolvedUrl() || "about:blank";
    }
  };

  const isTunneled = () => {
    const u = browser().url;
    if (!u || !resolvedUrl()) return false;
    return resolvedUrl() !== u;
  };

  return (
    <div
      class={`pane browser-pane ${p.isActive ? "active" : ""}`}
      onMouseDown={() => p.onFocus(p.pane.pane_id)}
    >
      <div class="pane-header browser-header">
        <button
          class="pane-btn"
          title="Back"
          disabled={browser().history.length === 0}
          onClick={(e) => {
            e.stopPropagation();
            p.onGoBack(p.pane.pane_id);
          }}
        >
          ←
        </button>
        <button
          class="pane-btn"
          title="Reload"
          onClick={(e) => {
            e.stopPropagation();
            reload();
          }}
        >
          ↺
        </button>
        <button
          class="pane-btn"
          title="Home"
          disabled={!browser().home_url}
          onClick={(e) => {
            e.stopPropagation();
            p.onGoHome(p.pane.pane_id);
          }}
        >
          🏠
        </button>
        <input
          class="browser-url"
          spellcheck={false}
          placeholder="https://…"
          value={urlDraft()}
          onMouseDown={(e) => e.stopPropagation()}
          onInput={(e) => setUrlDraft(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submitUrl();
            }
          }}
        />
        <button
          class="pane-btn"
          title="Go"
          onClick={(e) => {
            e.stopPropagation();
            submitUrl();
          }}
        >
          →
        </button>
        <Show when={isTunneled()}>
          <span
            class="browser-tunnel-badge"
            title={`tunneled via SSH → ${resolvedUrl()}`}
          >
            🔗 tunneled
          </span>
        </Show>
        <Show when={resolveErr()}>
          <span class="browser-tunnel-err" title={resolveErr()!}>
            ⚠
          </span>
        </Show>
        <label
          class="browser-forward-toggle"
          title="Forward localhost via SSH (Phase 8.B)"
          onMouseDown={(e) => e.stopPropagation()}
        >
          <input
            type="checkbox"
            checked={forwardOn()}
            onChange={(e) =>
              p.onSetForward(p.pane.pane_id, e.currentTarget.checked)
            }
          />
          fwd
        </label>
        <Show when={p.pane.title}>
          <span class="pane-title browser-title" title={p.pane.title!}>
            · {p.pane.title}
          </span>
        </Show>
        <button
          class="pane-btn"
          title="Split right"
          onClick={(e) => {
            e.stopPropagation();
            p.onSplit(p.pane.pane_id, "horizontal");
          }}
        >
          ↔
        </button>
        <button
          class="pane-btn"
          title="Split down"
          onClick={(e) => {
            e.stopPropagation();
            p.onSplit(p.pane.pane_id, "vertical");
          }}
        >
          ↕
        </button>
        <button
          class="pane-btn pane-close"
          title="Close pane"
          onClick={(e) => {
            e.stopPropagation();
            p.onClose(p.pane.pane_id);
          }}
        >
          ×
        </button>
      </div>
      <div class="pane-body browser-body">
        <Show
          when={resolvedUrl()}
          fallback={
            <div class="browser-placeholder">
              <p>Enter a URL above to load a page.</p>
              <p class="browser-hint">
                Note: many sites (Google, banks, etc.) block iframe embedding via
                X-Frame-Options. WebView2 native panes will lift this in a later phase.
              </p>
            </div>
          }
        >
          <iframe
            ref={iframeRef!}
            class="browser-iframe"
            src={resolvedUrl()}
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
            referrerpolicy="no-referrer-when-downgrade"
          />
        </Show>
      </div>
    </div>
  );
}
