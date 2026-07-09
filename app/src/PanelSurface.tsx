import { Match, Switch, type JSX } from "solid-js";
import { SideDrawer } from "./SideDrawer";
import { PanelChrome } from "./PanelChrome";
import { PanelFloat } from "./PanelFloat";
import type { Surface } from "./panels";
import type { Geometry } from "./floatingWindow";

// Unified side-panel lifecycle: given a panel's current `surface`, render
// the right chrome around its body — docked drawer, floating window, or
// fullscreen overlay. This is the single place the three surfaces + their
// transition buttons (⇤ dock / ⛶ fullscreen / ⤢ float / ✕ close) are wired,
// so every panel is "one body, three surfaces".
//
// `body`/`headerActions` are thunks so the body is freshly created for
// whichever surface is active (Switch mounts one arm at a time). Panels
// that must preserve fetched data across a surface change keep their data
// signals on the stable outer component and read them inside `body` — the
// DOM re-renders on transition, the signals survive.

interface Props {
  surface: Surface;
  icon?: JSX.Element;
  title: string;
  /** Persisted-width key for the drawer surface; enables its resize handle. */
  drawerStorageKey?: string;
  /** Initial drawer width in px. */
  drawerDefaultWidth?: number;
  drawerMinWidth?: number;
  /** Extra class on the scrollable body of every surface. */
  bodyClass?: string;
  /** localStorage key for the float geometry (usually per panel + workspace). */
  floatStorageKey: string;
  floatDefault: Geometry;
  floatMinW: number;
  floatMinH: number;
  onClose: () => void;
  onDrawer: () => void; // dock back as a drawer
  onFloat: () => void; // pop out to floating window
  onFullscreen: () => void; // expand to fill the workspace
  headerActions?: () => JSX.Element;
  body: () => JSX.Element;
}

export function PanelSurface(p: Props) {
  return (
    <Switch>
      <Match when={p.surface === "drawer"}>
        <SideDrawer
          icon={p.icon}
          title={p.title}
          storageKey={p.drawerStorageKey}
          defaultWidth={p.drawerDefaultWidth}
          minWidth={p.drawerMinWidth}
          bodyClass={p.bodyClass}
          onClose={p.onClose}
          onExpand={p.onFullscreen}
          onPopOut={p.onFloat}
          headerActions={p.headerActions?.()}
        >
          {p.body()}
        </SideDrawer>
      </Match>

      <Match when={p.surface === "float"}>
        <PanelFloat
          icon={p.icon}
          title={p.title}
          bodyClass={p.bodyClass}
          storageKey={p.floatStorageKey}
          defaultGeom={p.floatDefault}
          minW={p.floatMinW}
          minH={p.floatMinH}
          onClose={p.onClose}
          onCollapse={p.onDrawer}
          onFullscreen={p.onFullscreen}
          headerActions={p.headerActions?.()}
        >
          {p.body()}
        </PanelFloat>
      </Match>

      <Match when={p.surface === "fullscreen"}>
        {/* Fills .layout-root (position:relative, overflow:hidden) — the
            same workspace area a maximized pane occupies. */}
        <div class="panel-fullscreen">
          <PanelChrome
            icon={p.icon}
            title={p.title}
            headerActions={p.headerActions?.()}
            onCollapse={p.onDrawer}
            onFloat={p.onFloat}
            onClose={p.onClose}
          />
          <div class={`panel-fullscreen-body ${p.bodyClass ?? ""}`}>{p.body()}</div>
        </div>
      </Match>
    </Switch>
  );
}
