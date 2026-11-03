package ai.magicbeans.magdroid.chat

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import java.net.URLEncoder
import java.util.concurrent.ConcurrentHashMap

/** One thread as the app names it: the internal [id] never changes; its URL [slug] follows its [name]. */
data class ThreadEntry(val id: String, val name: String, val slug: String?)

/**
 * The threads this machine has shown the app, for two jobs:
 *
 * - a link's `/t/<key>` may be the thread's current slug, an old slug or its
 *   id; [resolve] maps it to the canonical id (cached; one
 *   `GET /api/magician/v2/ui-threads/{key}` on a miss; the key itself when that
 *   fails), and every local lookup keeps using the id;
 * - a thread chip shows the thread's name ([label]), not its id.
 *
 * Scoped to one paired machine: [useOrigin] forgets everything on a switch.
 */
object ThreadDirectory {
    private val _entries = MutableStateFlow<Map<String, ThreadEntry>>(emptyMap())
    /** Known threads by id. */
    val entries: StateFlow<Map<String, ThreadEntry>> = _entries.asStateFlow()
    private val aliases = ConcurrentHashMap<String, String>()
    @Volatile private var origin: String? = null

    fun useOrigin(value: String?) {
        if (value == origin) return
        origin = value
        clear()
    }

    fun clear() {
        aliases.clear()
        _entries.value = emptyMap()
    }

    fun remember(threads: Iterable<UiThreadRecord>) {
        val known = threads.filter { it.id.isNotBlank() }
        if (known.isEmpty()) return
        known.forEach { thread -> thread.slug?.takeIf(String::isNotBlank)?.let { aliases[it] = thread.id } }
        _entries.value = _entries.value + known.associate { it.id to ThreadEntry(it.id, it.name, it.slug) }
    }

    fun rememberAlias(key: String, id: String) {
        if (key.isNotBlank() && id.isNotBlank()) aliases[key] = id
    }

    /** The canonical id for [key] when already known: an id, then a slug or alias. */
    fun canonicalId(key: String): String? {
        val entries = _entries.value
        if (key in entries) return key
        return aliases[key] ?: entries.values.firstOrNull { it.slug == key }?.id
    }

    /** `#<name>`, or `#<id>` for a thread the app has not listed. */
    fun label(id: String?, entries: Map<String, ThreadEntry> = _entries.value): String {
        val key = id?.trim().orEmpty().ifEmpty { "general" }
        val name = entries[key]?.name?.trim().orEmpty()
        return "#" + name.ifEmpty { key }
    }

    /** The thread in a `{thread: {...}}` envelope or a bare record; null when it names none. */
    fun threadRecordOf(element: JsonElement): UiThreadRecord? {
        val root = element as? JsonObject ?: return null
        val thread = root["thread"] as? JsonObject ?: root
        return runCatching { chatJson.decodeFromJsonElement(UiThreadRecord.serializer(), thread) }
            .getOrNull()?.takeIf { it.id.isNotBlank() }
    }

    /**
     * The canonical id for a link's thread [key]. [fetch] reads a v2 API path
     * (`/api/magician/v2/ui-threads/{key}`); a failed read leaves the key as the id.
     */
    suspend fun resolve(key: String, fetch: suspend (String) -> JsonElement): String {
        val clean = key.trim()
        if (clean.isEmpty()) return key
        canonicalId(clean)?.let { return it }
        return try {
            val path = "/api/magician/v2/ui-threads/" + URLEncoder.encode(clean, Charsets.UTF_8.name()).replace("+", "%20")
            val thread = threadRecordOf(fetch(path)) ?: return clean
            remember(listOf(thread))
            rememberAlias(clean, thread.id)
            thread.id
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            clean
        }
    }
}
