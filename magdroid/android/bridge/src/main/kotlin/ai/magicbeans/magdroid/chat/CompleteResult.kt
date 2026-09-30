package ai.magicbeans.magdroid.chat

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import java.net.URLEncoder

internal data class CompleteResultReadTarget(
    val path: String,
    val executionId: String?,
)

data class CompleteResult(
    val text: String,
    val contentHash: String,
)

@Serializable
internal data class CompleteResultReadRequest(
    @SerialName("result_ref") val resultRef: String,
    val cursor: String? = null,
    @SerialName("field_paths") val fieldPaths: List<String> = emptyList(),
    @SerialName("max_records") val maxRecords: Int = 100,
    @SerialName("execution_id") val executionId: String? = null,
)

@Serializable
internal data class CompleteResultReadEnvelope(
    val page: CompleteResultReadPage? = null,
    val error: String? = null,
)

@Serializable
internal data class CompleteResultReadPage(
    @SerialName("reconstruction_version") val reconstructionVersion: Int = 0,
    @SerialName("content_ref") val contentRef: String? = null,
    @SerialName("content_hash") val contentHash: String? = null,
    @SerialName("selection_paths") val selectionPaths: List<String> = listOf(""),
    val entries: List<JsonObject> = emptyList(),
    @SerialName("page_start") val pageStart: Int? = null,
    @SerialName("total_records") val totalRecords: Int? = null,
    @SerialName("total_entries") val totalEntries: Int? = null,
    @SerialName("next_cursor") val nextCursor: String? = null,
)

internal fun resolveCompleteResultReadTarget(
    owner: ActivityResultOwner?,
    hostSessionId: String?,
    legacyTaskId: String? = null,
    legacyExecutionId: String? = null,
): CompleteResultReadTarget? {
    when (owner?.kind) {
        "chat" -> owner.sessionId.nonEmpty()?.let {
            return CompleteResultReadTarget(
                path = "/api/magician/v2/chat/sessions/${encodePathSegment(it)}/results/read",
                executionId = null,
            )
        }
        "task" -> owner.taskId.nonEmpty()?.let {
            return CompleteResultReadTarget(
                path = "/api/magician/v3/tasks/${encodePathSegment(it)}/results/read",
                executionId = owner.executionId.nonEmpty(),
            )
        }
        "ephemeral_voice" -> return null
    }

    // Compatibility for persisted events written before canonical owner
    // metadata. Navigation ids are never preferred over an explicit owner.
    legacyTaskId.nonEmpty()?.let {
        return CompleteResultReadTarget(
            path = "/api/magician/v3/tasks/${encodePathSegment(it)}/results/read",
            executionId = legacyExecutionId.nonEmpty(),
        )
    }
    return hostSessionId.nonEmpty()?.let {
        CompleteResultReadTarget(
            path = "/api/magician/v2/chat/sessions/${encodePathSegment(it)}/results/read",
            executionId = null,
        )
    }
}

private fun String?.nonEmpty(): String? = this?.trim()?.takeIf(String::isNotEmpty)

private fun encodePathSegment(value: String): String =
    URLEncoder.encode(value, Charsets.UTF_8.name()).replace("+", "%20")

private data class FragmentAssembly(
    val nextByte: Int,
    val totalBytes: Int,
    val text: String,
)

@OptIn(ExperimentalSerializationApi::class)
private val completeResultPrettyJson = Json {
    prettyPrint = true
    prettyPrintIndent = "  "
}

private fun decodePointer(path: String): List<String> {
    if (path.isEmpty()) return emptyList()
    require(path.startsWith('/')) { "Complete result contained an invalid reconstruction path." }
    return path.drop(1).split('/').map { token ->
        var index = 0
        while (index < token.length) {
            if (token[index] == '~') {
                require(index + 1 < token.length && token[index + 1] in charArrayOf('0', '1')) {
                    "Complete result contained an invalid JSON-pointer escape."
                }
                index++
            }
            index++
        }
        token.replace("~1", "/").replace("~0", "~")
    }
}

private fun exactArrayIndex(token: String): Int? {
    if (!token.matches(Regex("^(0|[1-9][0-9]*)$"))) return null
    return token.toIntOrNull()?.takeIf { it >= 0 }
}

