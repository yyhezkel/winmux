export type Connection =
  | { type: "local"; shell?: string }
  | { type: "ssh"; host: string; user: string; port: number; key_path?: string };

// Phase 23.F: shape returned by pane_list_tmux_sessions. Used by
// the Connect (tmux) picker.
export interface TmuxSessionInfo {
  name: string;
  created: number;
  attached: boolean;
  windows: number;
  last_attached: number;
}

export type SplitDirection = "horizontal" | "vertical";

// Phase 8.A: pane kind. Default = terminal for legacy panes (server omits the field).
// Phase 24.D: removed "claudechat" (Phase 22) + "claudelog" (Phase 24.B) — backend
// aliases those JSON variants back to "terminal" at deserialize time.
export type PaneKind = "terminal" | "browser" | "filemanager";

export type BrowserState = {
  url: string;
  home_url?: string;
  history: string[];
  // Phase 8.B: when true (default), localhost:N URLs in this pane are
  // transparently routed through an SSH local port forward to the remote
  // workspace. Persisted; toggle via pane_browser_set_forward.
  forward_localhost?: boolean;
};

// Phase 24.D: ChatRole / MessageStatus / ChatMessage / ClaudeChatState
// (Phase 22) removed alongside the ClaudeChat pane. The
// ClaudeLog* types just below are kept for the dead-code-but-
// registered backend (a future unified-view rebuild can consume the
// existing claude_log_sync / list / read commands without re-typing).

/** Phase 24.B: kept for future unified-view rebuild — no current consumer. */
export interface ClaudeSyncResult {
  synced: number;
  skipped: number;
  errors: string[];
  total_bytes: number;
}

/** Phase 24.B: kept for future unified-view rebuild — no current consumer. */
export interface ClaudeLogSummary {
  session_id: string;
  message_count: number;
  first_user?: string;
  last_assistant?: string;
  project_path?: string;
  file_size: number;
  /** Unix seconds. */
  local_mtime: number;
}

/** Phase 24.B: kept for future unified-view rebuild — no current consumer. */
export interface ClaudeLogEntry {
  line_no: number;
  /** "user" | "assistant" | "tool_use" | "tool_result" | "system" | "summary" */
  entry_type: string;
  text: string;
  tool_name?: string;
  timestamp?: string;
  session_id?: string;
}

/** Phase 24.B: kept for future unified-view rebuild — no current consumer.
 *  The Rust-side `claudelog` field on LayoutNode::Pane was removed in 24.D
 *  along with `chat`; if the pane comes back, restore both. */
export interface ClaudeLogState {
  session_id?: string;
  filter?: string;
}

export type LayoutNode =
  | {
      kind: "pane";
      pane_id: string;
      // Optional in JSON for backward-compat; treat absent as "terminal".
      pane_kind?: PaneKind;
      // Required for terminal panes; absent for browser panes.
      connection?: Connection;
      browser?: BrowserState;
      // Phase 24.D: removed `chat` / `claudelog` fields with the
      // ClaudeChat (Phase 22) + ClaudeLog (Phase 24.B) panes.
      // Legacy JSON that still has those keys deserializes cleanly
      // (TS is structural; missing keys here are ignored).
      title?: string;
      annotation?: string;
      // Phase 31: per-pane identity overrides the workspace's. Absent
      // = inherit from the parent workspace.
      color?: string;
      emoji?: string;
    }
  | {
      kind: "split";
      split_id: string;
      direction: SplitDirection;
      first: LayoutNode;
      second: LayoutNode;
      ratio: number;
    };

export function paneKindOf(p: LayoutNode & { kind: "pane" }): PaneKind {
  return p.pane_kind ?? "terminal";
}

export type EnvVar = { key: string; value: string };

export type Workspace = {
  id: string;
  name: string;
  color?: string;
  // Phase 30: per-workspace emoji glyph, shown as a sidebar prefix
  // and in the OS window title. Free-form (up to 16 UTF-8 bytes).
  emoji?: string;
  cwd?: string;
  // Phase 23.D: canonical workspace-level connection. Set on create
  // and back-filled on load from the first Terminal pane. Drives the
  // Connect dropdown's SSH-vs-Local options via PaneView.isSsh().
  connection?: Connection;
  layout?: LayoutNode;
  // Phase 7.C
  setup_command?: string;
  teardown_command?: string;
  env?: EnvVar[];
};

export type WorkspacesFile = {
  version: 1;
  active_workspace_id: string | null;
  workspaces: Workspace[];
};

export type PtyDataEvent = { session_id: string; data: string };
export type PtyExitEvent = { session_id: string; reason: string | null };

export type FeedItemState = "pending" | "allowed" | "denied" | "timedout" | "passive";

export type FeedItem = {
  request_id: string;
  kind: string;
  subkind: string;
  pane_id?: string | null;
  workspace_id?: string | null;
  title: string;
  summary: string;
  payload: unknown;
  state: FeedItemState;
  created_ms: number;
  blocking: boolean;
};

export type FeedResolvedEvent = { request_id: string; decision: string };

export type NoteStatus = "open" | "done";

export type Note = {
  id: string;
  created_at: string;
  updated_at: string;
  text: string;
  tag?: string;
  status: NoteStatus;
  workspace_id?: string | null;
  pane_id?: string | null;
};

export type NotesFile = {
  version: 1;
  notes: Note[];
};

export function collectPanes(node: LayoutNode): string[] {
  if (node.kind === "pane") return [node.pane_id];
  return [...collectPanes(node.first), ...collectPanes(node.second)];
}

export function findPane(
  node: LayoutNode,
  paneId: string
): (LayoutNode & { kind: "pane" }) | null {
  if (node.kind === "pane")
    return node.pane_id === paneId ? node : null;
  return findPane(node.first, paneId) ?? findPane(node.second, paneId);
}

// Phase 31: a pane's effective identity is its own override falling
// back to its workspace's. Used by the pane header, the rename dialog's
// "inheriting" hint, and the OS window title.
export function effectiveIdentity(
  pane: { color?: string; emoji?: string } | null | undefined,
  ws: { color?: string; emoji?: string } | null | undefined,
): { color?: string; emoji?: string } {
  return {
    color: pane?.color ?? ws?.color,
    emoji: pane?.emoji ?? ws?.emoji,
  };
}

export function describeConnection(c: Connection): string {
  if (c.type === "local") return c.shell ? `local · ${c.shell}` : "local";
  return `ssh ${c.user}@${c.host}:${c.port}`;
}
