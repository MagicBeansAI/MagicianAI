package ai.magicbeans.magdroid.preferences

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.patch
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.encodeURLPathPart
import io.ktor.http.isSuccess
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

/**
 * The owner's preferences, exactly as preference checks see them, and their
 * history — the Android port of the web's `preferencesApi.ts` and iOS's
 * `OwnerPreferences` (docs/components/magician/preference-alignment.md, "Owner
 * view and edits", "Preference history").
 *
 * - `GET /preference-alignment/intentions?surface=android`: the view.
 * - `POST` / `PATCH` / `DELETE /preference-alignment/intentions[/{id}]`: add,
 *   edit, confirm, retire and forget. Every edit but add names the revision
 *   this screen showed; a changed preference answers `stale` with its current
 *   one, and nothing is saved.
 * - `GET /preference-alignment/history` and `/intentions/{id}/history`.
 *
 * Android is an owner-private surface since 2026-10-11 (owner decision).
 */
enum class PreferenceKind(val wire: String, val label: String) {
    StandingInstruction("standing_instruction", "Instruction"),
    Goal("goal", "Goal"),
    Taste("taste", "Taste");

    companion object {
        fun of(wire: String?): PreferenceKind = entries.firstOrNull { it.wire == wire } ?: Taste
    }
}

data class OwnerPreference(
    val id: String,
    val revision: String,
    val kind: PreferenceKind,
    /** `direct_owner_statement`, `confirmed`, `inferred`, … */
    val status: String,
    val statement: String,
    val conditions: String?,
    val lifecycle: String,
    /** The memory tier, or another source name. */
    val source: String,
    val editable: Boolean,
    val lastCheckedAtMs: Long?,
) {
    /** Where it came from, in the owner's words. */
    val statusLabel: String
        get() = when (status) {
            "direct_owner_statement" -> "You said it"
            "confirmed" -> "You confirmed it"
            "inferred" -> "Magician noticed it"
            "inferred_untrusted" -> "Someone else said it"
            else -> "Source unknown"
        }

    /** An inferred preference can be confirmed as the owner's own. */
    val confirmable: Boolean get() = editable && status in setOf("inferred", "inferred_untrusted", "unknown_provenance")

    /** The statement shaped for reading, or `null` for prose. */
    val presented: PresentedStatement? get() = presentStatement(statement)

    /** Waiting on the owner to say which of two statements holds. */
    val unresolved: Boolean get() = lifecycle == "unresolved"
}

data class RetiredPreference(val id: String, val statement: String, val lifecycle: String, val supersededBy: String?)

data class PreferenceView(
    val preferences: List<OwnerPreference>,
    val retired: List<RetiredPreference>,
    /** Possibly in force but not readable here: the reason codes. */
    val omitted: Int,
    val coverage: String,
)

data class PreferenceHistoryEntry(
    val atMs: Long,
    val recordId: String,
    val op: String,
    val via: String,
    val surface: String?,
    val wordingBefore: String?,
    val wordingAfter: String?,
    val wordingErased: Boolean,
) {
    val opLabel: String
        get() = OP_WORDS[op] ?: op

    /** Who changed it and from where, e.g. "you on Android". */
    val whoLabel: String
        get() {
            val via = VIA_WORDS[via] ?: via
            val where = when (surface) {
                null -> return via
                "ios" -> "iPhone"
                "android" -> "Android"
                else -> surface
            }
            return "$via on $where"
        }

    companion object {
        private val OP_WORDS = mapOf(
            "added" to "Added", "edited" to "Reworded", "confirmed" to "Confirmed",
            "superseded" to "Replaced", "retired" to "Retired", "restored" to "Restored",
            "scope_changed" to "Scope changed", "forgotten" to "Forgotten",
        )
        private val VIA_WORDS = mapOf(
            "owner_surface" to "you", "memory_tool" to "chat", "consolidator" to "memory review",
            "learning" to "learning", "alignment" to "a preference check", "lifecycle" to "memory upkeep",
            "memory_api" to "the memory page", "backfill" to "a migration", "taste_review" to "taste review",
            "unknown" to "unknown",
        )
    }
}

/**
 * A stored statement shaped for reading. Memory sometimes stores a preference
 * as `key: {json}`; shown raw it is a blob. Display only: editing, checks and
 * history keep the stored wording. The same rule as the web's and iOS's
 * `presentStatement` (fields sorted by key on every client).
 */
