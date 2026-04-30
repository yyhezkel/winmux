export type Connection =
  | { type: "local"; shell?: string }
  | { type: "ssh"; host: string; user: string; port: number; key_path?: string };

export type SplitDirection = "horizontal" | "vertical";

export type LayoutNode =
  | { kind: "pane"; pane_id: string; connection: Connection }
  | {
      kind: "split";
      split_id: string;
      direction: SplitDirection;
      first: LayoutNode;
      second: LayoutNode;
      ratio: number;
    };

export type Workspace = {
  id: string;
  name: string;
  color?: string;
  cwd?: string;
  // Legacy field, only present on disk during a migration window.
  connection?: Connection;
  layout?: LayoutNode;
};

export type WorkspacesFile = {
  version: 1;
  active_workspace_id: string | null;
  workspaces: Workspace[];
};

export type PtyDataEvent = { session_id: string; data: string };
export type PtyExitEvent = { session_id: string; reason: string | null };

export function collectPanes(node: LayoutNode): string[] {
  if (node.kind === "pane") return [node.pane_id];
  return [...collectPanes(node.first), ...collectPanes(node.second)];
}

export function findPane(
  node: LayoutNode,
  paneId: string
): { kind: "pane"; pane_id: string; connection: Connection } | null {
  if (node.kind === "pane")
    return node.pane_id === paneId ? node : null;
  return findPane(node.first, paneId) ?? findPane(node.second, paneId);
}

export function describeConnection(c: Connection): string {
  if (c.type === "local") return c.shell ? `local · ${c.shell}` : "local";
  return `ssh ${c.user}@${c.host}:${c.port}`;
}
