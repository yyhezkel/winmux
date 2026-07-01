// ws.ts — typed WebSocket wrapper for a workspace session stream (8a/8b). The
// server sends WinmuxFrame values (generated union); this wraps subscribe +
// the client→server commands. WebSocket is injected so it works in the browser
// (global), node (`ws` package), or React Native.
import type { WinmuxFrame } from "./frames.gen.js";

export type { WinmuxFrame } from "./frames.gen.js";

// Minimal structural type for a WebSocket implementation (browser or `ws`).
export interface WSLike {
  send(data: string): void;
  close(): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  onclose: ((ev: unknown) => void) | null;
}
export type WSFactory = (url: string) => WSLike;

export interface SubscribeOptions {
  baseUrl: string; // http(s):// or ws(s):// — normalized to ws(s)://
  token?: string;
  workspaceId: string;
  sessionId: string;
  clientId?: string;
  deviceName?: string;
  cursor?: number;
  /** WebSocket factory. Browser: `(u) => new WebSocket(u)`. */
  makeSocket: WSFactory;
  onFrame: (frame: WinmuxFrame) => void;
  onError?: (err: unknown) => void;
  onClose?: () => void;
}

export class WorkspaceSocket {
  private ws: WSLike;

  constructor(private readonly opts: SubscribeOptions) {
    const base = opts.baseUrl.replace(/^http/, "ws").replace(/\/$/, "");
    const q = new URLSearchParams();
    if (opts.clientId) q.set("client_id", opts.clientId);
    if (opts.deviceName) q.set("device_name", opts.deviceName);
    if (opts.cursor != null) q.set("cursor", String(opts.cursor));
    if (opts.token) q.set("token", opts.token); // header not always settable on WS
    const url = `${base}/api/v2/workspace/${encodeURIComponent(opts.workspaceId)}/session/${encodeURIComponent(
      opts.sessionId,
    )}/subscribe?${q.toString()}`;
    this.ws = opts.makeSocket(url);
    this.ws.onmessage = (ev) => {
      try {
        opts.onFrame(JSON.parse(String(ev.data)) as WinmuxFrame);
      } catch (e) {
        opts.onError?.(e);
      }
    };
    this.ws.onerror = (e) => opts.onError?.(e);
    this.ws.onclose = () => opts.onClose?.();
  }

  private send(frame: Record<string, unknown>): void {
    this.ws.send(JSON.stringify(frame));
  }

  /** Send a user message; the server echoes it to all subscribers. */
  sendUserInput(content: string): void {
    this.send({ type: "user_input", content });
  }
  /** Answer a pending hook_request (first decision from any client wins). */
  sendHookDecision(reqId: string, decision: "allow" | "deny"): void {
    this.send({ type: "hook_decision", req_id: reqId, decision });
  }
  interrupt(): void {
    this.send({ type: "interrupt" });
  }
  unsubscribe(): void {
    this.send({ type: "unsubscribe" });
  }
  close(): void {
    this.ws.close();
  }
}