private fun assignAtPointer(
    current: JsonElement?,
    tokens: List<String>,
    value: JsonElement,
): JsonElement {
    if (tokens.isEmpty()) return value
    val token = tokens.first()
    val remainder = tokens.drop(1)
    val container = current ?: if (exactArrayIndex(token) == null) {
        JsonObject(emptyMap())
    } else {
        JsonArray(emptyList())
    }
    return when (container) {
        is JsonArray -> {
            val index = exactArrayIndex(token)
                ?: throw IllegalArgumentException("Complete result contained a non-numeric array path.")
            require(index <= container.size) {
                "Complete result array entries were missing or out of order."
            }
            val values = container.toMutableList()
            val child = values.getOrNull(index)?.takeUnless { it is JsonNull }
            val assigned = assignAtPointer(child, remainder, value)
            if (index == values.size) values.add(assigned) else values[index] = assigned
            JsonArray(values)
        }
        is JsonObject -> {
            val values = container.toMutableMap()
            values[token] = assignAtPointer(values[token], remainder, value)
            JsonObject(values)
        }
        else -> throw IllegalArgumentException(
            "Complete result reconstruction tried to descend through a scalar.",
        )
    }
}

private fun isPrefix(prefix: List<String>, value: List<String>): Boolean =
    prefix.size <= value.size && prefix.indices.all { prefix[it] == value[it] }

private fun assertCompatiblePath(
    path: String,
    assignedPaths: Map<String, String>,
    fragmentPaths: Set<String>,
    continuingFragment: Boolean = false,
) {
    val tokens = decodePointer(path)
    assignedPaths.forEach { (assignedPath, assignedKind) ->
        val assignedTokens = decodePointer(assignedPath)
        require(assignedPath != path) { "Complete result contained a duplicate reconstruction path." }
        require(!isPrefix(assignedTokens, tokens) || assignedKind == "container") {
            "Complete result contained overlapping scalar reconstruction paths."
        }
        require(!isPrefix(tokens, assignedTokens)) {
            "Complete result contained an out-of-order parent reconstruction path."
        }
    }
    fragmentPaths.forEach { fragmentPath ->
        if (fragmentPath == path && continuingFragment) return@forEach
        val fragmentTokens = decodePointer(fragmentPath)
        require(
            fragmentPath != path &&
                !isPrefix(fragmentTokens, tokens) &&
                !isPrefix(tokens, fragmentTokens),
        ) { "Complete result contained overlapping string-fragment reconstruction paths." }
    }
}

internal fun reconstructCompleteResult(entries: List<JsonObject>, version: Int): JsonElement {
    require(version == 0 || version == 1) {
        "Unsupported complete-result reconstruction version: $version"
    }
    var root: JsonElement? = null
    val fragments = mutableMapOf<String, FragmentAssembly>()
    val assignedPaths = mutableMapOf<String, String>()
    entries.forEach { entry ->
        val kind = entry.string("kind") ?: "complete_value"
        val fieldPath = entry.string("field_path") ?: ""
        val path = entry.string("reconstruction_path")
            ?: entry.int("source_index")?.let { "$fieldPath/$it" }
            ?: fieldPath
        if (kind == "string_fragment") {
            val text = entry["value"]?.let { it as? JsonPrimitive }
                ?.takeIf(JsonPrimitive::isString)?.contentOrNull
            val metadata = entry["string_fragment"] as? JsonObject
            require(version == 1 && text != null && metadata != null) {
                "Complete result contained an invalid string fragment."
            }
            val byteStart = metadata.int("byte_start")
            val byteEnd = metadata.int("byte_end")
            val totalBytes = metadata.int("total_bytes")
            require(byteStart != null && byteEnd != null && totalBytes != null) {
                "Complete result contained an invalid string fragment."
            }
            assertCompatiblePath(path, assignedPaths, fragments.keys, path in fragments)
            val state = fragments[path] ?: FragmentAssembly(0, totalBytes, "")
            val fragmentBytes = text.toByteArray(Charsets.UTF_8).size
            require(
                state.totalBytes == totalBytes && state.nextByte == byteStart &&
                    byteEnd == byteStart + fragmentBytes && byteEnd <= totalBytes,
            ) { "Complete result string fragments were missing, duplicated, or out of order." }
            val next = FragmentAssembly(byteEnd, totalBytes, state.text + text)
            if (next.nextByte == next.totalBytes) {
                root = assignAtPointer(root, decodePointer(path), JsonPrimitive(next.text))
                fragments.remove(path)
                assignedPaths[path] = "string_fragment"
            } else {
                fragments[path] = next
            }
            return@forEach
        }

        require(kind == "complete_value" || kind == "container") {
            "Complete result contained an unknown entry kind."
        }
        require(kind != "container" || version == 1) {
            "Legacy complete result unexpectedly contained a container entry."
        }
        require(entry.containsKey("value")) { "Complete result entry did not contain a value." }
        val value = entry["value"] ?: JsonNull
        if (kind == "container") {
            require((value is JsonObject && value.isEmpty()) || (value is JsonArray && value.isEmpty())) {
                "Complete result container entry was malformed."
            }
        }
        assertCompatiblePath(path, assignedPaths, fragments.keys)
        root = assignAtPointer(root, decodePointer(path), value)
        assignedPaths[path] = kind
    }
    require(fragments.isEmpty()) { "Complete result ended before all string fragments arrived." }
    return requireNotNull(root) { "Complete result did not contain any values." }
}

