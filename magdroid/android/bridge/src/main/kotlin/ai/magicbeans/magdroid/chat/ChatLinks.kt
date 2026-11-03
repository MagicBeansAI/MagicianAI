package ai.magicbeans.magdroid.chat

import java.io.ByteArrayOutputStream
import java.net.URI
import java.net.URLEncoder

/**
 * What a link in a chat reply points at. The Kotlin twin of web `classifyRef`
 * (ui/unified-ui/src/lib/magician/links/classifyRef.ts) and iOS
 * `classifyActivityLink`; the app's chat link opener turns it into an action.
 */
sealed interface ChatLinkTarget {
    /** http(s), mailto, tel, sms or `magican://` — the system opens it. */
    data class External(val url: String) : ChatLinkTarget

    /** A file in a task's outputs, served by the task output API. */
    data class TaskOutput(val taskId: String, val relativePath: String) : ChatLinkTarget {
        /** `/api/magician/v3/tasks/{id}/outputs/{rel}`, each segment encoded. */
        val apiPath: String get() = ChatLinks.taskOutputPath(taskId, relativePath)
        val filename: String get() = relativePath.substringAfterLast('/')
    }

    /** Another `/api/…` resource on the paired machine (needs the device bearer). */
    data class ServerResource(val path: String) : ChatLinkTarget

    /**
     * A file on the paired machine — an absolute path, `file://`, or a
     * workspace-relative mention — opened there through the chat session's
     * open-file endpoint, which checks it against the session scope.
     */
    data class HostFile(val path: String) : ChatLinkTarget

    /** An app route (`/t/…`, `/tasks`, `/work?selected=…`, `/chat`, …), relative. */
    data class AppRoute(val route: String) : ChatLinkTarget

    /** Not openable: an unknown scheme, a protocol-relative escape, prose. */
    data class Unknown(val raw: String) : ChatLinkTarget
}

/** Where an app route (a [ChatLinkTarget.AppRoute]) lands in the app. */
sealed interface ChatAppDestination {
    /**
     * A chat conversation: [sessionId] when the link names one, else the
     * thread's latest conversation, else (both null) just the chat tab.
     */
    data class Chat(val sessionId: String?, val threadId: String?) : ChatAppDestination

    /** The History list of conversations and threads. */
    data object History : ChatAppDestination

    /** A Work/task route, canonical for the task and Work deep-link parsers. */
    data class Work(val route: String) : ChatAppDestination
    data class Attention(val itemId: String?) : ChatAppDestination
    data object Today : ChatAppDestination
    data class Observe(val route: String) : ChatAppDestination
    data object Settings : ChatAppDestination
    data object Notes : ChatAppDestination
    data object VibeDev : ChatAppDestination

    /** No native screen: the machine's web page. */
    data class Web(val route: String) : ChatAppDestination
}

object ChatLinks {
    private val taskId = Regex("^task_[0-9a-fA-F]{32}$")
    private val executionId = Regex("^exec_[0-9a-fA-F]{32}$")
    private val executionOutput = Regex("(?:^|/)tasks/(task_[0-9a-fA-F]{32})/(?:outputs/)?(executions/[^/]+/outputs/.+)$")
    private val taskOutput = Regex("(?:^|/)tasks/(task_[0-9a-fA-F]{32})/(?:.*?/)?outputs/(.+)$")
    private val scheme = Regex("^[A-Za-z][A-Za-z0-9+.\\-]*:")
    private val fileExtension = Regex("\\.[A-Za-z0-9]{1,12}$")
    private val externalSchemes = listOf("mailto:", "tel:", "sms:", "magican:")

    /** App pages that are never file paths, even when a segment has a dot. */
    private val appRoutes = listOf(
        "/t", "/tasks", "/work", "/notes", "/chat", "/attention", "/today", "/settings", "/observe",
        "/crew", "/agents", "/skills", "/briefing", "/briefings", "/dashboard", "/debug", "/vibedev",
        "/harness", "/relentless", "/memory", "/ops",
    )
    private val fileRoots = listOf("/Users/", "/Volumes/", "/tmp/", "/private/", "/var/folders/", "/home/")

