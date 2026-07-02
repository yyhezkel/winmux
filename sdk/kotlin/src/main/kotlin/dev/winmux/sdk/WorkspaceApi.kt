// WorkspaceApi.kt — workspace list + per-session access (Phase 77 S6). Reached
// via client.workspaces. The live event stream is WorkspaceSocket (separate).
package dev.winmux.sdk

import kotlinx.serialization.builtins.ListSerializer

class WorkspaceApi(private val c: WinmuxClient) {
    /** GET /api/v2/workspace/list — all workspaces (ws_default always present). */
    fun list(): List<Workspace> =
        c.json.decodeFromString(ListSerializer(Workspace.serializer()), c.getText("/api/v2/workspace/list"))

    /** GET /api/v2/workspace/{id}/session/{sid} — a session's detail. */
    fun getSession(workspaceId: String, sessionId: String): Session =
        c.json.decodeFromString(
            Session.serializer(),
            c.getText("/api/v2/workspace/${enc(workspaceId)}/session/${enc(sessionId)}"),
        )

    /** Sessions under a workspace: `workspaces.sessions(id).create(...)`. */
    fun sessions(workspaceId: String): WorkspaceSessionApi = WorkspaceSessionApi(c, workspaceId)

    private fun enc(s: String): String = java.net.URLEncoder.encode(s, "UTF-8")
}