data class PresentedStatement(val title: String, val fields: List<Pair<String, String>>)

private val ACRONYMS = setOf("ui", "api", "url", "llm", "id", "pdf", "sms", "ai")
private val STRUCTURED = Regex("""^\s*([A-Za-z][\w.]*)\s*:\s*(\{.*\})\s*$""", RegexOption.DOT_MATCHES_ALL)

internal fun humanizeKey(key: String): String {
    val text = key.replace(Regex("[._]+"), " ").trim().split(Regex("\\s+"))
        .joinToString(" ") { if (it.lowercase() in ACRONYMS) it.uppercase() else it }
    return text.replaceFirstChar { it.uppercase() }
}

/** `null` when the statement is ordinary prose. */
fun presentStatement(statement: String): PresentedStatement? {
    val match = STRUCTURED.matchEntire(statement) ?: return null
    val fields = runCatching { Json.parseToJsonElement(match.groupValues[2]) }.getOrNull() as? JsonObject
        ?: return null
    return PresentedStatement(
        title = humanizeKey(match.groupValues[1]),
        fields = fields.entries.sortedBy { it.key }.map { (key, value) ->
            humanizeKey(key) to ((value as? JsonPrimitive)?.takeIf { it.isString }?.content ?: value.toString())
        },
    )
}

/** The result of one change. */
sealed interface PreferenceEditOutcome {
    data class Applied(val recordId: String) : PreferenceEditOutcome

    /** The preference changed since this screen showed it; nothing was saved. */
    data class Stale(val currentRevision: String, val message: String) : PreferenceEditOutcome
}

class PreferencesError(message: String) : Exception(message)

private fun JsonElement?.text(): String? =
    (this as? JsonPrimitive)?.takeIf { it.isString }?.content

private fun JsonElement?.objects(): List<JsonObject> = (this as? JsonArray).orEmpty().mapNotNull { it as? JsonObject }

private fun sourceName(source: JsonElement?): String = when (source) {
    is JsonObject -> source.entries.firstOrNull()?.let { (variant, detail) ->
        (detail as? JsonObject)?.get("tier").text() ?: variant.replace('_', ' ')
    } ?: "unknown"
    is JsonPrimitive -> source.content.replace('_', ' ')
    else -> "unknown"
}

/** `GET /intentions` body, read leniently: a malformed row is skipped. */
fun parsePreferenceView(body: String): PreferenceView {
    val root = runCatching { Json.parseToJsonElement(body) }.getOrNull() as? JsonObject
        ?: throw PreferencesError("Your preferences could not be read.")
    return PreferenceView(
        preferences = root["intentions"].objects().mapNotNull { row ->
            val id = row["id"].text() ?: return@mapNotNull null
            OwnerPreference(
                id = id,
                revision = row["revision"].text().orEmpty(),
                kind = PreferenceKind.of(row["kind"].text()),
                status = row["assertion_status"].text() ?: "inferred",
                statement = row["statement"].text().orEmpty(),
                conditions = row["conditions"].text(),
                lifecycle = row["lifecycle"].text() ?: "active",
                source = sourceName(row["source"]),
                editable = (row["editable"] as? JsonPrimitive)?.booleanOrNull == true,
                lastCheckedAtMs = (row["last_checked_at_ms"] as? JsonPrimitive)?.longOrNull,
            )
        },
        retired = root["retired"].objects().mapNotNull { row ->
            val id = row["id"].text() ?: return@mapNotNull null
            RetiredPreference(id, row["statement"].text().orEmpty(), row["lifecycle"].text() ?: "retired", row["superseded_by"].text())
        },
        omitted = root["omitted"].objects().size,
        coverage = (root["coverage"] as? JsonObject)?.get("status").text() ?: "complete",
    )
}

