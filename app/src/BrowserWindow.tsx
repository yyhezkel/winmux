import {
  createEffect,
  createSignal,
  onCleanup,
  Show,
} from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { Workspace } from "./types";
import { t } from "./i18n";

// Phase 53 (rebased): workspace-level Browser floating window.
//
// Owns an HTML "shell" (header + slot div + resize grip) plus a per-
// workspace geometry signal. The native child Webview (managed by
// `workspace_browser` on the Rust side) paints above the slot at the
// same logical rect. Geometry is hydrated from localStorage on mount
// and persisted on every change so the next open lands at the same
// size + position the user left it at.
//
// Z-order: native Webview always paints above HTML, so any modal
// opening in the SolidJS layer hides this Webview via App.tsx's
// `anyModalOpen` effect. On modal close, the show effect below re-
// fires with the current geometry.

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
const STORAGE_KEY = (workspaceId: string) =>
  `winmux.workspace-browser-geometry.${workspaceId}`;
const HOME_URL = "about:blank";

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

export function BrowserWindow(p: Props) {
  const [geom, setGeom] = createSignal<Geometry>(DEFAULT_GEOMETRY);

  // Workspace changed → load that workspace's saved geometry.
  createEffect(() => {
    const w = p.workspace;
    if (!w) return;
    setGeom(loadGeometry(w.id));
  });

  // Spawn / show effect. Fires when the window opens, when the
  // workspace changes, or when modals close (anyModalOpen→false).
  // The backend's workspace_browser_show spawns the Webview on first
  // call for the workspace and reposition+shows it on subsequent calls
  // — page state survives across hide/show cycles.
  createEffect(() => {
    if (!p.open) return;
    const w = p.workspace;
    if (!w) return;
    if (p.anyModalOpen()) return;
    const g = geom();
    void invoke("workspace_browser_show", {
      workspaceId: w.id,
      url: HOME_URL,
      x: g.x,
      y: g.y,
      w: g.w,
      h: g.h,
    }).catch((err) =>
      console.error("workspace_browser_show failed", err),
    );
  });

  // Hide (not close) on unmount so the next open restores page state.
  // workspace_delete is the only path that hard-closes the Webview.
  onCleanup(() => {
    const w = p.workspace;
    if (!w) return;
    void invoke("workspace_browser_hide", {
      workspaceId: w.id,
    }).catch(() => {});
  });

  // Persist geometry whenever it changes.
  createEffect(() => {
    const w = p.workspace;
    if (!w) return;
    saveGeometry(w.id, geom());
  });

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
        <div class="browser-window-slot" />
        <div
          class="browser-window-resize"
          onMouseDown={onResizeStart}
          title={t("browser.window.resize.tooltip")}
        />
      </div>
    </Show>
  );
}
