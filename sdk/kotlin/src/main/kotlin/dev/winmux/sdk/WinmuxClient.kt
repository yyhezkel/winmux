// WinmuxClient.kt — hand-written OkHttp client for winmux-server's REST surface,
// typed against the generated DTOs in Models.kt. The WS side is WorkspaceSocket.
package dev.winmux.sdk

import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MultipartBody
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

class WinmuxApiException(val status: Int, val bodyText: String) :
    RuntimeException("winmux-server $status: $bodyText")

/** Result of reading a file: the raw bytes + whether the server truncated them. */
data class FileBytes(val bytes: ByteArray, val truncated: Boolean)

/**
 * @param baseUrl e.g. "http://127.0.0.1:7879"
 * @param token bearer token (omit only for the public /healthz + /api/version)
 */
class WinmuxClient(
    private val baseUrl: String,
    private val token: String? = null,
    private val http: OkHttpClient = OkHttpClient(),
) {
    internal val json = Json { ignoreUnknownKeys = true; encodeDefaults = false }
    private val base = baseUrl.trimEnd('/')

    /** Pairing endpoints (`client.pairing.redeem(...)`). */
    val pairing = PairingApi(this)

    /** Workspace list + session access (`client.workspaces.list()`, `.sessions(id).create(...)`). */
    val workspaces = WorkspaceApi(this)

    private fun req(path: String): Request.Builder {
        val b = Request.Builder().url(base + path)
        if (token != null) b.header("Authorization", "Bearer $token")
        return b
    }

    // Shared request helpers used by the namespaced API classes.
    internal fun getText(path: String): String =
        http.newCall(req(path).get().build()).execute().use { res ->
            val body = res.body?.string().orEmpty()
            if (!res.isSuccessful) throw WinmuxApiException(res.code, body)
            body
        }

    internal fun postText(path: String, jsonBody: String): String =
        http.newCall(req(path).post(jsonBody.toRequestBody("application/json".toMediaType())).build()).execute().use { res ->
            val body = res.body?.string().orEmpty()
            if (!res.isSuccessful) throw WinmuxApiException(res.code, body)
            body
        }

    private inline fun <reified T> getJson(path: String): T = json.decodeFromString(getText(path))

    // ── meta (public) ──────────────────────────────────────────────────────
    fun version(): VersionBody = getJson("/api/version")
    fun health(): HealthBody = getJson("/healthz")

    // ── files ──────────────────────────────────────────────────────────────
    fun listFiles(path: String = "", depth: Int = 1): FileListBody =
        getJson("/api/v2/files/list?path=${enc(path)}&depth=$depth")

    fun readFile(path: String, maxBytes: Long? = null): FileBytes {
        val q = if (maxBytes != null) "&max_bytes=$maxBytes" else ""
        http.newCall(req("/api/v2/files/read?path=${enc(path)}$q").get().build()).execute().use { res ->
            if (!res.isSuccessful) throw WinmuxApiException(res.code, res.body?.string().orEmpty())
            return FileBytes(res.body!!.bytes(), res.header("X-Winmux-Truncated") == "true")
        }
    }

    fun uploadFile(path: String, data: ByteArray, filename: String = "file"): UploadResultBody {
        val body = MultipartBody.Builder().setType(MultipartBody.FORM)
            .addFormDataPart("file", filename, data.toRequestBody("application/octet-stream".toMediaType()))
            .build()
        http.newCall(req("/api/v2/files/upload?path=${enc(path)}").post(body).build()).execute().use { res ->
            val txt = res.body?.string().orEmpty()
            if (!res.isSuccessful) throw WinmuxApiException(res.code, txt)
            return json.decodeFromString(txt)
        }
    }

    fun downloadFile(path: String): ByteArray {
        http.newCall(req("/api/v2/files/download?path=${enc(path)}").get().build()).execute().use { res ->
            if (!res.isSuccessful) throw WinmuxApiException(res.code, res.body?.string().orEmpty())
            return res.body!!.bytes()
        }
    }

    fun deleteFile(path: String) {
        http.newCall(req("/api/v2/files/delete?path=${enc(path)}").delete().build()).execute().use { res ->
            if (!res.isSuccessful) throw WinmuxApiException(res.code, res.body?.string().orEmpty())
        }
    }

    // ── logs ───────────────────────────────────────────────────────────────
    fun listLogClients(): ClientsBody = getJson("/api/v2/logs/list")

    fun readLog(clientId: String, file: String = "", tail: Int = 200): ReadBody =
        getJson("/api/v2/logs/read?client_id=${enc(clientId)}&file=${enc(file)}&tail=$tail")

    private fun enc(s: String): String = java.net.URLEncoder.encode(s, "UTF-8")
}