internal suspend fun readCompleteResult(
    resultRef: String,
    expectedContentHash: String?,
    readTarget: CompleteResultReadTarget,
    fetch: suspend (CompleteResultReadRequest) -> CompleteResultReadPage,
): CompleteResult {
    var cursor: String? = null
    val entries = mutableListOf<JsonObject>()
    var reconstructionVersion: Int? = null
    var contentHash = expectedContentHash.nonEmpty()
    var expectedPageStart = 0
    var expectedTotalEntries: Int? = null
    var expectedSelectionPaths: List<String>? = null
    val seenCursors = mutableSetOf<String>()

    repeat(256) { pageIndex ->
        val page = fetch(
            CompleteResultReadRequest(
                resultRef = resultRef,
                cursor = cursor,
                executionId = readTarget.executionId,
            ),
        )
        require(page.contentRef == null || page.contentRef == resultRef) {
            "Complete result response changed result identity between pages."
        }
        val pageHash = page.contentHash.nonEmpty()
            ?: throw IllegalArgumentException("Complete result response was missing its verified content hash.")
        require(contentHash == null || contentHash == pageHash) {
            "Complete result content hash changed between pages."
        }
        contentHash = pageHash
        require(reconstructionVersion == null || reconstructionVersion == page.reconstructionVersion) {
            "Complete result changed reconstruction version between pages."
        }
        reconstructionVersion = page.reconstructionVersion
        require(page.pageStart == expectedPageStart) {
            "Complete result pages were missing, duplicated, or out of order."
        }
        val total = page.totalEntries ?: page.totalRecords
        require(total != null && total >= 0) {
            "Complete result response contained an invalid entry count."
        }
        require(expectedTotalEntries == null || expectedTotalEntries == total) {
            "Complete result entry count changed between pages."
        }
        expectedTotalEntries = total
        require(expectedSelectionPaths == null || expectedSelectionPaths == page.selectionPaths) {
            "Complete result field selection changed between pages."
        }
        expectedSelectionPaths = page.selectionPaths
        require(cursor == null || page.entries.isNotEmpty()) {
            "Complete result cursor made no forward progress."
        }
        entries.addAll(page.entries)
        expectedPageStart += page.entries.size
        cursor = page.nextCursor.nonEmpty()
        if (cursor == null) {
            require(expectedTotalEntries == entries.size) {
                "Complete result ended before every entry was returned."
            }
            val value = reconstructCompleteResult(entries, reconstructionVersion ?: 0)
            val pretty = completeResultPrettyJson.encodeToString(JsonElement.serializer(), value)
            require(pretty.isNotEmpty()) { "Complete result decoded to empty text." }
            return CompleteResult(pretty, pageHash)
        }
        require(seenCursors.add(cursor!!)) { "Complete result repeated a page cursor." }
        require(pageIndex < 255) { "Complete result exceeded the safe page limit." }
    }
    throw IllegalArgumentException("Complete result exceeded the safe page limit.")
}

private fun JsonObject.string(key: String): String? =
    (this[key] as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.contentOrNull

private fun JsonObject.int(key: String): Int? =
    (this[key] as? JsonPrimitive)?.takeUnless(JsonPrimitive::isString)?.intOrNull
