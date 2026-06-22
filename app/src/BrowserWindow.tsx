import {
  createEffect,
  createSignal,
  For,
  Show,
} from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { Workspace } from "./types";
import { t } from "./i18n";
import {
  clampToViewport,
  makeWindowControls,
  ResizeHandles,
  type Geometry,
} from "./floatingWindow";

// Phase 53 → 60 → 62.C: workspace-level Browser floating window.
//
// Phase 62.C reframed the browser as what it actually is: a window onto
// the services running ON THE REMOTE SERVER, reached through the SSH
// tunnel. The free-form URL bar is gone. Instead the user picks one of
// the ports the remote port-watcher has detected, optionally types a
// path, and the window forwards that remote port on demand and points
// the native child Webview at http://127.0.0.1:<local_tunnel_port>/<path>
// (127.0.0.1, not localhost — see item F note in go()). External
// browsing is intentionally not offered here.
//
// The native child Webview (managed by `workspace_browser` on the Rust
// side) paints above the SLOT and only the slot — the chrome around it
// (header + port bar + bottom strip + resize handles) stays HTML and
// clickable. When no URL is loaded yet the Webview is hidden so the
// in-slot empty-state / hint shows through.

interface DetectedPort {
  remote_port: number;
  addr: string;
  family: string;
}
interface ForwardInfo {
  remote_port: number;
  local_port: number;
}

interface Props {
  open: boolean;
  /** The active workspace — its id keys the Webview + persistence. */
  workspace: Workspace | null;
  onClose: () => void;
  /** Lets the window re-call show() on modal-close transitions. */
  anyModalOpen: () => boolean;
  /** Remote ports detected on this workspace's server (live). */
  detectedPorts: DetectedPort[];
  /** Forwards already open for this workspace (remote→local mapping). */
  forwards: ForwardInfo[];
  /** Ensure the remote port-watcher is running + refresh the snapshot. */
  onEnsurePorts: (workspaceId: string) => void;
  /** Open (or reuse) a forward for a remote port; resolves to the local
   *  tunnel port the Webview should hit. */
  onStartForward: (remotePort: number) => Promise<number>;
}

const DEFAULT_GEOMETRY: Geometry = { x: 120, y: 80, w: 1100, h: 700 };
const MIN_W = 480;
const MIN_H = 320;
/** Header (32) + port bar (32). Must match the CSS heights. */
const CHROME_TOP_PX = 64;
/** Bottom strip that keeps the resize grip clear of the Webview. */
const CHROME_BOTTOM_PX = 16;
/** Horizontal inset so the native Webview clears the left/right resize
 *  handles (native content paints above HTML). Matches .fw-resize width. */
const CHROME_SIDE_PX = 6;

const GEOM_KEY = (id: string) => `winmux.workspace-browser-geometry.${id}`;
const PORT_KEY = (id: string) => `winmux.workspace-browser-port.${id}`;
const PATH_KEY = (id: string) => `winmux.workspace-browser-path.${id}`;

function loadGeometry(workspaceId: string): Geometry {
  // Phase 64 (N): clamp to the viewport (stored OR default) for small
  // screens — the window must stay fully on-screen.
  try {
    const raw = localStorage.getItem(GEOM_KEY(workspaceId));
    if (raw) {
      const parsed: unknown = JSON.parse(raw);
      if (
        parsed &&
        typeof parsed === "object" &&
        typeof (parsed as Geometry).x === "number" &&
        typeof (parsed as Geometry).y === "number" &&
        typeof (parsed as Geometry).w === "number" &&
        typeof (parsed as Geometry).h === "number"
      ) {
        return clampToViewport(parsed as Geometry, MIN_W, MIN_H);
      }
    }
  } catch {
    // Corrupt entry — fall through to default.
  }
  return clampToViewport(DEFAULT_GEOMETRY, MIN_W, MIN_H);
}

function saveGeometry(workspaceId: string, g: Geometry): void {
  try {
    localStorage.setItem(GEOM_KEY(workspaceId), JSON.stringify(g));
  } catch {
    // Quota or private mode — ignore.
  }
}

function loadPort(workspaceId: string): number | null {
  try {
    const raw = localStorage.getItem(PORT_KEY(workspaceId));
    if (!raw) return null;
    const n = Number(raw);
    return Number.isFinite(n) && n > 0 ? n : null;
  } catch {
    return null;
  }
}
function savePort(workspaceId: string, port: number): void {
  try {
    localStorage.setItem(PORT_KEY(workspaceId), String(port));
  } catch {
    // ignore
  }
}
function loadPath(workspaceId: string): string {
  try {
    return localStorage.getItem(PATH_KEY(workspaceId)) || "";
  } catch {
    return "";
  }
}
function savePath(workspaceId: string, path: string): void {
  try {
    localStorage.setItem(PATH_KEY(workspaceId), path);
  } catch {
    // ignore
  }
}

/** Leading-slash-normalize the path field. "" stays "" (root). */
function normalizePath(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return "";
  return trimmed.startsWith("/") ? trimmed : `/${trimmed}`;
}