    /**
     * Classifies [raw]. With [origin] (the paired machine's `scheme://host[:port]`),
     * an absolute link on that machine is read by its path, so
     * `https://<machine>/api/…` still gets the device bearer.
     */
    fun classify(raw: String?, origin: String? = null): ChatLinkTarget {
        val text = raw?.trim().orEmpty()
        if (text.isEmpty()) return ChatLinkTarget.Unknown("")
        val lower = text.lowercase()

        if (lower.startsWith("file:")) {
            val rest = text.substring(5).trimStart('/')
            if (rest.isEmpty()) return ChatLinkTarget.Unknown(text)
            val path = percentDecoded("/$rest")
            return taskOutputOf(path) ?: ChatLinkTarget.HostFile(path)
        }
        if (lower.startsWith("http:") || lower.startsWith("https:")) {
            if (origin != null) sameOriginReference(text, origin)?.let { return classify(it) }
            return ChatLinkTarget.External(text)
        }
        if (externalSchemes.any { lower.startsWith(it) }) return ChatLinkTarget.External(text)
        if (taskId.matches(text)) return ChatLinkTarget.AppRoute("/work?filter=all&selected=$text")
        if (executionId.matches(text)) return ChatLinkTarget.AppRoute("/debug?execution_id=$text")

        val rawPath = text.substringBefore('?').substringBefore('#')
        val path = percentDecoded(rawPath)
        taskOutputOf(path)?.let { return it }

        if (text.startsWith("//") || text.contains("://") || scheme.containsMatchIn(text)) {
            return ChatLinkTarget.Unknown(text)
        }
        if (path.startsWith("/api/")) return ChatLinkTarget.ServerResource(text)
        if (path.startsWith("/")) {
            if (appRoutes.any { path == it || path.startsWith("$it/") }) return ChatLinkTarget.AppRoute(text)
            if (fileRoots.any { path.startsWith(it) } || path.contains("/magician_data_v3/") || hasFileExtension(path)) {
                return ChatLinkTarget.HostFile(path)
            }
            return ChatLinkTarget.AppRoute(text)
        }
        // Workspace-relative file mention (`Pages/report.md`, `Chapter%20Map.md`):
        // the session's scoped resolver opens it, like web `filePath`.
        ChatLocalState.outputPath(text)?.takeIf { hasFileExtension(it) }?.let { return ChatLinkTarget.HostFile(it) }
        return ChatLinkTarget.Unknown(text)
    }

    /**
     * The in-app destination of a relative app route — every web route form a
     * chat reply can carry (ui/unified-ui/src/routes): `/chat[?session=&thread=]`,
     * `/t/<thread>[/chat|settings|files|models][?session=]`, `/t/<thread>?selected=`,
     * `/t/<thread>/tasks[/<id>]`, `/work…`, `/tasks…`, `/crew…`, `/harness`,
     * `/relentless`, `/history`, `/attention`, `/today`, `/observe`, `/settings`,
     * `/notes`, `/vibe`. Anything else is [ChatAppDestination.Web].
     */
    fun appDestination(route: String): ChatAppDestination {
        val path = route.substringBefore('?').substringBefore('#').trimEnd('/').ifEmpty { "/" }
        val query = queryOf(route)
        fun q(name: String) = query[name]?.trim()?.takeIf(String::isNotEmpty)
        fun under(prefix: String) = path == prefix || path.startsWith("$prefix/")
        val segments = path.split('/').filter(String::isNotEmpty).map { percentDecoded(it) }
        return when {
            segments.firstOrNull() == "t" && segments.size >= 2 -> {
                val thread = segments[1]
                val tab = segments.getOrNull(2)
                when {
                    thread.equals("vibedev", ignoreCase = true) -> ChatAppDestination.VibeDev
                    tab == "tasks" -> ChatAppDestination.Work(route)
                    tab == null && q("selected") != null ->
                        ChatAppDestination.Work("/t/${encodeSegment(thread)}/tasks/${encodeSegment(q("selected")!!)}")
                    tab == null || tab in listOf("chat", "settings", "files", "models") ->
                        ChatAppDestination.Chat(q("session"), thread)
                    else -> ChatAppDestination.Web(route)
                }
            }
            under("/chat") -> ChatAppDestination.Chat(q("session"), q("thread"))
            path == "/history" -> ChatAppDestination.History
            listOf("/work", "/tasks", "/crew", "/harness", "/relentless").any { under(it) } -> ChatAppDestination.Work(route)
            under("/attention") -> ChatAppDestination.Attention(q("item_id") ?: q("attention_id") ?: q("selected") ?: q("item"))
            path == "/today" -> ChatAppDestination.Today
            under("/observe") -> ChatAppDestination.Observe(route)
            under("/settings") -> ChatAppDestination.Settings
            path == "/notes" -> ChatAppDestination.Notes
            listOf("/vibe", "/vibedev", "/coding-runs").any { under(it) } -> ChatAppDestination.VibeDev
            else -> ChatAppDestination.Web(route)
        }
    }

