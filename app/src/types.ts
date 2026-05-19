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
// Phase 22 adds "claudechat". Phase 24.B adds "claudelog".
export type PaneKind =
  | "terminal"
  | "browser"
  | "filemanager"
  | "claudechat"
  | "claudelog";

export type BrowserState = {
  url: string;
  home_url?: string;
  history: string[];
  // Phase 8.B: when true (default), localhost:N URLs in this pane are
  // transparently routed through an SSH local port forward to the remote
  // workspace. Persisted; toggle via pane_browser_set_forward.
  forward_localhost?: boolean;
};

// Phase 22: chat state persisted per pane.
export type ChatRole = "user" | "assistant" | "system";
export type MessageStatus = "sending" | "done" | "error";

export type ChatMessage = {
  id: string;
  role: ChatRole;
  content: string;
  timestamp: string;
  status?: MessageStatus;
};

export type ClaudeChatState = {
  session_id?: string;
  model?: string;
  messages: ChatMessage[];
};

// Phase 24.B: backend response shapes for claude_log_* tauri
// commands. Mirror Rust serde output (snake_case field names; None
// values are omitted from the JSON via skip_serializing_if).
export interface ClaudeSyncResult {
  synced: number;
  skipped: number;
  errors: string[];
  total_bytes: number;
}

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

export interface ClaudeLogEntry {
  line_no: number;
  /** "user" | "assistant" | "tool_use" | "tool_result" | "system" | "summary" */
  entry_type: string;
  text: string;
  tool_name?: string;
  timestamp?: string;
  session_id?: string;
}

export interface ClaudeLogState {
  /** Current session being viewed. Unset = pane shows the picker. */
  session_id?: string;
  /** Manual filter input. Persisted so reload preserves it. */
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
      // Phase 22: only set on ClaudeChat panes.
      chat?: ClaudeChatState;
      // Phase 24.B: only set on ClaudeLog panes.
      claudelog?: ClaudeLogState;
      title?: string;
      annotation?: string;
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

export function describeConnection(c: Connection): string {
  if (c.type === "local") return c.shell ? `local · ${c.shell}` : "local";
  return `ssh ${c.user}@${c.host}:${c.port}`;
}
