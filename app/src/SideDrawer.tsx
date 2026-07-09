import { Show, createSignal, type JSX } from "solid-js";
import { t } from "./i18n";
import { IconMaximize, IconExternalLink, IconClose } from "./icons";

// Round B (unified side-drawer): the generic slide-in panel shell. It
// generalizes the Notification Center's chrome — a click-away backdrop plus a
// panel docked to `inline-end` — so Monitor / Diff / Browser / Files can all
// present as consistent side drawers with a shared header (title + optional
// pop-out-to-window + close). Behavioural state (what's inside) stays in each
// feature; this owns only the shell.
//
// Phase 79: the drawer width is now drag-resizable. When `storageKey` is given,
// the inline-start edge grows/shrinks the panel (RTL-aware) and the chosen
// width persists to localStorage — mirrors the sidebar's `startSidebarResize`.

interface Props {
  icon?: JSX.Element;
  title: string;
  /** localStorage key for the persisted width; enables the resize handle. */
  storageKey?: string;
  /** Initial width in px (used until the user drags / a stored value exists). */
  defaultWidth?: number;
  minWidth?: number;
  maxWidth?: number;
  onClose: () => void;
  /** When provided, a maximize button appears that expands the drawer to fill
   *  the workspace like a maximized pane. */
  onExpand?: () => void;
  /** When provided, a pop-out button appears that floats the drawer into its
   *  own window. */
  onPopOut?: () => void;
  /** Extra header controls (tabs, refresh, filters) rendered before the
   *  pop-out / close buttons. */
  headerActions?: JSX.Element;
  /** Extra class on the scrollable body (per-feature layout). */
  bodyClass?: string;
  children: JSX.Element;
}

const clampW = (w: number, min: number, max: number) => Math.max(min, Math.min(max, w));

function loadWidth(key: string | undefined, def: number, min: number, max: number): number {
  if (key) {
    try {
      const raw = localStorage.getItem(key);
      if (raw) {
        const n = parseInt(raw, 10);
        if (Number.isFinite(n)) return clampW(n, min, max);
      }
    } catch {
      /* localStorage unavailable — fall through to the default */
    }
  }
  return clampW(def, min, max);
}

export function SideDrawer(p: Props) {
  const def = () => p.defaultWidth ?? 400;
  const min = () => p.minWidth ?? 300;
  const max = () => p.maxWidth ?? Math.round(window.innerWidth * 0.96);
  const [width, setWidth] = createSignal(loadWidth(p.storageKey, def(), min(), max()));

  const startResize = (e: MouseEvent) => {
    e.preventDefault();
    const rtl = getComputedStyle(document.documentElement).direction === "rtl";
    const onMove = (ev: MouseEvent) => {
      // The drawer is docked to inline-end; its free (draggable) edge is
      // inline-start, so width = distance from the cursor to the docked edge.
      // LTR docks right (width = innerWidth - x); RTL docks left (width = x).
      const raw = rtl ? ev.clientX : window.innerWidth - ev.clientX;
      setWidth(clampW(Math.round(raw), min(), max()));
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      if (p.storageKey) {
        try {
          localStorage.setItem(p.storageKey, String(width()));
        } catch {
          /* ignore persistence failure */
        }
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  return (
    <div class="side-drawer-backdrop" onClick={p.onClose}>
      <div
        class="side-drawer"
        style={{ width: `${width()}px` }}
        onClick={(e) => e.stopPropagation()}
      >
        <Show when={p.storageKey}>
          <div
            class="side-drawer-resizer"
            onMouseDown={startResize}
            title={t("sidebar.resize.tooltip")}
          />
        </Show>
        <div class="side-drawer-head">
          <span class="side-drawer-title">
            <Show when={p.icon}>
              <span class="side-drawer-icon" aria-hidden="true">{p.icon}</span>
            </Show>
            {p.title}
          </span>
          <div class="side-drawer-actions">
            {p.headerActions}
            <Show when={p.onExpand}>
              <button
                class="side-drawer-btn"
                title={t("drawer.expand")}
                aria-label={t("drawer.expand")}
                onClick={() => p.onExpand!()}
              >
                <IconMaximize />
              </button>
            </Show>
            <Show when={p.onPopOut}>
              <button
                class="side-drawer-btn"
                title={t("drawer.popout")}
                aria-label={t("drawer.popout")}
                onClick={() => p.onPopOut!()}
              >
                <IconExternalLink />
              </button>
            </Show>
            <button
              class="side-drawer-btn"
              title={t("common.close")}
              aria-label={t("common.close")}
              onClick={p.onClose}
            >
              <IconClose />
            </button>
          </div>
        </div>
        <div class={`side-drawer-body ${p.bodyClass ?? ""}`}>{p.children}</div>
      </div>
    </div>
  );
}