    /**
     * The thread key of a `/t/<key>…` route, decoded — the thread's slug, an
     * old slug or its id ([ThreadDirectory.resolve] maps it); null otherwise.
     */
    fun threadRouteKey(route: String): String? {
        val segments = route.substringBefore('?').substringBefore('#').split('/')
        if (segments.size < 3 || segments[0].isNotEmpty() || segments[1] != "t" || segments[2].isEmpty()) return null
        return percentDecoded(segments[2])
    }

    /** [route] with its `/t/<key>` segment replaced by the canonical thread [id]. */
    fun withThreadId(route: String, id: String): String {
        if (threadRouteKey(route) == null) return route
        val rest = route.removePrefix("/t/")
        val keyLength = rest.indexOfFirst { it == '/' || it == '?' || it == '#' }.let { if (it < 0) rest.length else it }
        return "/t/" + encodeSegment(id) + rest.substring(keyLength)
    }

    /** Query values, `encodeURIComponent`-decoded (a literal `+` stays a plus). */
    private fun queryOf(route: String): Map<String, String> {
        val raw = route.substringAfter('?', "").substringBefore('#')
        val values = linkedMapOf<String, String>()
        raw.split('&').filter(String::isNotBlank).forEach { part ->
            val pieces = part.split('=', limit = 2)
            values.putIfAbsent(percentDecoded(pieces[0]), percentDecoded(pieces.getOrElse(1) { "" }))
        }
        return values
    }

    /** `/api/magician/v3/tasks/{id}/outputs/{rel}` with `.`/`..` dropped and each segment encoded. */
    fun taskOutputPath(taskId: String, relativePath: String): String {
        val segments = relativePath.trim().trimStart('/').removePrefix("outputs/").split('/')
            .filter { it.isNotBlank() && it != "." && it != ".." }
            .joinToString("/") { encodeSegment(it) }
        return "/api/magician/v3/tasks/${encodeSegment(taskId)}/outputs/$segments"
    }

    /**
     * The link a structured `artifacts` row opens: its `href`/`url`, its
     * absolute path, its task output, or its `magician-artifact:task:` id; null
     * when the row names nothing openable.
     */
    fun artifactItemLink(item: Map<String, String>): String? {
        fun value(key: String) = item[key]?.trim()?.takeIf(String::isNotEmpty)
        value("href")?.let { return it }
        value("url")?.let { return it }
        value("absolute_path")?.let { return it }
        val relative = value("relative_path") ?: value("path")
        val task = value("task_id")
        if (task != null && relative != null) return taskOutputPath(task, relative)
        val artifactId = value("artifact_id")
        if (artifactId != null && artifactId.startsWith("magician-artifact:task:")) {
            val parts = artifactId.removePrefix("magician-artifact:task:").split(':', limit = 2)
            if (parts.size == 2 && parts[0].isNotEmpty() && parts[1].isNotEmpty()) return taskOutputPath(parts[0], parts[1])
        }
        return relative
    }

    private fun taskOutputOf(path: String): ChatLinkTarget.TaskOutput? {
        for (pattern in listOf(executionOutput, taskOutput)) {
            val match = pattern.find(path) ?: continue
            val relative = match.groupValues[2]
            if (relative.isNotEmpty()) return ChatLinkTarget.TaskOutput(match.groupValues[1], relative)
        }
        return null
    }

    private fun hasFileExtension(path: String): Boolean {
        val last = path.substringAfterLast('/')
        return last.indexOf('.') > 0 && fileExtension.containsMatchIn(last)
    }

    private fun sameOriginReference(raw: String, origin: String): String? {
        val link = runCatching { URI(raw) }.getOrNull() ?: return null
        val home = runCatching { URI(origin.trim()) }.getOrNull() ?: return null
        val scheme = link.scheme?.lowercase() ?: return null
        if (scheme != home.scheme?.lowercase() || link.rawUserInfo != null) return null
        if (link.host.isNullOrBlank() || !link.host.equals(home.host, ignoreCase = true)) return null
        val defaultPort = if (scheme == "https") 443 else 80
        val linkPort = if (link.port == -1) defaultPort else link.port
        val homePort = if (home.port == -1) defaultPort else home.port
        if (linkPort != homePort) return null
        return buildString {
            append(link.rawPath?.takeIf(String::isNotEmpty) ?: "/")
            link.rawQuery?.let { append('?').append(it) }
            link.rawFragment?.let { append('#').append(it) }
        }
    }

