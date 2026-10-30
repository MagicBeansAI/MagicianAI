package ai.magicbeans.magdroid.work

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.plugins.timeout
import io.ktor.client.request.header
import io.ktor.client.request.prepareGet
import io.ktor.client.request.request
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.client.statement.bodyAsChannel
import io.ktor.http.ContentType
import io.ktor.http.HttpMethod
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.utils.io.readAvailable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import java.io.ByteArrayOutputStream

/** [body] is the server's JSON error object, when it sent one. */
open class WorkRequestError(val status: Int, message: String, val body: JsonObject? = null) : Exception(message)

/** The program changed, appeared or vanished after it was opened (409 `program_changed`). */
class WorkProgramChangedError(message: String, val currentSha256: String?) : WorkRequestError(409, message)

/** Frozen URL and headers: switching machine cannot send a new bearer to the old host. */
class WorkRepository(
    connection: MagicianConnectionSnapshot,
    private val client: HttpClient = HttpClient(CIO) {
        followRedirects = false
        install(HttpTimeout) { requestTimeoutMillis = 30_000; connectTimeoutMillis = 10_000 }
    },
) : AutoCloseable {
    private val connection = connection.copy(headers = connection.headers.toMap())
    companion object {
        fun usingConnection(connection: MagicianConnectionSnapshot): WorkRepository = WorkRepository(connection)
        fun connected(context: Context): WorkRepository = WorkRepository(
            MagicianAccess.connectionSnapshot(context)
                ?: throw WorkRequestError(0, "Connect to your Magician in Settings."),
        )
        fun segment(value: String): String =
            URLEncoder.encode(value, StandardCharsets.UTF_8.name()).replace("+", "%20")
    }
    fun isCurrentConnection(context: Context): Boolean = connection == MagicianAccess.connectionSnapshot(context)
    override fun close() = client.close()

    /** UI-facing JSON calls keep Ktor types inside the bridge module. */
    suspend fun readJson(path: String): JsonElement = request(path).first
    suspend fun postJson(path: String, body: JsonObject): JsonElement = request(path, HttpMethod.Post, body).first
    suspend fun putJson(path: String, body: JsonObject): JsonElement = request(path, HttpMethod.Put, body).first
    suspend fun deleteJson(path: String): JsonElement = request(path, HttpMethod.Delete).first

    suspend fun request(path: String, method: HttpMethod = HttpMethod.Get,
                                body: JsonObject? = null, etag: String? = null): Pair<JsonElement, String?> {
        require(path.startsWith("/api/magician/v2/") || (method == HttpMethod.Get && Regex("^/api/magician/v3/tasks/bootstrap__[^/?]+$").matches(path)))
        val response = client.request(connection.baseUrl.trimEnd('/') + path) {
            this.method = method
            if (path == "/api/magician/v2/analytics/llm_calls/query" && body?.text("sql")?.contains("llm_cost_daily") == true) {
                timeout { requestTimeoutMillis = 150_000 }
            }
            connection.headers.forEach { (name, value) -> header(name, value) }
            if (body != null) { contentType(ContentType.Application.Json); setBody(body.toString()) }
            if (etag != null) header("If-Match", etag)
        }
        val value = runCatching { Json.parseToJsonElement(response.bodyAsText()) }.getOrNull()
        if (!response.status.isSuccess()) {
            val message = when (response.status.value) {
                401, 403 -> "Your connection cannot access this work. Check Settings."
                412 -> "This crew member changed elsewhere. Refresh before saving again."
                else -> (value as? JsonObject)?.text("error") ?: ((value as? JsonObject)?.get("error") as? JsonObject)?.text("message") ?: (value as? JsonObject)?.text("message") ?: "Work request failed (${response.status.value})."
            }
            throw WorkRequestError(response.status.value, message, value as? JsonObject)
        }
        return (value ?: throw WorkRequestError(0, "This work response could not be read.")) to response.headers["ETag"]
    }
    /** One GET with its status and JSON body, for callers that read a 404 as absent and word failures themselves (the Goals notes reads). */
    suspend fun getWithStatus(path: String): Pair<Int, JsonElement?> =
        try { 200 to request(path).first }
        catch (error: WorkRequestError) { if (error.status == 0) throw error; error.status to error.body }
    /** One JSON PATCH with its status and JSON body, for callers that word a 409 themselves (a goal's Mark done and Reopen). */
    suspend fun patchWithStatus(path: String, body: JsonObject): Pair<Int, JsonElement?> =
        try { 200 to request(path, HttpMethod.Patch, body).first }
        catch (error: WorkRequestError) { if (error.status == 0) throw error; error.status to error.body }
    suspend fun file(path: String, maximumBytes: Int = 128 * 1024 * 1024): ByteArray {
        require(path.startsWith("/api/magician/v2/"))
        require(maximumBytes in 1..128 * 1024 * 1024)
        return client.prepareGet(connection.baseUrl.trimEnd('/') + path) {
            connection.headers.forEach { (name, value) -> header(name, value) }
        }.execute { response ->
            if (!response.status.isSuccess()) throw WorkRequestError(response.status.value, "Artifact download failed (${response.status.value}).")
            if ((response.headers["Content-Length"]?.toLongOrNull() ?: 0) > maximumBytes) throw WorkRequestError(0, "Artifact is too large for this device.")
            val channel = response.bodyAsChannel()
            val buffer = ByteArray(minOf(8 * 1024, maximumBytes + 1))
            val output = ByteArrayOutputStream(minOf(8 * 1024, maximumBytes))
            var total = 0
            while (true) {
                // Read at most one byte beyond the cap; never buffer the rest.
                val count = channel.readAvailable(buffer, 0, minOf(buffer.size, maximumBytes - total + 1))
                if (count == -1) break
                if (count == 0) continue
                total += count
                if (total > maximumBytes) throw WorkRequestError(0, "Artifact is too large for this device.")
                output.write(buffer, 0, count)
            }
            output.toByteArray()
        }
    }
    suspend fun crew(): List<WorkCrewMember> {
        val value = request("/api/magician/v2/agents").first
        val rows = ((value as? JsonObject)?.get("agents") ?: value) as JsonArray
        return rows.map { WorkCrewMember.decode(it as JsonObject) }
    }
    suspend fun member(id: String): WorkCrewMember {
        val (value, etag) = request("/api/magician/v2/agents/${segment(id)}")
        return WorkCrewMember.decode(value as JsonObject, etag.orEmpty())
    }
    suspend fun patchMember(id: String, patch: JsonObject, etag: String): WorkCrewMember {
        require(etag.isNotBlank()) { "Refresh the crew member before saving changes." }
        val (value, header) = request("/api/magician/v2/agents/${segment(id)}", HttpMethod.Patch, patch, etag)
        return WorkCrewMember.decode(value as JsonObject, header.orEmpty())
    }
    enum class AgentAction(val path: String) { Pause("pause"), Resume("resume"), Trigger("trigger"), SetPrimary("set-primary") }
    suspend fun actOnMember(id: String, action: AgentAction) {
        request("/api/magician/v2/agents/${segment(id)}/${action.path}", HttpMethod.Post,
            if (action == AgentAction.Trigger) buildJsonObject {} else null)
    }
    suspend fun programs(): List<WorkProgramSummary> {
        val value = request("/api/magician/v2/programs").first as JsonObject
        return (value["programs"] as JsonArray).map { item ->
            val row = item as JsonObject
            WorkProgramSummary(requireNotNull(row.text("name")), requireNotNull(row.text("title")))
        }
    }
    suspend fun program(name: String): WorkProgramDocument =
        WorkProgramDocument.decode(request("/api/magician/v2/programs/${segment(name)}").first as JsonObject)
    /**
     * Owner save of a whole program document. [baseSha256] is the `sha256` of
     * the version that was opened for editing; a document that changed since
     * is refused with [WorkProgramChangedError] instead of being overwritten.
     * Only a signed-in owner session may save, and a refused credential
     * reports the server's own words.
     */
    suspend fun saveProgram(name: String, content: String, baseSha256: String): WorkProgramSaved {
        require(content.isNotBlank()) { "Write the document before saving." }
        require(baseSha256.isNotBlank()) { "Reload the program before saving changes." }
        val value = try {
            request("/api/magician/v2/programs/${segment(name)}", HttpMethod.Put,
                buildJsonObject { put("content", content); put("base_sha256", baseSha256) }).first
        } catch (error: WorkRequestError) {
            val body = error.body
            if (error.status == 409 && body?.text("error") == "program_changed") {
                throw WorkProgramChangedError(body.text("message")?.takeIf(String::isNotBlank) ?: "This program changed after you opened it.", body.text("current_sha256"))
            }
            throw body?.text("message")?.takeIf(String::isNotBlank)?.let { WorkRequestError(error.status, it, body) } ?: error
        }
        return WorkProgramSaved.decode(value as? JsonObject ?: throw WorkRequestError(0, "This work response could not be read."))
    }
    suspend fun restoreProgram(name: String, snapshot: String) {
        request("/api/magician/v2/programs/${segment(name)}/revert", HttpMethod.Post,
            buildJsonObject { put("snapshot", snapshot) })
    }
}
