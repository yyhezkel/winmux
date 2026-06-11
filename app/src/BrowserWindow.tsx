import {
  createEffect,
  createSignal,
  Show,
} from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { Workspace } from "./types";
import { t } from "./i18n";

// Phase 53 (rebased) → Phase 60 smoke-test fixes: workspace-level
// Browser floating window.
//
// The component owns an HTML "shell" (header + URL bar + slot +
// resize grip) and a per-workspace geometry signal. The native child
// Webview (managed by `workspace_browser` on the Rust side) paints
// above the SLOT — and only the slot. Phase 60 root-caused the
// "can't close the browser" smoke-test bug to the Webview being
// positioned over the ENTIRE window rect, covering the header, the
// X button, and the resize grip with native content that eats every
// click. The slot rect now excludes the chrome:
//
//   ┌─────────────────────────────┐ ← y
//   │ header (drag + title + X)   │   CHROME_TOP_PX
//   │ URL bar                     │
//   ├─────────────────────────────┤
//   │                             │
//   │   native Webview lives here │   (the slot)
//   │                             │
//   ├─────────────────────────────┤
//   │ bottom strip      [grip] ↘  │   CHROME_BOTTOM_PX
//   └─────────────────────────────┘ ← y + h
//
// Close = HIDE, not destroy: page state survives reopen. The
// workspace's Webview is only destroyed by workspace_delete.

interface Props {
  open: boolean;
  /** The active workspace — its id keys the Webview + geometry storage. */
  workspace: Workspace | null;
  onClose: () => void;
  /** Lets the window re-call show() on modal-close transitions. */
  anyModalOpen: () => boolean;
}

type Geometry = { x: number; y: number; w: number; h: number };

const DEFAULT_GEOMETRY: Geometry = { x: 120, y: 80, w: 1100, h: 700 };
const MIN_W = 480;
const MIN_H = 320;
/** Header (32) + URL bar (32). Must match the CSS heights. */
const CHROME_TOP_PX = 64;
/** Bottom strip that hosts the resize grip, clear of the Webview. */
const CHROME_BOTTOM_PX = 16;
const STORAGE_KEY = (workspaceId: string) =>
  `winmux.workspace-browser-geometry.${workspaceId}`;
const URL_KEY = (workspaceId: string) =>
  `winmux.workspace-browser-url.${workspaceId}`;
// Phase 60: about:blank rendered as a white void (smoke-test bug
// 2b). First open now lands on a real page; afterwards the last
// navigated URL is restored per workspace.
const DEFAULT_URL = "https://www.google.com";

function loadGeometry(workspaceId: string): Geometry {
  try {
    const raw = localStorage.getItem(STORAGE_KEY(workspaceId));
    if (!raw) return DEFAULT_GEOMETRY;
    const parsed: unknown = JSON.parse(raw);
    if (
      parsed &&
      typeof parsed === "object" &&
      typeof (parsed as Geometry).x === "number" &&
      typeof (parsed as Geometry).y === "number" &&
      typeof (parsed as Geometry).w === "number" &&
      typeof (parsed as Geometry).h === "number"
    ) {
      const g = parsed as Geometry;
      return {
        x: Math.max(0, g.x),
        y: Math.max(0, g.y),
        w: Math.max(MIN_W, g.w),
        h: Math.max(MIN_H, g.h),
      };
    }
  } catch {
    // Corrupt entry — fall through to default.
  }
  return DEFAULT_GEOMETRY;
}

function saveGeometry(workspaceId: string, g: Geometry): void {
  try {
    localStorage.setItem(STORAGE_KEY(workspaceId), JSON.stringify(g));
  } catch {
    // Quota or private mode — ignore. Window still works; just no
    // persistence this session.
  }
}

function loadUrl(workspaceId: string): string {
  try {
    return localStorage.getItem(URL_KEY(workspaceId)) || DEFAULT_URL;
  } catch {
    return DEFAULT_URL;
  }
}

function saveUrl(workspaceId: string, url: string): void {
  try {
    localStorage.setItem(URL_KEY(workspaceId), url);
  } catch {
    // ignore
  }
}

/** The rect the native Webview should occupy, derived from the
 *  window geometry minus the chrome. */
function slotRect(g: Geometry): Geometry {
  return {
    x: g.x,
    y: g.y + CHROME_TOP_PX,
    w: g.w,
    h: Math.max(1, g.h - CHROME_TOP_PX - CHROME_BOTTOM_PX),
  };
}

