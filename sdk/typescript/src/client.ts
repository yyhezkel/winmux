// client.ts — the hand-written winmux-server REST client. Thin, dependency-free
// (uses the built-in fetch), typed against the generated OpenAPI schemas in
// types.gen.ts. The WS side lives in ws.ts.
import type { components } from "./types.gen.js";

type Schemas = components["schemas"];
export type FileEntry = Schemas["FileEntry"];
export type FileList = Schemas["FileListBody"];
export type UploadResult = Schemas["UploadResultBody"];
export type ClientInfo = Schemas["ClientInfo"];
export type LogRead = Schemas["ReadBody"];
export type Version = Schemas["VersionBody"];
export type Health = Schemas["HealthBody"];

export interface WinmuxClientOptions {
  /** e.g. "http://127.0.0.1:7879" */
  baseUrl: string;
  /** bearer token; omitted only for the public /healthz + /api/version. */
  token?: string;
  /** override fetch (tests / non-browser runtimes). Defaults to global fetch. */
  fetch?: typeof fetch;
}

export class WinmuxApiError extends Error {
  constructor(
    readonly status: number,
    readonly body: string,
  ) {
    super(`winmux-server ${status}: ${body}`);
    this.name = "WinmuxApiError";
  }
}

export class WinmuxClient {
  private readonly base: string;
  private readonly token?: string;
  private readonly f: typeof fetch;

  constructor(opts: WinmuxClientOptions) {
    this.base = opts.baseUrl.replace(/\/$/, "");
    this.token = opts.token;
    this.f = opts.fetch ?? globalThis.fetch.bind(globalThis);
  }

  private auth(headers: Record<string, string> = {}): Record<string, string> {
    return this.token ? { ...headers, Authorization: `Bearer ${this.token}` } : headers;
  }

  private async json<T>(path: string, init?: RequestInit): Promise<T> {
    const res = await this.f(this.base + path, { ...init, headers: this.auth(init?.headers as Record<string, string>) });
    if (!res.ok) throw new WinmuxApiError(res.status, await res.text());
    return (await res.json()) as T;
  }

  // ── meta (public) ──────────────────────────────────────────────────────
  health(): Promise<Health> {
    return this.json<Health>("/healthz");
  }
  version(): Promise<Version> {
    return this.json<Version>("/api/version");
  }

  // ── files ──────────────────────────────────────────────────────────────
  listFiles(path = "", depth: 1 | 2 = 1): Promise<FileList> {
    return this.json<FileList>(`/api/v2/files/list?path=${encodeURIComponent(path)}&depth=${depth}`);
  }
  async readFile(path: string, maxBytes?: number): Promise<{ bytes: Uint8Array; truncated: boolean }> {
    const q = maxBytes != null ? `&max_bytes=${maxBytes}` : "";
    const res = await this.f(`${this.base}/api/v2/files/read?path=${encodeURIComponent(path)}${q}`, {
      headers: this.auth(),
    });
    if (!res.ok) throw new WinmuxApiError(res.status, await res.text());
    return { bytes: new Uint8Array(await res.arrayBuffer()), truncated: res.headers.get("X-Winmux-Truncated") === "true" };
  }
  async uploadFile(path: string, data: Blob | Uint8Array, filename = "file"): Promise<UploadResult> {
    const form = new FormData();
    const blob = data instanceof Blob ? data : new Blob([data as BlobPart]);
    form.append("file", blob, filename);
    return this.json<UploadResult>(`/api/v2/files/upload?path=${encodeURIComponent(path)}`, { method: "POST", body: form });
  }
  async downloadFile(path: string): Promise<Uint8Array> {
    const res = await this.f(`${this.base}/api/v2/files/download?path=${encodeURIComponent(path)}`, {
      headers: this.auth(),
    });
    if (!res.ok) throw new WinmuxApiError(res.status, await res.text());
    return new Uint8Array(await res.arrayBuffer());
  }
  deleteFile(path: string): Promise<{ ok: boolean }> {
    return this.json<{ ok: boolean }>(`/api/v2/files/delete?path=${encodeURIComponent(path)}`, { method: "DELETE" });
  }

  // ── logs ───────────────────────────────────────────────────────────────
  listLogClients(): Promise<{ clients: ClientInfo[] }> {
    return this.json<{ clients: ClientInfo[] }>("/api/v2/logs/list");
  }
  readLog(clientId: string, file = "", tail = 200): Promise<LogRead> {
    return this.json<LogRead>(
      `/api/v2/logs/read?client_id=${encodeURIComponent(clientId)}&file=${encodeURIComponent(file)}&tail=${tail}`,
    );
  }
}
