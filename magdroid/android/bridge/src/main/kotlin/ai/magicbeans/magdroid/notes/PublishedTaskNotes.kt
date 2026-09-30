package ai.magicbeans.magdroid.notes

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.client.statement.bodyAsText
import ai.magicbeans.magdroid.net.CarriesFailure
import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.Failures
import io.ktor.http.isSuccess
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

val notesJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
}

/**
 * A task whose write-up has been published to the notes store.
 *
 * The note is a Markdown file in the Magician notes space. Opening it uses
 * that file path, not a separate notes server.
 */
@Serializable
data class PublishedTaskNote(
    @SerialName("projection_id") val projectionId: String = "",
    @SerialName("task_id") val taskId: String = "",
    val title: String = "",
    val status: String = "",
    @SerialName("agent_id") val agentId: String = "",
    val mode: String = "",
    @SerialName("task_completed_at") val taskCompletedAt: String? = null,
    @SerialName("source_updated_at") val sourceUpdatedAt: String = "",
    @SerialName("published_at") val publishedAt: String = "",
    val tags: List<String> = emptyList(),
    @SerialName("note_path") val notePath: String = "",
    @SerialName("open_url") val openUrl: String? = null,
) {
    val id: String get() = projectionId

    /** Only a note with a file path can be opened in the notes browser. */
    val isOpenable: Boolean get() = notePath.isNotBlank()
}

@Serializable
data class PublishedTaskNotePage(
    val items: List<PublishedTaskNote> = emptyList(),
    val offset: Int = 0,
    val limit: Int = 0,
    val total: Int = 0,
    @SerialName("has_more") val hasMore: Boolean = false,
)

/** Named once, so every sentence about this read reads alike. */
private const val NOTES = "published notes"

/** Why a notes read failed, classified so the section can offer Retry. */
class NotesError(override val failure: Failure) :
    Exception(failure.headline), CarriesFailure {

    constructor(message: String) : this(
        Failure(FailureKind.Unknown, message, "", retryable = true),
    )
}

/** Reads `GET /notes/published-tasks`. */
class PublishedTaskNotesRepository(context: Context) {

    private val app = context.applicationContext

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 30_000
            connectTimeoutMillis = 20_000
        }
    }

    suspend fun page(offset: Int = 0, limit: Int = PAGE, query: String = ""): PublishedTaskNotePage {
        val response = client.get("${base()}/notes/published-tasks") {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
            parameter("offset", offset)
            parameter("limit", limit)
            // `q` is what `TaskNotesListQuery` reads (and what iOS and the web
            // send). `query` was silently ignored, so a search returned
            // every note.
            query.trim().takeIf { it.isNotEmpty() }?.let { parameter("q", it) }
        }
        if (!response.status.isSuccess()) {
            throw NotesError(Failures.ofStatus(response.status.value, NOTES))
        }
        return runCatching {
            notesJson.decodeFromString(PublishedTaskNotePage.serializer(), response.bodyAsText())
        }.getOrElse { throw NotesError(Failures.garbled(NOTES)) }
    }

    /** `POST /notes/publish/tasks/backfill` — publish the next batch of completed tasks. */
    suspend fun backfill(limit: Int = BACKFILL_BATCH): PublishedTaskNotesBackfillReceipt {
        val response = client.post("${base()}/notes/publish/tasks/backfill") {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
            contentType(ContentType.Application.Json)
            setBody(backfillRequestBody(limit))
        }
        if (!response.status.isSuccess()) {
            throw NotesError(Failures.ofStatus(response.status.value, "publishing notes"))
        }
        return runCatching {
            notesJson.decodeFromString(PublishedTaskNotesBackfillReceipt.serializer(), response.bodyAsText())
        }.getOrElse { throw NotesError(Failures.garbled("publishing notes")) }
    }

    /** `POST /notes/published-tasks/{taskId}/promote-memory` — a memory candidate for review. */
    suspend fun promoteToMemory(taskId: String): PublishedTaskNotePromotionReceipt {
        val id = taskId.trim()
        if (id.isEmpty()) throw NotesError("This note has no task to promote.")
        val response = client.post(
            "${base()}/notes/published-tasks/${java.net.URLEncoder.encode(id, Charsets.UTF_8.name()).replace("+", "%20")}/promote-memory",
        ) {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
            contentType(ContentType.Application.Json)
            setBody("{}")
        }
        if (!response.status.isSuccess()) {
            throw NotesError(Failures.ofStatus(response.status.value, "promoting the note"))
        }
        return runCatching {
            notesJson.decodeFromString(PublishedTaskNotePromotionReceipt.serializer(), response.bodyAsText())
        }.getOrElse { throw NotesError(Failures.garbled("promoting the note")) }
    }

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isEmpty()) throw NotesError("No Magician host configured yet.")
        return "$host/api/magician/v2"
    }

    fun close() = client.close()

    companion object {
        const val PAGE = 10
        const val BACKFILL_BATCH = 25
    }
}