export function BrowserWindow(p: Props) {
  const [geom, setGeom] = createSignal<Geometry>(DEFAULT_GEOMETRY);
  const [urlInput, setUrlInput] = createSignal("");

  // Phase 60: track the workspace by ID, not object identity. The
  // previous effect read `p.workspace` directly — every file()
  // refresh (persist, pane status, ratio commit) produces NEW
  // workspace objects, so the effect re-ran constantly and snapped
  // the geometry back to the stored value mid-drag ("the window is
  // stuck" smoke-test report).
  let lastWsId: string | null = null;
  createEffect(() => {
    const id = p.workspace?.id;
    if (!id || id === lastWsId) return;
    lastWsId = id;
    setGeom(loadGeometry(id));
    setUrlInput(loadUrl(id));
  });

  // Spawn / show / reposition. Fires when the window opens, the
  // geometry changes (drag/resize), or modals close. The backend's
  // workspace_browser_show spawns on first call and repositions +
  // shows on subsequent ones — page state survives hide/show.
  createEffect(() => {
    if (!p.open) return;
    const id = p.workspace?.id;
    if (!id) return;
    if (p.anyModalOpen()) return;
    const s = slotRect(geom());
    void invoke("workspace_browser_show", {
      workspaceId: id,
      url: loadUrl(id),
      x: s.x,
      y: s.y,
      w: s.w,
      h: s.h,
    }).catch((err) =>
      console.error("workspace_browser_show failed", err),
    );
  });

  // Phase 60: hide the Webview when the window CLOSES — not only on
  // component unmount. The original code put this in onCleanup(),
  // which never fires when the inner <Show> collapses (the component
  // itself stays mounted in App.tsx). Result: closing the shell left
  // the native Webview orphaned on screen, eating clicks — including
  // the FileManagerWindow underneath it ("FM is stuck" was THIS bug,
  // not an FM bug).
  let wasOpen = false;
  createEffect(() => {
    const open = p.open;
    const id = p.workspace?.id;
    if (wasOpen && !open && id) {
      void invoke("workspace_browser_hide", { workspaceId: id }).catch(
        () => {},
      );
    }
    wasOpen = open;
  });

  // Persist geometry whenever it changes (keyed write — cheap).
  createEffect(() => {
    const id = p.workspace?.id;
    if (!id) return;
    saveGeometry(id, geom());
  });

  const navigate = () => {
    const id = p.workspace?.id;
    if (!id) return;
    let url = urlInput().trim();
    if (!url) return;
    // Friendly default: no scheme → https.
    if (!/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(url)) {
      url = `https://${url}`;
      setUrlInput(url);
    }
    saveUrl(id, url);
    void invoke("workspace_browser_navigate", {
      workspaceId: id,
      url,
    }).catch((err) => console.error("workspace_browser_navigate failed", err));
  };

  // ── Drag (header) ────────────────────────────────────────────────
  let dragState: { startX: number; startY: number; origX: number; origY: number } | null = null;
  const onDragMouseMove = (e: MouseEvent) => {
    if (!dragState) return;
    setGeom((g) => ({
      ...g,
      x: Math.max(0, dragState!.origX + (e.clientX - dragState!.startX)),
      y: Math.max(0, dragState!.origY + (e.clientY - dragState!.startY)),
    }));
  };
  const onDragMouseUp = () => {
    dragState = null;
    window.removeEventListener("mousemove", onDragMouseMove);
    window.removeEventListener("mouseup", onDragMouseUp);
  };
  const onDragStart = (e: MouseEvent) => {
    // Don't drag when the user clicked the close button itself.
    if ((e.target as HTMLElement).closest(".browser-window-x")) return;
    e.preventDefault();
    const g = geom();
    dragState = {
      startX: e.clientX,
      startY: e.clientY,
      origX: g.x,
      origY: g.y,
    };
    window.addEventListener("mousemove", onDragMouseMove);
    window.addEventListener("mouseup", onDragMouseUp);
  };

  // ── Resize (bottom-right grip) ───────────────────────────────────
  let resizeState: { startX: number; startY: number; origW: number; origH: number } | null = null;
  const onResizeMouseMove = (e: MouseEvent) => {
    if (!resizeState) return;
    setGeom((g) => ({
      ...g,
      w: Math.max(MIN_W, resizeState!.origW + (e.clientX - resizeState!.startX)),
      h: Math.max(MIN_H, resizeState!.origH + (e.clientY - resizeState!.startY)),
    }));
  };
  const onResizeMouseUp = () => {
    resizeState = null;
    window.removeEventListener("mousemove", onResizeMouseMove);
    window.removeEventListener("mouseup", onResizeMouseUp);
  };
  const onResizeStart = (e: MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const g = geom();
    resizeState = {
      startX: e.clientX,
      startY: e.clientY,
      origW: g.w,
      origH: g.h,
    };
    window.addEventListener("mousemove", onResizeMouseMove);
    window.addEventListener("mouseup", onResizeMouseUp);
  };

  return (
    <Show when={p.open && p.workspace}>
      <div
        class="browser-window"
        style={{
          left: `${geom().x}px`,
          top: `${geom().y}px`,
          width: `${geom().w}px`,
          height: `${geom().h}px`,
        }}
      >
        <div class="browser-window-header" onMouseDown={onDragStart}>
          <span class="browser-window-title">
            🌐{" "}
            {t("browser.window.title", { workspace: p.workspace!.name })}
          </span>
          <button
            class="browser-window-x"
            onClick={p.onClose}
            title={t("common.close")}
          >
            ×
          </button>
        </div>
        {/* Phase 60: URL bar — part of the bug-2b fix (blank screen
            with no way to navigate anywhere). Enter or the ⏎ button
            navigates; the last URL persists per workspace. */}
        <div class="browser-window-urlbar">
          <input
            type="text"
            value={urlInput()}
            placeholder={t("browser.window.urlBar.placeholder")}
            onInput={(e) => setUrlInput(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                navigate();
              }
              // Keep keystrokes out of the global shortcut handler
              // (Ctrl+Enter would otherwise toggle pane-maximize).
              e.stopPropagation();
            }}
          />
          <button onClick={navigate} title={t("browser.window.urlBar.placeholder")}>
            ⏎
          </button>
        </div>
        <div class="browser-window-slot" />
        <div class="browser-window-bottom">
          <div
            class="browser-window-resize"
            onMouseDown={onResizeStart}
            title={t("browser.window.resize.tooltip")}
          />
        </div>
      </div>
    </Show>
  );
}
