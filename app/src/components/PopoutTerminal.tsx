import { onCleanup, onMount } from "solid-js";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { TerminalInstance } from "../terminalInstance";

// Unshipped-fivefer (#4): the pop-out terminal window.
//
// index.tsx early-bails here when the URL carries `?popout=<sid>`, so this
// renders in a FRESH webview with none of App.tsx's workspace/settings
// bootstrap. It reuses TerminalInstance (same onData→pty_write + resize
// contract as an in-grid pane) and taps the app-wide `pty:data` / `pty:exit`
// streams filtered to its own session.
//
// Ownership while open: this window drives input + resize (pty_write /
// pty_resize); the origin pane in the main window detaches to a read-only
// mirror (App.tsx), so there's no SIGWINCH tug-of-war over the PTY.
//
// Lifecycle: `pty:exit` for our session → notice + self-close. The main
// window re-attaches when it receives `popout:closed` (emitted by the Rust
// side on window Destroyed).

interface PtyDataEvent {
  session_id: string;
  data: string;
}
interface PtyExitEvent {
  session_id: string;
  reason?: string | null;
}

export function PopoutTerminal(props: { sessionId: string }) {
  let hostRef!: HTMLDivElement;
  let ti: TerminalInstance | null = null;
  const unlistens: UnlistenFn[] = [];

  onMount(() => {
    ti = new TerminalInstance(`popout-${props.sessionId}`);
    hostRef.appendChild(ti.container);
    ti.container.style.display = "block";
    // attach() binds onData→pty_write and pushes an initial pty_resize, so
    // this window becomes the resize authority immediately.
    ti.attach(props.sessionId);
    requestAnimationFrame(() => ti?.fitAndResize(true));
    ti.focus();

    void (async () => {
      unlistens.push(
        await listen<PtyDataEvent>("pty:data", (e) => {
          if (e.payload.session_id === props.sessionId) {
            ti?.writeData(e.payload.data);
          }
        }),
      );
      unlistens.push(
        await listen<PtyExitEvent>("pty:exit", (e) => {
          if (e.payload.session_id !== props.sessionId) return;
          ti?.notice(
            `[session ended${e.payload.reason ? ` (${e.payload.reason})` : ""}]`,
          );
          // Let the notice land, then close the window. Rust's Destroyed
          // handler emits popout:closed so the main pane cleans up.
          setTimeout(() => {
            void getCurrentWindow().close();
          }, 1200);
        }),
      );
    })();
  });

  onCleanup(() => {
    for (const u of unlistens) {
      try {
        u();
      } catch {
        // best-effort
      }
    }
    ti?.dispose();
    ti = null;
  });

  return <div ref={hostRef!} class="popout-terminal-host" />;
}
