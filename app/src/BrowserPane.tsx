import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
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

// Phase 8.D.1: BrowserPane no longer renders an <iframe>. The actual page is
// drawn by a native Tauri 2 child Webview (WebView2 on Windows) that overlays
// the placeholder div. We only OWN positioning/visibility/navigation here;
// the OS draws the pixels. Trade-off: we can't put anything ON TOP of the
// webview (it always paints last), but X-Frame-Options is no longer a thing
// and `eval` works on cross-origin pages.
export function BrowserPane(p: Props) {
  const browser = () => {
    const b = p.pane.browser;
    return {
      url: b?.url ?? "",
      home_url: b?.home_url,
      history: b?.history ?? [],
      forward_localhost: b?.forward_localhost ?? true,
    };
  };
  const forwardOn = () => browser().forward_localhost ?? true;
  const [urlDraft, setUrlDraft] = createSignal(browser().url);
  // Phase 8.B: rewritten URL the WEBVIEW actually navigates to. Differs from
  // browser().url when localhost forwarding rewrites it to 127.0.0.1:<local>.
  const [resolvedUrl, setResolvedUrl] = createSignal(browser().url);
  const [resolveErr, setResolveErr] = createSignal<string | null>(null);
  let mountRef: HTMLDivElement | undefined;

  // Sync the address bar with the persisted URL whenever it changes (CLI
  // navigate, back, home).
  let lastUrl = browser().url;
  let lastResolvedSource = "";
  let lastResolvedForward = forwardOn();
  createEffect(() => {
    const u = browser().url;
    const f = forwardOn();
    if (u !== lastUrl) {
      lastUrl = u;
      setUrlDraft(u);
    }
    if (u === lastResolvedSource && f === lastResolvedForward) {
      return;
    }
    lastResolvedSource = u;
    lastResolvedForward = f;
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
      .then((rewritten) => {
        setResolvedUrl(rewritten);
        ensureWebviewAt(rewritten);
      })
      .catch((err) => {
        setResolvedUrl(u);
        setResolveErr(String(err));
      });
  });

  const submitUrl = () => {
    let v = urlDraft().trim();
    if (!v) return;
    if (!/^[a-z][a-z0-9+\-.]*:/i.test(v)) {
      const isLocal = /^(localhost|127\.0\.0\.1)(:|$|\/)/i.test(v);
      v = (isLocal ? "http://" : "https://") + v;
    }
    p.onNavigate(p.pane.pane_id, v);
  };

  // Force a fresh resolve + push to the webview (bypassing the
  // lastResolvedSource cache). Used by the ↺ button and the
  // pane:browser:resolve-stale event after SSH comes up.
  // Ensure-or-navigate the native webview at the placeholder. Used after a
  // successful URL resolve. If no webview exists yet (resolve failed earlier
  // and we deferred creation), creates one. If it exists, repositions and
  // navigates to the new URL.
  const ensureWebviewAt = (url: string) => {
    const rect = computeRect();
    if (!rect) return;
    invoke("pane_browser_webview_ensure", {
      paneId: p.pane.pane_id,
      url,
      x: rect.x,
      y: rect.y,
      w: rect.w,
      h: rect.h,
    }).catch((err) => console.error("webview ensure failed", err));
  };

  const forceResolveAndReload = async () => {
    const u = browser().url;
    if (!u) return;
    setResolveErr(null);
    try {
      const rewritten = await invoke<string>("pane_browser_resolve_url", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        url: u,
      });
      lastResolvedSource = u;
      lastResolvedForward = forwardOn();
      setResolvedUrl(rewritten);
      ensureWebviewAt(rewritten);
    } catch (err) {
      setResolveErr(String(err));
      // Don't create the webview — leave the waiting overlay visible until
      // pane:browser:resolve-stale arrives (SSH up) and re-tries.
    }
  };

  const reload = () => {
    void forceResolveAndReload();
  };

  const isTunneled = () => {
    const u = browser().url;
    if (!u || !resolvedUrl()) return false;
    return resolvedUrl() !== u;
  };

  // Compute the placeholder's screen-relative geometry in logical pixels.
  // Tauri's add_child / set_position work in logical coordinates relative to
  // the window's content area, which matches getBoundingClientRect (the
  // browser pixel ratio is applied implicitly inside Webview2).
  const computeRect = (): { x: number; y: number; w: number; h: number } | null => {
    if (!mountRef) return null;
    const r = mountRef.getBoundingClientRect();
    return {
      x: Math.round(r.left),
      y: Math.round(r.top),
      w: Math.max(1, Math.round(r.width)),
      h: Math.max(1, Math.round(r.height)),
    };
  };

  // Debounce position updates so a fast resize/drag doesn't flood IPC.
  let positionTimer: number | null = null;
  const schedulePosition = () => {
    if (positionTimer) window.clearTimeout(positionTimer);
    positionTimer = window.setTimeout(() => {
      positionTimer = null;
      const rect = computeRect();
      if (!rect) return;
      invoke("pane_browser_webview_position", {
        paneId: p.pane.pane_id,
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
      }).catch(() => {});
    }, 30);
  };

  // Phase 8.B race fix: when SSH session comes up, re-resolve and re-navigate.
  onMount(() => {
    let cancelledStale = false;
    let unlistenStale: UnlistenFn | undefined;
    listen<{ workspace_id: string }>("pane:browser:resolve-stale", (e) => {
      if (cancelledStale) return;
      if (e.payload?.workspace_id !== p.workspaceId) return;
      void forceResolveAndReload();
    }).then((u) => {
      if (cancelledStale) {
        u();
      } else {
        unlistenStale = u;
      }
    });
    onCleanup(() => {
      cancelledStale = true;
      unlistenStale?.();
    });
  });

  // Phase 8.D.1: position-track the native webview through resize/scroll;
  // hide on unmount (workspace switch). Backend destroys on workspace_close_pane.
  // The webview itself is created LAZILY by forceResolveAndReload — only after
  // a successful resolve. While resolve fails (e.g. SSH session not up yet),
  // no webview exists, so the DOM "Waiting for SSH session…" overlay is visible
  // (WebView2 always paints on top of our DOM, so any DOM overlay would be
  // hidden behind a created webview).
  onMount(() => {
    requestAnimationFrame(() => {
      void forceResolveAndReload();
    });

    let ro: ResizeObserver | undefined;
    if (mountRef) {
      ro = new ResizeObserver(() => schedulePosition());
      ro.observe(mountRef);
    }
    // Window resize + scroll can move the placeholder too.
    const onWindow = () => schedulePosition();
    window.addEventListener("resize", onWindow);
    window.addEventListener("scroll", onWindow, true);
    onCleanup(() => {
      ro?.disconnect();
      window.removeEventListener("resize", onWindow);
      window.removeEventListener("scroll", onWindow, true);
      if (positionTimer) window.clearTimeout(positionTimer);
      // Hide (don't destroy) on unmount — workspace switch returns later
      // and we want to preserve page state. Backend destroys for real on
      // workspace_close_pane / workspace_delete.
      invoke("pane_browser_webview_hide", {
        paneId: p.pane.pane_id,
      }).catch(() => {});
    });
  });

  // Phase 8.C: agent-driven eval / screenshot. The eval path now runs in the
  // native webview where cross-origin restrictions don't apply (the webview
  // IS the origin). Result return is fire-and-forget for now (Webview::eval
  // doesn't return a value in Tauri 2.10 — full IPC bridge lands in 8.D.3).
  type BrowserRequest = {
    request_id: string;
    kind: "eval" | "screenshot";
    pane_id: string;
    expression?: string;
  };
  onMount(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    listen<BrowserRequest>("browser:request", async (e) => {
      if (cancelled) return;
      const r = e.payload;
      if (r.pane_id !== p.pane.pane_id) return;
      try {
        if (r.kind === "eval") {
          await invoke("pane_browser_webview_eval", {
            paneId: p.pane.pane_id,
            script: r.expression || "",
          });
          await invoke("pane_browser_response", {
            requestId: r.request_id,
            ok: {
              note: "eval submitted (fire-and-forget; CDP return-value bridge lands in Phase 8.D.3)",
            },
            err: null,
          });
        } else if (r.kind === "screenshot") {
          await invoke("pane_browser_response", {
            requestId: r.request_id,
            ok: null,
            err: "screenshot via native webview lands in Phase 8.D.3 (CDP page.captureScreenshot)",
          });
        }
      } catch (err) {
        await invoke("pane_browser_response", {
          requestId: r.request_id,
          ok: null,
          err: String(err),
        }).catch(() => {});
      }
    }).then((u) => {
      if (cancelled) {
        u();
      } else {
        unlisten = u;
      }
    });
    onCleanup(() => {
      cancelled = true;
      unlisten?.();
    });
  });

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
      {/* The webview overlays this placeholder. We keep it as the layout slot
          so Solid's flex-grow / split sizing computes the right geometry. */}
      <div ref={(el) => (mountRef = el)} class="pane-body browser-webview-mount">
        <Show when={resolveErr()?.includes("no active SSH session")}>
          <div class="browser-waiting">
            <p>Waiting for SSH session to come up…</p>
            <p class="browser-hint">
              Connect a terminal pane in this workspace to enable port forwarding,
              then press ↺ to retry.
            </p>
          </div>
        </Show>
      </div>
    </div>
  );
}