/** A friendly label for a detected port — the port number, plus the
 *  bind address when it's not the loopback default. */
function portLabel(p: DetectedPort): string {
  const showAddr = p.addr && p.addr !== "127.0.0.1" && p.addr !== "localhost";
  return showAddr ? `${p.remote_port} · ${p.addr}` : `${p.remote_port}`;
}

/** The rect the native Webview should occupy = window minus chrome. */
function slotRect(g: Geometry): Geometry {
  return {
    x: g.x + CHROME_SIDE_PX,
    y: g.y + CHROME_TOP_PX,
    w: Math.max(1, g.w - 2 * CHROME_SIDE_PX),
    h: Math.max(1, g.h - CHROME_TOP_PX - CHROME_BOTTOM_PX),
  };
}

export function BrowserWindow(p: Props) {
  const [geom, setGeom] = createSignal<Geometry>(DEFAULT_GEOMETRY);
  const [selectedPort, setSelectedPort] = createSignal<number | null>(null);
  const [pathInput, setPathInput] = createSignal("");
  // The localhost tunnel URL currently loaded in the Webview. null ⇒
  // nothing navigated yet → Webview hidden, empty-state/hint shown.
  const [currentUrl, setCurrentUrl] = createSignal<string | null>(null);
  const [navError, setNavError] = createSignal<string | null>(null);
  // One-shot guard so the persisted "last port" auto-opens only once per
  // open session (not every time detectedPorts updates).
  let autoTried = false;
  // Phase 62.C (F.1): the URL last handed to the native webview. Lets the
  // show effect tell a geometry change (same url → reposition only) from
  // a real URL change (different port → must navigate; the backend
  // fast-path only repositions, it doesn't navigate).
  let lastShownUrl: string | null = null;

  // Track the workspace by ID (object identity churns on every file()
  // refresh). On a real workspace change: reload geometry + persisted
  // port/path, drop the loaded URL, and pull a fresh port snapshot.
  let lastWsId: string | null = null;
  createEffect(() => {
    const id = p.workspace?.id;
    if (!id || id === lastWsId) return;
    lastWsId = id;
    setGeom(loadGeometry(id));
    setSelectedPort(loadPort(id));
    setPathInput(loadPath(id));
    setCurrentUrl(null);
    setNavError(null);
    autoTried = false;
    lastShownUrl = null;
    if (p.open) p.onEnsurePorts(id);
  });

  // Rising edge of `open`: ensure the watcher + refresh the snapshot so
  // the dropdown is populated even when auto_port_forward is off.
  let wasOpenForPorts = false;
  createEffect(() => {
    const open = p.open;
    const id = p.workspace?.id;
    if (open && !wasOpenForPorts && id) {
      autoTried = false;
      p.onEnsurePorts(id);
    }
    wasOpenForPorts = open;
  });

  // Auto-open the persisted port once it actually shows up in the
  // detected list (reopen-to-last behavior).
  createEffect(() => {
    if (!p.open || p.anyModalOpen()) return;
    if (currentUrl() || autoTried) return;
    const port = selectedPort();
    if (port == null) return;
    if (!p.detectedPorts.some((d) => d.remote_port === port)) return;
    autoTried = true;
    void go();
  });

  // Spawn / show / reposition / navigate the native Webview — or hide it
  // when there's no URL yet (so the empty-state HTML shows through).
  createEffect(() => {
    if (!p.open) return;
    const id = p.workspace?.id;
    if (!id) return;
    if (p.anyModalOpen()) return;
    const url = currentUrl();
    if (!url) {
      void invoke("workspace_browser_hide", { workspaceId: id }).catch(() => {});
      return;
    }
    const s = slotRect(geom());
    // Phase 62.C (F.1): workspace_browser_show spawns (loading `url`) on
    // the first call, or repositions + shows an existing webview. Its
    // fast path does NOT navigate, so when the URL changed (a different
    // port) we navigate explicitly. A geometry change keeps the same url
    // → reposition only.
    const urlChanged = lastShownUrl !== null && lastShownUrl !== url;
    lastShownUrl = url;
    void invoke("workspace_browser_show", {
      workspaceId: id,
      url,
      x: s.x,
      y: s.y,
      w: s.w,
      h: s.h,
    })
      .then(() => {
        if (urlChanged) {
          return invoke("workspace_browser_navigate", { workspaceId: id, url });
        }
      })
      .catch((err) => console.error("workspace_browser_show failed", err));
  });

  // Hide the Webview when the window CLOSES (not only on unmount — the
  // component stays mounted in App.tsx; the inner <Show> just collapses).
  let wasOpen = false;
  createEffect(() => {
    const open = p.open;
    const id = p.workspace?.id;
    if (wasOpen && !open && id) {
      void invoke("workspace_browser_hide", { workspaceId: id }).catch(() => {});
    }
    wasOpen = open;
  });

  // Persist geometry whenever it changes (keyed write — cheap).
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
    closeGuardSelector: ".browser-window-x",
  });

  // Resolve the chosen remote port to a local tunnel port (reusing an
  // existing forward when present, else opening one) and point the
  // Webview at it.
  const go = async () => {
    const ws = p.workspace;
    if (!ws) return;
    const port = selectedPort();
    if (port == null) return;
    setNavError(null);
    let local = p.forwards.find((f) => f.remote_port === port)?.local_port;
    if (local == null) {
      try {
        local = await p.onStartForward(port);
      } catch (e) {
        setNavError(t("browser.ports.forwardFailed", { msg: String(e) }));
        return;
      }
    }
    const path = normalizePath(pathInput());
    savePort(ws.id, port);
    savePath(ws.id, pathInput());
    // Phase 62.A (item F): use 127.0.0.1, NOT localhost. The local
    // tunnel listener binds 127.0.0.1 (IPv4) only; on dual-stack
    // Windows `localhost` resolves to ::1 (IPv6) first, which both
    // fails to connect AND — when it falls back — makes the page ORIGIN
    // differ from the 127.0.0.1 origin the Ports window uses, tripping
    // the service's CORS / cookie checks. PortsWindow already learned
    // this; the in-app browser now matches it.
    setCurrentUrl(`http://127.0.0.1:${local}${path}`);
  };

  const refresh = () => {
    const id = p.workspace?.id;
    if (id) p.onEnsurePorts(id);
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
        {/* Phase 62.A (item A): close button last → inline-END corner
            (right in LTR, left in RTL). */}
        <div class="browser-window-header" onMouseDown={onDragStart}>
          <span class="browser-window-title">
            🌐{" "}
            {t("browser.window.title", { workspace: p.workspace!.name })}
          </span>
          <button
            class="browser-window-x"
            onClick={p.onClose}
            title={t("common.close")}
            aria-label={t("common.close")}
          >
            ×
          </button>
        </div>
        {/* Phase 62.C: port bar replaces the free URL bar. Refresh ·
            remote-port dropdown · path · Go. Height must stay in sync
            with CHROME_TOP_PX (header 32 + this 32 = 64). */}
        <div class="browser-window-portbar">
          <button
            class="bw-port-btn"
            title={t("browser.ports.refresh")}
            onClick={refresh}
          >
            ⟳
          </button>
          <span class="bw-port-server">{t("browser.ports.serverPrefix")}</span>
          <Show
            when={p.detectedPorts.length > 0}
            fallback={
              <span class="bw-port-none">{t("browser.ports.none.inline")}</span>
            }
          >
            <select
              class="bw-port-select"
              value={selectedPort() ?? ""}
              onChange={(e) => {
                const v = e.currentTarget.value;
                setSelectedPort(v === "" ? null : Number(v));
              }}
            >
              <Show when={selectedPort() == null}>
                <option value="">{t("browser.ports.choose")}</option>
              </Show>
              <For each={p.detectedPorts}>
                {(d) => <option value={d.remote_port}>{portLabel(d)}</option>}
              </For>
            </select>
          </Show>
          <span class="bw-port-sep">/</span>
          <input
            class="bw-port-path"
            type="text"
            value={pathInput()}
            placeholder={t("browser.ports.path.placeholder")}
            onInput={(e) => setPathInput(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void go();
              }
              // Keep keystrokes out of the global shortcut handler.
              e.stopPropagation();
            }}
          />
          <button
            class="bw-port-go"
            onClick={() => void go()}
            disabled={selectedPort() == null}
            title={t("browser.ports.go")}
          >
            {t("browser.ports.go")}
          </button>
        </div>
        <div class="browser-window-slot">
          {/* Empty-state / hint — visible only while the Webview is
              hidden (no URL loaded). */}
          <Show when={!currentUrl()}>
            <div class="browser-window-empty">
              <Show
                when={p.detectedPorts.length === 0}
                fallback={
                  <div class="bw-empty-state">
                    <div class="bw-empty-icon">🔌</div>
                    <p class="bw-empty-body">{t("browser.empty.pickHint")}</p>
                    <Show when={navError()}>
                      <p class="bw-empty-err">⚠ {navError()}</p>
                    </Show>
                  </div>
                }
              >
                <div class="bw-empty-state">
                  <div class="bw-empty-icon">🌐</div>
                  <h3 class="bw-empty-title">{t("browser.empty.title")}</h3>
                  <p class="bw-empty-body">{t("browser.empty.body")}</p>
                  <button class="bw-empty-refresh" onClick={refresh}>
                    ⟳ {t("browser.ports.refresh.label")}
                  </button>
                  <Show when={navError()}>
                    <p class="bw-empty-err">⚠ {navError()}</p>
                  </Show>
                </div>
              </Show>
            </div>
          </Show>
        </div>
        <div class="browser-window-bottom" />
        {/* Phase 62 (item 2): 4 edges + 4 corners. */}
        <ResizeHandles onStart={onResizeStart} />
      </div>
    </Show>
  );
}
