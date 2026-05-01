import { createEffect, createSignal, Show } from "solid-js";
import type { LayoutNode, SplitDirection } from "./types";

interface Props {
  pane: Extract<LayoutNode, { kind: "pane" }>;
  isActive: boolean;
  onFocus: (paneId: string) => void;
  onSplit: (paneId: string, direction: SplitDirection) => void;
  onClose: (paneId: string) => void;
  onNavigate: (paneId: string, url: string) => void;
  onGoBack: (paneId: string) => void;
  onGoHome: (paneId: string) => void;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
}

export function BrowserPane(p: Props) {
  const browser = () => p.pane.browser ?? { url: "", home_url: undefined, history: [] };
  const [urlDraft, setUrlDraft] = createSignal(browser().url);
  let iframeRef: HTMLIFrameElement | undefined;

  // Sync the input draft whenever the persisted URL changes (e.g. CLI nav, back, home).
  let lastUrl = browser().url;
  createEffect(() => {
    const u = browser().url;
    if (u !== lastUrl) {
      lastUrl = u;
      setUrlDraft(u);
    }
  });

  const submitUrl = () => {
    let v = urlDraft().trim();
    if (!v) return;
    // Auto-prepend https:// if no scheme and it isn't about:/file:/etc.
    if (!/^[a-z][a-z0-9+\-.]*:/i.test(v)) v = "https://" + v;
    p.onNavigate(p.pane.pane_id, v);
  };

  const reload = () => {
    if (iframeRef) {
      // Re-assigning src forces a reload even if the URL is unchanged.
      iframeRef.src = browser().url || "about:blank";
    }
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
          when={browser().url}
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
            src={browser().url}
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
            referrerpolicy="no-referrer-when-downgrade"
          />
        </Show>
      </div>
    </div>
  );
}
