import { Show, type JSX } from "solid-js";
import { t } from "./i18n";

// Unified side-panel lifecycle: the shared header for the NON-drawer
// surfaces (float + fullscreen). SideDrawer owns its own header; this
// mirrors it for the floating window and the fullscreen overlay so the
// three surfaces share button semantics. Which buttons appear is driven
// purely by which callbacks are passed:
//   ⇤ collapse → back to drawer   ⛶ fullscreen   ⤢ float   ✕ close
// `onHeaderMouseDown` (float only) makes the header a drag handle; the
// actions cluster carries `.panel-chrome-actions` so the drag guard can
// ignore mousedowns on the buttons.

interface Props {
  icon?: string;
  title: string;
  /** Extra header controls (filters, refresh, tabs) rendered before the
   *  surface-transition buttons. */
  headerActions?: JSX.Element;
  onCollapse?: () => void; // ⇤ dock back as a drawer
  onFullscreen?: () => void; // ⛶ expand to fill the workspace
  onFloat?: () => void; // ⤢ pop out to a floating window
  onClose: () => void;
  /** When set, the header acts as a drag handle (floating surface). */
  onHeaderMouseDown?: (e: MouseEvent) => void;
}

export function PanelChrome(p: Props) {
  return (
    <div class="panel-chrome-head" onMouseDown={p.onHeaderMouseDown}>
      <span class="panel-chrome-title">
        <Show when={p.icon}>
          <span class="panel-chrome-icon" aria-hidden="true">{p.icon}</span>
        </Show>
        {p.title}
      </span>
      <div class="panel-chrome-actions">
        {p.headerActions}
        <Show when={p.onCollapse}>
          <button
            class="side-drawer-btn"
            title={t("panel.collapse")}
            aria-label={t("panel.collapse")}
            onClick={() => p.onCollapse!()}
          >
            ⇤
          </button>
        </Show>
        <Show when={p.onFullscreen}>
          <button
            class="side-drawer-btn"
            title={t("drawer.expand")}
            aria-label={t("drawer.expand")}
            onClick={() => p.onFullscreen!()}
          >
            ⛶
          </button>
        </Show>
        <Show when={p.onFloat}>
          <button
            class="side-drawer-btn"
            title={t("drawer.popout")}
            aria-label={t("drawer.popout")}
            onClick={() => p.onFloat!()}
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
  );
}
