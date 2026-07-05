/* @refresh reload */
import { render } from "solid-js/web";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
// Global stylesheets live at the entry point so BOTH the main <App> and the
// #4 pop-out window (which bypasses <App>) get xterm's CSS + our theme.
// Previously these were imported inside App.tsx, so a popout webview rendered
// unstyled — blank white screen, invisible terminal.
import "@xterm/xterm/css/xterm.css";
import "./App.css";
import App from "./App";
import { PopoutTerminal } from "./components/PopoutTerminal";

// Phase 8.E: capture console.error / console.warn so `winmux dev console-tail`
// can surface frontend issues. We forward to a fire-and-forget Tauri command
// that pushes into a 200-entry ring buffer in the backend. Original console
// output is preserved.
{
  const origErr = console.error;
  const origWarn = console.warn;
  const fmt = (args: unknown[]): string =>
    args
      .map((a) => {
        if (typeof a === "string") return a;
        if (a instanceof Error) return `${a.name}: ${a.message}`;
        try {
          return JSON.stringify(a);
        } catch {
          return String(a);
        }
      })
      .join(" ");
  console.error = (...args: unknown[]) => {
    origErr(...(args as []));
    invoke("dev_console_log", {
      level: "error",
      message: fmt(args),
      ts: Date.now(),
    }).catch(() => {});
  };
  console.warn = (...args: unknown[]) => {
    origWarn(...(args as []));
    invoke("dev_console_log", {
      level: "warn",
      message: fmt(args),
      ts: Date.now(),
    }).catch(() => {});
  };
  window.addEventListener("error", (e) => {
    invoke("dev_console_log", {
      level: "error",
      message: `unhandled: ${e.message} @ ${e.filename}:${e.lineno}`,
      ts: Date.now(),
    }).catch(() => {});
  });
  window.addEventListener("unhandledrejection", (e) => {
    invoke("dev_console_log", {
      level: "error",
      message: `unhandled rejection: ${String(e.reason)}`,
      ts: Date.now(),
    }).catch(() => {});
  });
}

// Unshipped-fivefer (#4): pop-out terminal windows. Bail to a bare
// full-screen <PopoutTerminal> BEFORE mounting <App>, so none of the
// workspace/settings bootstrap runs in the popout webview.
//
// The session id comes from the window LABEL (`popout-<sid>`). The popout URL
// is a CLEAN `index.html` (no query/fragment) because Tauri's built-app asset
// protocol serves a blank page for any suffixed path — so the label, not the
// URL, carries the id.
let winLabel = "";
try {
  winLabel = getCurrentWindow().label;
} catch {
  // window metadata not ready — treat as the main window
}
const popoutSid = winLabel.startsWith("popout-")
  ? winLabel.slice("popout-".length)
  : null;

if (popoutSid) {
  render(
    () => <PopoutTerminal sessionId={popoutSid} />,
    document.getElementById("root") as HTMLElement,
  );
} else {
  render(() => <App />, document.getElementById("root") as HTMLElement);
}
