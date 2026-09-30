package ai.magicbeans.magdroid.notes

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import java.net.URLEncoder
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
data class NoteTreeEntry(
    val name: String,
    @SerialName("relative_path") val relativePath: String,
    val kind: String,
    @SerialName("has_children") val hasChildren: Boolean? = null,
)

@Serializable
data class NoteTreePage(val entries: List<NoteTreeEntry> = emptyList())

@Serializable
data class NoteDocument(
    @SerialName("relative_path") val relativePath: String,
    val title: String = "",
    val markdown: String = "",
)

@Serializable
private data class CreateNoteBody(val folder: String, val name: String)

@Serializable
private data class SaveNoteBody(val path: String, val markdown: String)

@Serializable
private data class SearchNoteBody(val query: String, val limit: Int = 20)

@Serializable
data class NoteSearchMatch(val line: Int = 0, val text: String = "")

@Serializable
data class NoteSearchHit(
    val title: String = "",
    @SerialName("relative_path") val relativePath: String = "",
    val matches: List<NoteSearchMatch> = emptyList(),
)

@Serializable
data class NoteSearchResults(
    val hits: List<NoteSearchHit> = emptyList(),
    @SerialName("query_terms") val queryTerms: List<String> = emptyList(),
    @SerialName("more_available") val moreAvailable: Boolean = false,
)

class NotesLibraryException(message: String) : Exception(message)

class NotesLibraryRepository(context: Context) {
    private val app = context.applicationContext
    private val client = HttpClient(CIO) {
        followRedirects = false
        install(HttpTimeout) {
            requestTimeoutMillis = 20_000
            connectTimeoutMillis = 10_000
        }
    }
    private val json = Json { ignoreUnknownKeys = true }

    suspend fun tree(folder: String): List<NoteTreeEntry> {
        val response = client.get("${base()}/notes/tree") {
            authorize()
            parameter("path", folder)
        }
        if (!response.status.isSuccess()) throw NotesLibraryException("Could not list that folder.")
        return json.decodeFromString(NoteTreePage.serializer(), response.bodyAsText()).entries
    }

    suspend fun open(path: String): NoteDocument {
        val response = client.get("${base()}/notes/file") {
            authorize()
            parameter("path", path)
        }
        if (!response.status.isSuccess()) throw NotesLibraryException("Could not open that note.")
        return json.decodeFromString(NoteDocument.serializer(), response.bodyAsText())
    }

    suspend fun createFolder(folder: String, name: String): String {
        val response = client.post("${base()}/notes/tree") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(json.encodeToString(CreateNoteBody.serializer(), CreateNoteBody(folder, name)))
        }
        if (!response.status.isSuccess()) throw NotesLibraryException("Could not create that folder.")
        return json.decodeFromString(NoteDocument.serializer(), response.bodyAsText()).relativePath
    }

    suspend fun create(folder: String, name: String): NoteDocument {
        val response = client.post("${base()}/notes/file") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(json.encodeToString(CreateNoteBody.serializer(), CreateNoteBody(folder, name)))
        }
        if (!response.status.isSuccess()) throw NotesLibraryException("Could not create that note.")
        return json.decodeFromString(NoteDocument.serializer(), response.bodyAsText())
    }

    suspend fun save(path: String, markdown: String): NoteDocument {
        val response = client.put("${base()}/notes/file") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(json.encodeToString(SaveNoteBody.serializer(), SaveNoteBody(path, markdown)))
        }
        if (!response.status.isSuccess()) throw NotesLibraryException("Could not save that note.")
        return json.decodeFromString(NoteDocument.serializer(), response.bodyAsText())
    }

    suspend fun search(query: String): NoteSearchResults {
        val response = client.post("${base()}/notes/search") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(json.encodeToString(SearchNoteBody.serializer(), SearchNoteBody(query)))
        }
        if (!response.status.isSuccess()) throw NotesLibraryException("Could not search notes.")
        return json.decodeFromString(NoteSearchResults.serializer(), response.bodyAsText())
    }

    suspend fun deleteFile(path: String) = delete("file", path, "Could not delete that note.")

    suspend fun deleteFolder(path: String) = delete("tree", path, "Could not delete that folder.")

    fun close() = client.close()

    private suspend fun delete(kind: String, path: String, failure: String) {
        val encoded = URLEncoder.encode(path, Charsets.UTF_8.name())
        val response = client.delete("${base()}/notes/$kind?path=$encoded") { authorize() }
        if (response.status.value != 204 && !response.status.isSuccess()) {
            throw NotesLibraryException(failure)
        }
    }

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isBlank()) throw NotesLibraryException("Connect to Magician before opening Notes.")
        return "$host/api/magician/v2"
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorize() {
        val current = MagicianAccess.connectionSnapshot(app)
            ?: throw NotesLibraryException("Connect to Magician before opening Notes.")
        current.headers.forEach { (name, value) -> header(name, value) }
    }
}
