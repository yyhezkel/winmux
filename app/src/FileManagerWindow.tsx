import {
  createEffect,
  createSignal,
  Show,
} from "solid-js";
import type { Workspace } from "./types";
import { FileManagerPane } from "./FileManagerPane";
import { t } from "./i18n";
import {
  makeWindowControls,
  ResizeHandles,
  type Geometry,
} from "./floatingWindow";

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

  // Phase 60: workspace tracked by ID, not object identity. Reading
  // `p.workspace` directly made this effect re-run on EVERY file()
  // refresh (new object identities each persist) and snap the
  // geometry back to the stored value — the "window is fixed in
  // place" smoke-test report. Same fix as BrowserWindow.
  let lastWsId: string | null = null;
  createEffect(() => {
    const id = p.workspace?.id;
    if (!id || id === lastWsId) return;
    lastWsId = id;
    setGeom(loadGeometry(id));
  });

  // Persist whenever geometry changes.
  createEffect(() => {
    const id = p.workspace?.id;
    if (!id) return;
    saveGeometry(id, geom());
  });

  // Phase 62 (item 2): shared header-drag + 8-way resize.
  const { onDragStart, onResizeStart } = makeWindowControls({
    geom,
    setGeom,
    minW: MIN_W,
    minH: MIN_H,
    closeGuardSelector: ".fm-window-x",
  });

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
        {/* Phase 62 (item 3): the close button is the FIRST header child
            so it lands on the inline-start corner — left in LTR, right in
            RTL (the header flex follows the document `dir`). It sits on the
            opposite edge from the side a docked pane snaps to. */}
        <div class="fm-window-header" onMouseDown={onDragStart}>
          <button
            class="fm-window-x"
            onClick={p.onClose}
            title={t("common.close")}
            aria-label={t("common.close")}
          >
            ×
          </button>
          <span class="fm-window-title">
            🗂{" "}
            {t("files.window.title", { workspace: p.workspace!.name })}
          </span>
        </div>
        <div class="fm-window-body">
          <FileManagerPane
            workspaceId={p.workspace!.id}
            hasSsh={isSshWorkspace(p.workspace)}
            hasActiveSession={p.hasActiveSession}
          />
        </div>
        {/* Phase 62 (item 2): 4 edges + 4 corners. */}
        <ResizeHandles onStart={onResizeStart} />
      </div>
    </Show>
  );
}
