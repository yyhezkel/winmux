// WorkspaceSessionApi.kt — create sessions in a workspace (Phase 77 S6).
// Reached via client.workspaces.sessions(workspaceId).
package dev.winmux.sdk

import kotlinx.serialization.encodeToString

class WorkspaceSessionApi(private val c: WinmuxClient, private val workspaceId: String) {
    /**
     * POST /api/v2/workspace/{id}/sessions — start a session (e.g.
     * `CreateSessionRequest(kind = "claude_chat")`). Returns the new session id.
     */
    fun create(request: CreateSessionRequest): SessionCreated =
        c.json.decodeFromString(
            SessionCreated.serializer(),
            c.postText("/api/v2/workspace/${enc(workspaceId)}/sessions", c.json.encodeToString(request)),
        )

    private fun enc(s: String): String = java.net.URLEncoder.encode(s, "UTF-8")
}