/** `GET /history` body, newest first as the server sends it. */
fun parsePreferenceHistory(body: String): List<PreferenceHistoryEntry> {
    val root = runCatching { Json.parseToJsonElement(body) }.getOrNull() as? JsonObject
        ?: throw PreferencesError("The history could not be read.")
    return root["entries"].objects().map { entry ->
        val actor = entry["actor"] as? JsonObject
        PreferenceHistoryEntry(
            atMs = (entry["at_ms"] as? JsonPrimitive)?.longOrNull ?: 0,
            recordId = entry["record_id"].text().orEmpty(),
            op = entry["op"].text() ?: "changed",
            via = actor?.get("via").text() ?: "unknown",
            surface = actor?.get("surface").text(),
            wordingBefore = entry["wording_before"].text(),
            wordingAfter = entry["wording_after"].text(),
            wordingErased = (entry["wording_erased"] as? JsonPrimitive)?.booleanOrNull == true,
        )
    }
}

/** An edit's reply: applied, stale (409 `stale_preference`), or an error. */
fun parseEditOutcome(status: Int, body: String): PreferenceEditOutcome {
    val reply = runCatching { Json.parseToJsonElement(body) }.getOrNull() as? JsonObject
    if (status == 409 && reply?.get("error").text() == "stale_preference") {
        return PreferenceEditOutcome.Stale(
            currentRevision = reply?.get("current_revision").text().orEmpty(),
            message = reply?.get("message").text() ?: "This preference changed since you opened it.",
        )
    }
    if (status !in 200..299) {
        throw PreferencesError(reply?.get("message").text() ?: "The change was not saved.")
    }
    return PreferenceEditOutcome.Applied(reply?.get("record_id").text().orEmpty())
}

class PreferencesRepository(context: Context) {

    private val app = context.applicationContext

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 30_000
            connectTimeoutMillis = 20_000
        }
    }

    suspend fun view(): PreferenceView {
        val response = client.get("${base()}/intentions") { authorized() }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw PreferencesError(message(body, "Your preferences are unavailable."))
        return parsePreferenceView(body)
    }

    suspend fun history(id: String? = null): List<PreferenceHistoryEntry> {
        val path = id?.let { "${base()}/intentions/${it.encodeURLPathPart()}/history" } ?: "${base()}/history"
        val response = client.get(path) { authorized() }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw PreferencesError(message(body, "The history is unavailable."))
        return parsePreferenceHistory(body)
    }

    suspend fun add(kind: PreferenceKind, wording: String, conditions: String? = null): PreferenceEditOutcome =
        outcome(
            client.post("${base()}/intentions") {
                authorized()
                contentType(ContentType.Application.Json)
                setBody(
                    buildJsonObject {
                        put("kind", kind.wire)
                        put("wording", wording.trim())
                        conditions?.trim()?.takeIf { it.isNotEmpty() }?.let { put("conditions", it) }
                    }.toString(),
                )
            },
        )

    suspend fun edit(id: String, expectedRevision: String, wording: String) =
        patch(id, buildJsonObject { put("action", "edit"); put("expected_revision", expectedRevision); put("wording", wording.trim()) })

    suspend fun confirm(id: String, expectedRevision: String) =
        patch(id, buildJsonObject { put("action", "confirm"); put("expected_revision", expectedRevision) })

    suspend fun retire(id: String, expectedRevision: String) =
        patch(id, buildJsonObject { put("action", "retire"); put("expected_revision", expectedRevision) })

    /** Forget: the preference is deleted and its wording erased from the history. */
    suspend fun forget(id: String, expectedRevision: String): PreferenceEditOutcome =
        outcome(
            client.delete("${base()}/intentions/${id.encodeURLPathPart()}") {
                authorized()
                parameter("expected_revision", expectedRevision)
            },
        )

    private suspend fun patch(id: String, body: JsonObject): PreferenceEditOutcome =
        outcome(
            client.patch("${base()}/intentions/${id.encodeURLPathPart()}") {
                authorized()
                contentType(ContentType.Application.Json)
                setBody(body.toString())
            },
        )

    private suspend fun outcome(response: HttpResponse) = parseEditOutcome(response.status.value, response.bodyAsText())

    private fun io.ktor.client.request.HttpRequestBuilder.authorized() {
        MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
        parameter("surface", "android")
    }

    private fun message(body: String, fallback: String): String =
        (runCatching { Json.parseToJsonElement(body) }.getOrNull() as? JsonObject)?.get("message").text() ?: fallback

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isEmpty()) throw PreferencesError("No Magician host configured yet.")
        return "$host/api/magician/v2/preference-alignment"
    }

    fun close() = client.close()
}
