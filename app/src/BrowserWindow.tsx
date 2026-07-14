import {
  createEffect,
  createSignal,
  For,
  onCleanup,
  onMount,
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
import { IconGlobe, IconClose, IconRefresh, IconUnplug, IconWarning } from "./icons";

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
/** Header (32) + tab bar (26) + port bar (32). Must match the CSS
 *  heights. Beta.3: tab bar added between the header and the port bar. */
const CHROME_TOP_PX = 90;
/** Bottom strip that keeps the resize grip clear of the Webview. */
const CHROME_BOTTOM_PX = 16;
/** Horizontal inset so the native Webview clears the left/right resize
 *  handles (native content paints above HTML). Matches .fw-resize width. */
const CHROME_SIDE_PX = 6;

const GEOM_KEY = (id: string) => `winmux.workspace-browser-geometry.${id}`;
const PORT_KEY = (id: string) => `winmux.workspace-browser-port.${id}`;
const PATH_KEY = (id: string) => `winmux.workspace-browser-path.${id}`;
// Beta.3: per-workspace tabs. Each tab captures its own (port, path) —
// no free-form URL — matching the port-picker model of the port bar
// below. Legacy PORT_KEY / PATH_KEY are migrated into a single tab on
// first load, then this key takes over.
const TABS_KEY = (id: string) => `winmux.workspace-browser-tabs.${id}`;
const ACTIVE_TAB_KEY = (id: string) =>
  `winmux.workspace-browser-active-tab.${id}`;

/** One tab inside the workspace browser. `port`/`path` mirror what the
 *  port bar shows — nothing else is captured; the visible label is
 *  derived at render time from `port · path`. */
interface BrowserTab {
  id: string;
  port: number | null;
  path: string;
}

function newTabId(): string {
  const rand = Math.floor(Math.random() * 1e9).toString(36);
  return `tab-${Date.now().toString(36)}-${rand}`;
}

function loadTabs(
  workspaceId: string,
): { tabs: BrowserTab[]; activeId: string } {
  try {
    const raw = localStorage.getItem(TABS_KEY(workspaceId));
    if (raw) {
      const parsed: unknown = JSON.parse(raw);
      if (Array.isArray(parsed)) {
        const tabs: BrowserTab[] = parsed
          .filter(
            (t): t is BrowserTab =>
              typeof t === "object" &&
              t !== null &&
              typeof (t as { id?: unknown }).id === "string" &&
              typeof (t as { path?: unknown }).path === "string" &&
              (typeof (t as { port?: unknown }).port === "number" ||
                (t as { port?: unknown }).port === null),
          )
          .map((t) => ({ id: t.id, port: t.port, path: t.path }));
        if (tabs.length > 0) {
          const activeRaw = localStorage.getItem(ACTIVE_TAB_KEY(workspaceId));
          const activeId =
            activeRaw && tabs.some((t) => t.id === activeRaw)
              ? activeRaw
              : tabs[0]!.id;
          return { tabs, activeId };
        }
      }
    }
  } catch {
    // Corrupt entry — fall through to legacy migration.
  }
  // Legacy migration: build a single tab from the pre-tabs PORT_KEY /
  // PATH_KEY so users don't lose their last-used port/path on upgrade.
  const legacyPort = loadPort(workspaceId);
  const legacyPath = loadPath(workspaceId);
  const tab: BrowserTab = {
    id: newTabId(),
    port: legacyPort,
    path: legacyPath,
  };
  return { tabs: [tab], activeId: tab.id };
}

function saveTabs(
  workspaceId: string,
  tabs: BrowserTab[],
  activeId: string | null,
): void {
  try {
    localStorage.setItem(TABS_KEY(workspaceId), JSON.stringify(tabs));
    if (activeId) localStorage.setItem(ACTIVE_TAB_KEY(workspaceId), activeId);
  } catch {
    // quota / private mode — ignore
  }
}

/** Human-readable label for a tab: `port · path` or "New tab" if empty. */
function tabLabel(t: BrowserTab): string {
  if (t.port == null) return "New tab";
  return t.path ? `${t.port} · ${t.path}` : `${t.port}`;
}

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
  // Beta.3: per-workspace tab list. `selectedPort` / `pathInput` above
  // stay the LIVE editing signals; a `createEffect` below mirrors them
  // into the active tab so the persisted tab state matches what the
  // user just typed / picked. Tab switch goes in the other direction
  // (tab → editors), gated by `suppressActiveSync` to avoid feedback.
  const [tabs, setTabs] = createSignal<BrowserTab[]>([]);
  const [activeTabId, setActiveTabId] = createSignal<string | null>(null);
  let suppressActiveSync = false;
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
    // Beta.3: load tabs (migrating legacy PORT_KEY / PATH_KEY on first
    // run) and seed the port/path editors from the active tab.
    const { tabs: loaded, activeId } = loadTabs(id);
    const active = loaded.find((t) => t.id === activeId) ?? loaded[0]!;
    suppressActiveSync = true;
    setTabs(loaded);
    setActiveTabId(active.id);
    setSelectedPort(active.port);
    setPathInput(active.path);
    suppressActiveSync = false;
    // Persist so the legacy migration lands even if the user just
    // opens & closes the window without editing anything.
    saveTabs(id, loaded, active.id);
    setCurrentUrl(null);
    setNavError(null);
    autoTried = false;
    lastShownUrl = null;
    if (p.open) p.onEnsurePorts(id);
  });

  // Beta.3: mirror the live editor state (selectedPort / pathInput)
  // into the active tab and persist. `suppressActiveSync` lets a tab
  // switch / workspace load set the editors without a write-back.
  createEffect(() => {
    const port = selectedPort();
    const path = pathInput();
    const activeId = activeTabId();
    const wsId = p.workspace?.id;
    if (!activeId || !wsId) return;
    if (suppressActiveSync) return;
    setTabs((prev) => {
      let changed = false;
      const next = prev.map((t) => {
        if (t.id !== activeId) return t;
        if (t.port === port && t.path === path) return t;
        changed = true;
        return { ...t, port, path };
      });
      if (!changed) return prev;
      saveTabs(wsId, next, activeId);
      return next;
    });
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

  // ── Beta.3: tab operations ────────────────────────────────────────
  const switchTab = (id: string): void => {
    const t = tabs().find((x) => x.id === id);
    if (!t) return;
    const wsId = p.workspace?.id;
    suppressActiveSync = true;
    setActiveTabId(id);
    setSelectedPort(t.port);
    setPathInput(t.path);
    suppressActiveSync = false;
    setCurrentUrl(null);
    setNavError(null);
    autoTried = false;
    lastShownUrl = null;
    if (wsId) saveTabs(wsId, tabs(), id);
    // Auto-navigate if the tab's port is already detected on the server.
    if (
      t.port != null &&
      p.detectedPorts.some((d) => d.remote_port === t.port)
    ) {
      void go();
    }
  };

  const newTab = (): void => {
    const wsId = p.workspace?.id;
    if (!wsId) return;
    // Inherit the current port so exploring multiple paths on one
    // service is one keystroke; blank if nothing's been picked yet.
    const fresh: BrowserTab = {
      id: newTabId(),
      port: selectedPort(),
      path: "",
    };
    const next = [...tabs(), fresh];
    setTabs(next);
    saveTabs(wsId, next, fresh.id);
    switchTab(fresh.id);
  };

  const closeTab = (id: string): void => {
    const list = tabs();
    if (list.length <= 1) return; // never close the last tab
    const idx = list.findIndex((t) => t.id === id);
    if (idx === -1) return;
    const wsId = p.workspace?.id;
    const next = list.filter((t) => t.id !== id);
    setTabs(next);
    if (activeTabId() === id) {
      // Prefer the previous tab; fall through to index 0 if we closed [0].
      const neighbor = next[Math.max(0, idx - 1)] ?? next[0]!;
      if (wsId) saveTabs(wsId, next, neighbor.id);
      switchTab(neighbor.id);
    } else if (wsId) {
      saveTabs(wsId, next, activeTabId());
    }
  };

  // Ctrl+T / Ctrl+W / Ctrl+Tab while the browser window is open. Uses
  // window.addEventListener so it fires whether focus is on the port
  // bar, the webview host div, or nowhere in particular.
  onMount(() => {
    const handler = (e: KeyboardEvent) => {
      if (!p.open) return;
      if (p.anyModalOpen()) return;
      const meta = e.ctrlKey || e.metaKey;
      if (!meta) return;
      if (e.key === "t" && !e.shiftKey && !e.altKey) {
        e.preventDefault();
        newTab();
      } else if (e.key === "w" && !e.shiftKey && !e.altKey) {
        e.preventDefault();
        const id = activeTabId();
        if (id) closeTab(id);
      } else if (e.key === "Tab") {
        e.preventDefault();
        const list = tabs();
        if (list.length < 2) return;
        const idx = list.findIndex((t) => t.id === activeTabId());
        if (idx === -1) return;
        const delta = e.shiftKey ? -1 : 1;
        const nxt = list[(idx + delta + list.length) % list.length]!;
        switchTab(nxt.id);
      }
    };
    window.addEventListener("keydown", handler);
    onCleanup(() => window.removeEventListener("keydown", handler));
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
            <IconGlobe size={14} />{" "}
            {t("browser.window.title", { workspace: p.workspace!.name })}
          </span>
          <button
            class="browser-window-x"
            onClick={p.onClose}
            title={t("common.close")}
            aria-label={t("common.close")}
          >
            <IconClose size={14} />
          </button>
        </div>
        {/* Beta.3: tab bar. Each tab is a (port, path) pair — no
            URL — matching the port-picker model of the port bar below.
            Height (26) + header (32) + port bar (32) = CHROME_TOP_PX. */}
        <div class="browser-window-tabs">
          <div class="bw-tabs-list">
            <For each={tabs()}>
              {(tab) => (
                <div
                  class={`bw-tab${
                    activeTabId() === tab.id ? " bw-tab-active" : ""
                  }`}
                  onClick={() => switchTab(tab.id)}
                  onAuxClick={(e) => {
                    // Middle-click closes the tab (button === 1).
                    if (e.button === 1) {
                      e.preventDefault();
                      closeTab(tab.id);
                    }
                  }}
                  title={tabLabel(tab)}
                >
                  <span class="bw-tab-label">{tabLabel(tab)}</span>
                  <Show when={tabs().length > 1}>
                    <button
                      class="bw-tab-close"
                      onClick={(e) => {
                        e.stopPropagation();
                        closeTab(tab.id);
                      }}
                      title={t("common.close")}
                      aria-label={t("common.close")}
                    >
                      <IconClose size={10} />
                    </button>
                  </Show>
                </div>
              )}
            </For>
            <button
              class="bw-tab-new"
              onClick={newTab}
              title={t("browser.tabs.new")}
              aria-label={t("browser.tabs.new")}
            >
              +
            </button>
          </div>
        </div>
        {/* Phase 62.C: port bar replaces the free URL bar. Refresh ·
            remote-port dropdown · path · Go. Height (32) stays in sync
            with CHROME_TOP_PX (header 32 + tabs 26 + this 32 = 90). */}
        <div class="browser-window-portbar">
          <button
            class="bw-port-btn"
            title={t("browser.ports.refresh")}
            onClick={refresh}
          >
            <IconRefresh size={14} />
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
                    <div class="bw-empty-icon"><IconUnplug /></div>
                    <p class="bw-empty-body">{t("browser.empty.pickHint")}</p>
                    <Show when={navError()}>
                      <p class="bw-empty-err"><IconWarning size={14} /> {navError()}</p>
                    </Show>
                  </div>
                }
              >
                <div class="bw-empty-state">
                  <div class="bw-empty-icon"><IconGlobe /></div>
                  <h3 class="bw-empty-title">{t("browser.empty.title")}</h3>
                  <p class="bw-empty-body">{t("browser.empty.body")}</p>
                  <button class="bw-empty-refresh" onClick={refresh}>
                    <IconRefresh size={14} /> {t("browser.ports.refresh.label")}
                  </button>
                  <Show when={navError()}>
                    <p class="bw-empty-err"><IconWarning size={14} /> {navError()}</p>
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
