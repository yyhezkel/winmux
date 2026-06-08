import {
  createEffect,
  createSignal,
  Show,
} from "solid-js";
import type { Workspace } from "./types";
import { FileManagerPane } from "./FileManagerPane";
import { t } from "./i18n";

// Phase 53 (rebased): workspace-level File Manager floating window.
//
// Wraps the existing FileManagerPane (dual-column local + remote SFTP)
// inside the same drag/resize chrome as BrowserWindow. Pure HTML —
// unlike Browser, there's no native child Webview involved, so the
// only persistence concern is geometry (per-workspace) in localStorage.

interface Props {
  open: boolean;
  workspace: Workspace | null;
  /** True if the workspace currently has any pane with an SSH session
   *  authenticated. Drives the right column's render state. */
  hasActiveSession: boolean;
  onClose: () => void;
}

type Geometry = { x: number; y: number; w: number; h: number };

const DEFAULT_GEOMETRY: Geometry = { x: 160, y: 100, w: 1100, h: 700 };
const MIN_W = 600;
const MIN_H = 380;
const STORAGE_KEY = (workspaceId: string) =>
  `winmux.workspace-files-geometry.${workspaceId}`;

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
    // Quota or private mode — ignore.
  }
}

function isSshWorkspace(w: Workspace | null): boolean {
  if (!w) return false;
  return w.connection?.type === "ssh";
}

export function FileManagerWindow(p: Props) {
  const [geom, setGeom] = createSignal<Geometry>(DEFAULT_GEOMETRY);

  // Workspace changed → load that workspace's saved geometry.
  createEffect(() => {
    const w = p.workspace;
    if (!w) return;
    setGeom(loadGeometry(w.id));
  });

  // Persist whenever geometry changes.
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
    if ((e.target as HTMLElement).closest(".fm-window-x")) return;
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
        class="fm-window"
        style={{
          left: `${geom().x}px`,
          top: `${geom().y}px`,
          width: `${geom().w}px`,
          height: `${geom().h}px`,
        }}
      >
        <div class="fm-window-header" onMouseDown={onDragStart}>
          <span class="fm-window-title">
            🗂{" "}
            {t("files.window.title", { workspace: p.workspace!.name })}
          </span>
          <button
            class="fm-window-x"
            onClick={p.onClose}
            title={t("common.close")}
          >
            ×
          </button>
        </div>
        <div class="fm-window-body">
          <FileManagerPane
            workspaceId={p.workspace!.id}
            hasSsh={isSshWorkspace(p.workspace)}
            hasActiveSession={p.hasActiveSession}
          />
        </div>
        <div
          class="fm-window-resize"
          onMouseDown={onResizeStart}
          title={t("files.window.resize.tooltip")}
        />
      </div>
    </Show>
  );
}