    /** Percent-decoded as UTF-8 (`+` stays a plus); the input when it is not valid encoding. */
    fun percentDecoded(value: String): String {
        if ('%' !in value) return value
        val bytes = ByteArrayOutputStream()
        var index = 0
        while (index < value.length) {
            val char = value[index]
            if (char == '%') {
                if (index + 2 >= value.length) return value
                val hex = value.substring(index + 1, index + 3)
                if (!hex.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) return value
                bytes.write(hex.toInt(16))
                index += 3
            } else {
                bytes.write(char.toString().toByteArray(Charsets.UTF_8))
                index++
            }
        }
        return bytes.toString(Charsets.UTF_8.name())
    }

    private fun encodeSegment(value: String): String =
        URLEncoder.encode(value, Charsets.UTF_8.name()).replace("+", "%20")

    // MARK: Path linkification

    private val inline = Regex(
        listOf(
            """!?\[[^\]\n]*]\([^)\n]*\)""",
            """<[^>\n]*>""",
            """`([^`\n]+)`""",
            """(?:file:/{0,3})?/(?:[^\s<>"'`/()\[\]]+/)+[^\s<>"'`/()\[\]]+\.[A-Za-z0-9]{1,8}""",
            """[A-Za-z][A-Za-z0-9+.\-]*://[^\s<>]+""",
        ).joinToString("|") { "($it)" },
    )
    private const val destinationAllowed = "-._~!$&'*+,;=:@/"

    /**
     * Turns file paths that chat text mentions — inline code holding a path and
     * bare absolute paths in prose, with or without `file://` — into Markdown
     * links, like web `linkifyPaths` / `linkifyTextPaths`. Fenced code, existing
     * links, autolinks and URLs are left alone. The link target is the path
     * itself (percent-encoded), so a tap goes through [classify].
     */
    fun linkifyMarkdown(text: String): String {
        if ('/' !in text) return text
        var fence: Char? = null
        return text.split('\n').joinToString("\n") { line ->
            val trimmed = line.trimStart(' ')
            val marker = when {
                trimmed.startsWith("```") -> '`'
                trimmed.startsWith("~~~") -> '~'
                else -> null
            }
            if (marker != null && line.length - trimmed.length <= 3) {
                fence = if (fence == null) marker else if (fence == marker) null else fence
                line
            } else if (fence == null) linkifyLine(line) else line
        }
    }

    /** Inline code worth a link: `file://`, an absolute file or task output path, or `dir/file.ext`. */
    fun isLinkableCode(content: String): Boolean {
        val value = content.trim()
        if (value.isEmpty() || '\n' in value) return false
        val lower = value.lowercase()
        val isFile = lower.startsWith("file:")
        val absolute = value.startsWith("/") && !value.startsWith("//")
        val relative = !absolute && !isFile && '/' in value && ' ' !in value && ':' !in value
        if (!absolute && !relative && !isFile) return false
        if (absolute && value.split('/').count(String::isNotEmpty) < 2) return false
        return when (classify(value)) {
            is ChatLinkTarget.HostFile, is ChatLinkTarget.TaskOutput -> true
            is ChatLinkTarget.ServerResource -> absolute
            else -> false
        }
    }

    fun linkDestination(value: String): String {
        val out = StringBuilder()
        for (byte in value.toByteArray(Charsets.UTF_8)) {
            val code = byte.toInt() and 0xff
            val char = code.toChar()
            if (code < 0x80 && (char.isLetterOrDigit() || char in destinationAllowed)) out.append(char)
            else out.append('%').append("%02X".format(code))
        }
        return out.toString()
    }

    private fun escapedLabel(value: String): String = buildString {
        value.forEach { if (it in "\\`*_[]<>") append('\\'); append(it) }
    }

    private fun linkifyLine(line: String): String {
        if ('/' !in line) return line
        return inline.replace(line) { match ->
            val text = match.value
            when {
                match.groups[3] != null -> {
                    val content = match.groupValues[4]
                    if (isLinkableCode(content)) "[$text](${linkDestination(content.trim())})" else text
                }
                match.groups[5] != null -> {
                    val before = if (match.range.first > 0) line[match.range.first - 1] else ' '
                    if (before.isWhitespace() || before in "(\"'*_>~") "[${escapedLabel(text)}](${linkDestination(text)})" else text
                }
                else -> text
            }
        }
    }
}
