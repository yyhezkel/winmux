import { Terminal, type IDisposable } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { invoke } from "@tauri-apps/api/core";
import { reorderRtlForDisplay } from "./bidi";

export class TerminalInstance {
  term: Terminal;
  fit: FitAddon;
  container: HTMLDivElement;
  sessionId: string | null = null;
  paneId: string;
  private dataDisposable: IDisposable | null = null;
  private ro: ResizeObserver | null = null;

  constructor(paneId: string) {
    this.paneId = paneId;
    this.container = document.createElement("div");
    this.container.className = "terminal-container";

    this.term = new Terminal({
      fontFamily: '"Cascadia Mono", Consolas, "Courier New", monospace',
      fontSize: 14,
      cursorBlink: true,
      allowProposedApi: true,
      theme: { background: "#0b0d10", foreground: "#e6e6e6" },
      scrollback: 10000,
    });
    this.fit = new FitAddon();
    this.term.loadAddon(this.fit);
    this.term.open(this.container);
    try {
      this.term.loadAddon(new WebglAddon());
    } catch (e) {
      console.warn("WebGL addon unavailable", e);
    }

    this.ro = new ResizeObserver(() => this.fitAndResize());
    this.ro.observe(this.container);
  }

  attach(sessionId: string) {
    this.detach();
    this.sessionId = sessionId;
    this.dataDisposable = this.term.onData((data) => {
      if (this.sessionId)
        invoke("pty_write", { sessionId: this.sessionId, data }).catch((err) =>
          console.error("pty_write failed", err)
        );
    });
    this.fitAndResize();
  }

  detach() {
    this.dataDisposable?.dispose();
    this.dataDisposable = null;
    this.sessionId = null;
  }

  fitAndResize() {
    try {
      this.fit.fit();
    } catch {}
    if (this.sessionId) {
      invoke("pty_resize", {
        sessionId: this.sessionId,
        cols: this.term.cols,
        rows: this.term.rows,
      }).catch(() => {});
    }
  }

  writeData(data: string) {
    this.term.write(reorderRtlForDisplay(data));
  }

  notice(msg: string) {
    this.term.writeln(`\r\n\x1b[33m${msg}\x1b[0m`);
  }

  focus() {
    this.term.focus();
  }

  dispose() {
    this.ro?.disconnect();
    this.ro = null;
    this.detach();
    this.term.dispose();
    if (this.container.parentElement) {
      this.container.parentElement.removeChild(this.container);
    }
  }
}
