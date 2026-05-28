// Phase 35 (#1.5): the data-model types below are generated from the
// Rust structs by ts-rs and re-exported here so existing imports
// (`from "./types"`) keep working. Regenerate after a Rust struct
// change with `cd app/src-tauri && cargo test`. Do not hand-edit
// `src/bindings/*.ts`.
//
// Note: ts-rs renders `Option<T>` as `T | null` (a required, nullable
// key) rather than the optional `T?` the hand-written mirror used.
// Helpers that accept these structurally (e.g. effectiveIdentity)
// widen their params to `T | null | undefined` accordingly.
export type { Connection } from "./bindings/Connection";
export type { SplitDirection } from "./bindings/SplitDirection";
export type { PaneKind } from "./bindings/PaneKind";
export type { BrowserState } from "./bindings/BrowserState";
export type { EnvVar } from "./bindings/EnvVar";
export type { LayoutNode } from "./bindings/LayoutNode";
export type { Workspace } from "./bindings/Workspace";
export type { FeedItem } from "./bindings/FeedItem";
export type { FeedItemState } from "./bindings/FeedItemState";

import type { Connection } from "./bindings/Connection";
import type { LayoutNode } from "./bindings/LayoutNode";
import type { PaneKind } from "./bindings/PaneKind";
import type { Workspace } from "./bindings/Workspace";

// Phase 23.F: shape returned by pane_list_tmux_sessions. Used by
// the Connect (tmux) picker.
export interface TmuxSessionInfo {
  name: string;
  created: number;
  attached: boolean;
  windows: number;
  last_attached: number;
}

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

// Phase 35: pane_kind is now non-optional in the generated binding
// (ts-rs emits the serde default-elided field as required). The
// `?? "terminal"` is kept as a defensive fallback for any legacy
// object that still lacks it at runtime.
export function paneKindOf(p: LayoutNode & { kind: "pane" }): PaneKind {
  return p.pane_kind ?? "terminal";
}

export type WorkspacesFile = {
  version: 1;
  active_workspace_id: string | null;
  workspaces: Workspace[];
};

export type PtyDataEvent = { session_id: string; data: string };
export type PtyExitEvent = { session_id: string; reason: string | null };

// Phase 36 (#2.2): a live auto port-forward, as tracked on the
// frontend. opened_at is stamped client-side when the
// port-forward-opened event arrives (the backend doesn't persist it).
export type ForwardRow = {
  workspace_id: string;
  remote_port: number;
  local_port: number;
  remote_addr: string;
  opened_at: number;
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
  pane: { color?: string | null; emoji?: string | null } | null | undefined,
  ws: { color?: string | null; emoji?: string | null } | null | undefined,
): { color?: string; emoji?: string } {
  return {
    color: pane?.color ?? ws?.color ?? undefined,
    emoji: pane?.emoji ?? ws?.emoji ?? undefined,
  };
}

export function describeConnection(c: Connection): string {
  if (c.type === "local") return c.shell ? `local · ${c.shell}` : "local";
  return `ssh ${c.user}@${c.host}:${c.port}`;
}
