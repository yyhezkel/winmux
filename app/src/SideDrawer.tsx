import { Show, type JSX } from "solid-js";
import { t } from "./i18n";

// Round B (unified side-drawer): the generic slide-in panel shell. It
// generalizes the Notification Center's chrome — a click-away backdrop plus a
// panel docked to `inline-end` — so Monitor / Diff / Browser / Files can all
// present as consistent side drawers with a shared header (title + optional
// pop-out-to-window + close). Behavioural state (what's inside) stays in each
// feature; this owns only the shell.

interface Props {
  icon?: string;
  title: string;
  /** Panel width. Defaults to a comfortable 400px, clamped on small screens. */
  width?: string;
  onClose: () => void;
  /** When provided, a ⤢ button appears that pops the drawer out into its own
   *  floating window. Omit for drawers with no windowed mode. */
  onPopOut?: () => void;
  /** Extra header controls (tabs, refresh, filters) rendered before the
   *  pop-out / close buttons. */
  headerActions?: JSX.Element;
  /** Extra class on the scrollable body (per-feature layout). */
  bodyClass?: string;
  children: JSX.Element;
}

export function SideDrawer(p: Props) {
  return (
    <div class="side-drawer-backdrop" onClick={p.onClose}>
      <div
        class="side-drawer"
        style={p.width ? { width: p.width } : undefined}
        onClick={(e) => e.stopPropagation()}
      >
        <div class="side-drawer-head">
          <span class="side-drawer-title">
            <Show when={p.icon}>
              <span class="side-drawer-icon" aria-hidden="true">{p.icon}</span>
            </Show>
            {p.title}
          </span>
          <div class="side-drawer-actions">
            {p.headerActions}
            <Show when={p.onPopOut}>
              <button
                class="side-drawer-btn"
                title={t("drawer.popout")}
                aria-label={t("drawer.popout")}
                onClick={() => p.onPopOut!()}
              >
                ⤢
              </button>
            </Show>
            <button
              class="side-drawer-btn"
              title={t("common.close")}
              aria-label={t("common.close")}
              onClick={p.onClose}
            >
              ✕
            </button>
          </div>
        </div>
        <div class={`side-drawer-body ${p.bodyClass ?? ""}`}>{p.children}</div>
      </div>
    </div>
  );
}
