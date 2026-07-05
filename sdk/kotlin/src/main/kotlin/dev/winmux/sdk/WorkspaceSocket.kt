// WorkspaceSocket.kt — typed WebSocket wrapper for a workspace session stream
// (8a/8b). Deserializes server frames into the generated WinmuxFrame sealed
// type and exposes the client→server commands. Built on OkHttp's WebSocket.
package dev.winmux.sdk

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

interface FrameHandler {
    fun onFrame(frame: WinmuxFrame)
    fun onError(t: Throwable) {}
    fun onClosed() {}
}

class WorkspaceSocket private constructor(private val ws: WebSocket) {

    fun sendUserInput(content: String) = send(buildJsonObject {
        put("type", JsonPrimitive("user_input")); put("content", JsonPrimitive(content))
    })

    fun sendHookDecision(reqId: String, decision: String) = send(buildJsonObject {
        put("type", JsonPrimitive("hook_decision")); put("req_id", JsonPrimitive(reqId)); put("decision", JsonPrimitive(decision))
    })

    fun interrupt() = send(buildJsonObject { put("type", JsonPrimitive("interrupt")) })
    fun unsubscribe() = send(buildJsonObject { put("type", JsonPrimitive("unsubscribe")) })
    fun close() { ws.close(1000, null) }

    private fun send(obj: JsonObject) { ws.send(WinmuxJson.instance.encodeToString(JsonObject.serializer(), obj)) }

    companion object {
        /**
         * Open a subscribe stream. [baseUrl] may be http(s):// or ws(s)://.
         * The bearer token is passed as ?token= (WS can't always set headers).
         */
        fun subscribe(
            baseUrl: String,
            token: String?,
            workspaceId: String,
            sessionId: String,
            clientId: String? = null,
            deviceName: String? = null,
            cursor: Long? = null,
            handler: FrameHandler,
            http: OkHttpClient = OkHttpClient(),
        ): WorkspaceSocket {
            val wsBase = baseUrl.replace(Regex("^http"), "ws").trimEnd('/')
            val q = buildList {
                clientId?.let { add("client_id=" + enc(it)) }
                deviceName?.let { add("device_name=" + enc(it)) }
                cursor?.let { add("cursor=$it") }
                token?.let { add("token=" + enc(it)) }
            }.joinToString("&")
            val url = "$wsBase/api/v2/workspace/${enc(workspaceId)}/session/${enc(sessionId)}/subscribe?$q"
            val req = Request.Builder().url(url).build()
            val socket = http.newWebSocket(req, object : WebSocketListener() {
                override fun onMessage(webSocket: WebSocket, text: String) {
                    try {
                        handler.onFrame(WinmuxJson.instance.decodeFromString(WinmuxFrame.serializer(), text))
                    } catch (t: Throwable) {
                        handler.onError(t)
                    }
                }
                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) = handler.onError(t)
                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = handler.onClosed()
            })
            return WorkspaceSocket(socket)
        }

        private fun enc(s: String): String = java.net.URLEncoder.encode(s, "UTF-8")
    }
}
