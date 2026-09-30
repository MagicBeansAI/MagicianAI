package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicanAppLinks
import android.net.Uri
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

sealed interface TaskDeepLinkTarget {
    data class Task(val taskId: String) : TaskDeepLinkTarget
    data class Monitor(val taskId: String, val updateId: String? = null) : TaskDeepLinkTarget
}

/** One durable in-process handoff from Android intents into the Tasks workspace. */
object TaskDeepLinks {
    private val _target = MutableStateFlow<TaskDeepLinkTarget?>(null)
    val target: StateFlow<TaskDeepLinkTarget?> = _target.asStateFlow()

    fun accept(uri: Uri?): Boolean {
        val parsed = parse(uri?.toString()) ?: return false
        _target.value = parsed
        return true
    }

    /** In-app producers (Today, Monitor updates) use the same one-shot route as OS links. */
    fun request(target: TaskDeepLinkTarget) {
        _target.value = target
    }

    fun consume(target: TaskDeepLinkTarget) {
        if (_target.value == target) _target.value = null
    }

    internal fun parse(raw: String?): TaskDeepLinkTarget? {
        val uri = raw?.let { runCatching { java.net.URI(it) }.getOrNull() } ?: return null
        val query = runCatching { uri.rawQuery.orEmpty().split('&').filter(String::isNotBlank).associate { part ->
            val pieces = part.split('=', limit = 2)
            java.net.URLDecoder.decode(pieces[0], Charsets.UTF_8.name()) to
                java.net.URLDecoder.decode(pieces.getOrElse(1) { "" }, Charsets.UTF_8.name())
        } }.getOrElse { return null }
        if (MagicanAppLinks.isScheme(uri.scheme)) {
            val id = runCatching {
                java.net.URLDecoder.decode(uri.rawPath.trim('/').substringBefore('/'), Charsets.UTF_8.name())
            }.getOrNull()?.takeIf(String::isNotBlank) ?: return null
            return when (uri.host) {
                "task" -> TaskDeepLinkTarget.Task(id)
                "monitor" -> TaskDeepLinkTarget.Monitor(id, query["update"])
                else -> null
            }
        }
        if (uri.path?.trimEnd('/')?.endsWith("/tasks") == true) {
            val id = query["selected"]?.takeIf(String::isNotBlank) ?: return null
            return if (query["type"] == "monitors") {
                TaskDeepLinkTarget.Monitor(id, query["update"])
            } else TaskDeepLinkTarget.Task(id)
        }
        return null
    }
}

/** One-shot selection handed from Today into the eventual Attention workspace. */
object AttentionDeepLinks {
    private val _itemId = MutableStateFlow<String?>(null)
    val itemId: StateFlow<String?> = _itemId.asStateFlow()

    fun request(itemId: String?) { _itemId.value = itemId }
    fun consume(itemId: String) { if (_itemId.value == itemId) _itemId.value = null }
}

/** Canonical scoped link for an output row that did not carry a serving URL. */
internal object TaskArtifactLinks {
    fun url(baseUrl: String, taskId: String, relativePath: String): String? {
        val base = baseUrl.trim().trimEnd('/').takeIf(String::isNotEmpty) ?: return null
        val task = taskId.trim().takeIf(String::isNotEmpty) ?: return null
        val normalized = relativePath.trim().trimStart('/').removePrefix("outputs/")
        val path = normalized.split('/').filter { it.isNotBlank() && it !in setOf(".", "..") }
            .joinToString("/") { encodePathSegment(it) }
            .takeIf(String::isNotEmpty) ?: return null
        return "$base/api/magician/v3/tasks/${encodePathSegment(task)}/outputs/$path"
    }

    private fun encodePathSegment(value: String): String = java.net.URLEncoder
        .encode(value, Charsets.UTF_8.name())
        .replace("+", "%20")
        .replace("%2F", "/", ignoreCase = true)
}
