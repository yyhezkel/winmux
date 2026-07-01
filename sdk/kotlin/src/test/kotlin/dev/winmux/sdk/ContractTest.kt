// ContractTest.kt — pure serialization contract test (no server needed; runs in
// CI with a JDK). Verifies the generated WinmuxFrame sealed type + DTOs
// round-trip real winmux-server wire payloads. The full REST/WS wire contract
// against a live server is covered by the TypeScript SDK's contract test.
package dev.winmux.sdk

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
