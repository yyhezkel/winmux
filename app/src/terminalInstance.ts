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

    // Polish: tightened theme to match the new app palette + a couple of
    // xterm config tweaks aimed at full-screen TUIs (Claude Code's
    // slash-command popup, fzf, etc.) rendering correctly:
    //   - `allowProposedApi`     keep enabled (WebGL addon needs it)
    //   - `windowsPty.backend=conpty`  hints xterm to handle the extra clear-line
    //                            sequences ConPTY emits on local panes; harmless
    //                            for SSH panes since the remote backend is also
    //                            invariant under it
    //   - `cursorStyle: "bar"`   matches what modern interactive UIs expect;
    //                            block cursors can occlude TUI menu glyphs
    //   - `scrollOnUserInput: true` (default) — included for clarity
    //   - `windowOptions: { setWinSizeChars: true }` — let TUIs request
    //     reflow when their popup needs a different geometry
    // Known issue (tracked): Claude Code's slash-command dropdown does not
    // always render correctly inside winmux. Suspected interplay between
    // ConPTY's narrow line-clear behavior and INK's diff renderer. These
    // settings are a first attempt; if it still misbehaves the next step is
    // to verify TERM=xterm-256color is in the remote env.
    this.term = new Terminal({
      fontFamily: '"Cascadia Mono", "JetBrains Mono", Consolas, "Courier New", monospace',
      fontSize: 14,
      lineHeight: 1.15,
      cursorBlink: true,
      cursorStyle: "bar",
      cursorWidth: 2,
      allowProposedApi: true,
      allowTransparency: true,
      theme: {
        background: "#0e1116",
        foreground: "#e6edf3",
        cursor: "#7aa2f7",
        cursorAccent: "#0e1116",
        selectionBackground: "rgba(122, 162, 247, 0.35)",
        black: "#15161e",
        red: "#f7768e",
        green: "#9ece6a",
        yellow: "#e0af68",
        blue: "#7aa2f7",
        magenta: "#bb9af7",
        cyan: "#7dcfff",
        white: "#a9b1d6",
        brightBlack: "#414868",
        brightRed: "#ff7a93",
        brightGreen: "#b9f27c",
        brightYellow: "#ff9e64",
        brightBlue: "#7da6ff",
        brightMagenta: "#bb9af7",
        brightCyan: "#0db9d7",
        brightWhite: "#c0caf5",
      },
      scrollback: 10000,
      windowsPty: { backend: "conpty" },
      windowOptions: { setWinSizeChars: true },
      convertEol: false,
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
