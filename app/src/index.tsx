/* @refresh reload */
import { render } from "solid-js/web";
import { invoke } from "@tauri-apps/api/core";
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

// Unshipped-fivefer (#4): pop-out terminal windows load
// `index.html?popout=<sid>`. Bail to a bare full-screen <PopoutTerminal>
// BEFORE mounting <App>, so none of the workspace/settings bootstrap runs
// in the popout webview.
const popoutParams = new URLSearchParams(window.location.search);
const popoutSid = popoutParams.get("popout");
if (popoutSid) {
  const dir = popoutParams.get("dir");
  if (dir === "rtl" || dir === "ltr") document.documentElement.dir = dir;
  render(
    () => <PopoutTerminal sessionId={popoutSid} />,
    document.getElementById("root") as HTMLElement,
  );
} else {
  render(() => <App />, document.getElementById("root") as HTMLElement);
}
