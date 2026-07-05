// ContractTest.kt — pure serialization contract test (no server needed; runs in
// CI with a JDK). Verifies the generated WinmuxFrame sealed type + DTOs
// round-trip real winmux-server wire payloads. The full REST/WS wire contract
// against a live server is covered by the TypeScript SDK's contract test.
package dev.winmux.sdk

import kotlinx.serialization.builtins.ListSerializer
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ContractTest {
    private val json = WinmuxJson.instance

    @Test
    fun `hello frame deserializes to HelloFrame`() {
        val f = json.decodeFromString(
            WinmuxFrame.serializer(),
            """{"type":"hello","frame_version":2,"session_id":"sess_x","client_id":"A"}""",
        )
        assertTrue(f is HelloFrame)
        assertEquals(2L, f.frameVersion)
        assertEquals("sess_x", f.sessionId)
    }

    @Test
    fun `hook_resolved frame carries envelope plus decision`() {
        val f = json.decodeFromString(
            WinmuxFrame.serializer(),
            """{"type":"hook_resolved","seq":7,"session_id":"sess_x","ts":1782900000,"req_id":"req1","decision":"allow","resolved_by":"A"}""",
        )
        assertTrue(f is HookResolvedFrame)
        assertEquals("req1", f.reqId)
        assertEquals("allow", f.decision)
        assertEquals("A", f.resolvedBy)
        assertEquals(7L, f.seq)
    }

    @Test
    fun `unknown extra fields are ignored`() {
        val f = json.decodeFromString(
            WinmuxFrame.serializer(),
            """{"type":"user_input","seq":3,"session_id":"s","ts":1,"content":"hi","client_id":"A","future_field":true}""",
        )
        assertTrue(f is UserInputFrame)
        assertEquals("hi", f.content)
    }

    @Test
    fun `S6 mobile DTOs round-trip`() {
        val redeem = json.decodeFromString(
            PairingRedeemResponse.serializer(),
            """{"device_id":"dev_abc","long_term_token":"tok123","default_workspace_id":"ws_default"}""",
        )
        assertEquals("dev_abc", redeem.deviceId)
        assertEquals("ws_default", redeem.defaultWorkspaceId)

        val ws = json.decodeFromString(
            ListSerializer(Workspace.serializer()),
            """[{"id":"ws_default","name":"default","created_at":1782900000,"active_session_count":2}]""",
        )
        assertEquals("ws_default", ws[0].id)
        assertEquals(2L, ws[0].activeSessionCount)

        val created = json.decodeFromString(
            SessionCreated.serializer(),
            """{"session_id":"sess_x","kind":"claude_chat"}""",
        )
        assertEquals("sess_x", created.sessionId)

        val detail = json.decodeFromString(
            Session.serializer(),
            """{"id":"sess_x","kind":"claude_chat","workspace_id":"ws_default","subscribers":1,"pending_requests":[],"event_count":5}""",
        )
        assertEquals("ws_default", detail.workspaceId)
        assertEquals(5L, detail.eventCount)
    }

    @Test
    fun `REST DTOs round-trip`() {
        val v = json.decodeFromString(
            VersionBody.serializer(),
            """{"name":"winmux-server","version":"2.0.0","api_versions":[2],"frame_version":2}""",
        )
        assertEquals("winmux-server", v.name)
        assertEquals(listOf(2L), v.apiVersions)

        val list = json.decodeFromString(
            FileListBody.serializer(),
            """{"cwd":"/home","entries":[{"name":"a.txt","type":"file","size":9,"modified":1782900000}]}""",
        )
        assertEquals(1, list.entries.size)
        assertEquals("a.txt", list.entries[0].name)
    }
}
