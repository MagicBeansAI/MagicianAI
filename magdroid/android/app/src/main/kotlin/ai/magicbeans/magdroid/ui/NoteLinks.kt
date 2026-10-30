package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicanAppLinks
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.PairedMachine
import ai.magicbeans.magdroid.notes.NoteDocument
import ai.magicbeans.magdroid.notes.NotesLibraryRepository
import android.content.Context
import android.net.Uri
import android.os.SystemClock
import androidx.compose.ui.platform.UriHandler
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.URI
import java.net.URLDecoder
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

/**
 * A link to one document in Magician's own Notes app.
 *
 * The canonical form is relative — `/notes?path=<note path>&workspace=<id>` —
 * as the server writes it into chat replies and note records and the web
 * `/notes` page reads it. The same route on a paired machine's absolute
 * origin, and `magican://notes?path=…&workspace=…` (OS links, notifications),
 * mean the same thing. `workspace` is absent on older links; those belong to
 * the workspace the app is signed in to. [origin] is set only for an absolute
 * link and is the normalized `scheme://host[:port]`.
 */
data class NoteLink(val path: String, val workspace: String?, val origin: String? = null)

/** What opening a [NoteLink] should do on this phone. */
sealed interface NoteLinkResolution {
    /** The note is in the workspace the app is signed in to. */
    data class Open(val path: String) : NoteLinkResolution

    /** Another paired machine holds that workspace: switch to it, then open. */
    data class SwitchMachine(val origin: String, val path: String) : NoteLinkResolution

    /** No paired machine on this phone is signed in to that workspace. */
    data class OtherWorkspace(val workspace: String) : NoteLinkResolution

    /** An absolute link on a machine this phone is not paired with. */
    data object NotOurs : NoteLinkResolution
}

/** What the shell shows for an accepted note link. */
sealed interface NoteLinkTarget {
    /** [request] distinguishes two taps on the same note. */
    data class Open(val path: String, val request: Long) : NoteLinkTarget
    data class OtherWorkspace(val workspace: String, val request: Long) : NoteLinkTarget
}

/**
 * One durable in-process handoff from a note link — Markdown in chat replies,
 * task summaries, Attention details, notes previews, or a `magican://notes`
 * intent — to the shell, which opens the native Notes screen on it.
 *
 * A shared helper: any surface that renders a note link can call [accept]
 * (or install [NoteLinkUriHandler]) and needs no Notes state of its own.
 */
object NoteLinks {
    private val requests = AtomicLong(0)
    private val _target = MutableStateFlow<NoteLinkTarget?>(null)
    val target: StateFlow<NoteLinkTarget?> = _target.asStateFlow()

    fun accept(context: Context, uri: Uri?): Boolean = accept(context, uri?.toString())

    /**
     * Opens [raw] when it is a note link this phone can resolve, switching to
     * the paired machine that holds its workspace when needed. Returns false
     * for anything else, so the caller keeps its own handling.
     */
    fun accept(context: Context, raw: String?): Boolean {
        val link = parse(raw) ?: return false
        val machines = runCatching { MagicianAccess.listProfiles(context) }.getOrDefault(emptyList())
        when (val resolution = resolve(link, machines)) {
            NoteLinkResolution.NotOurs -> return false
            is NoteLinkResolution.Open -> openFetching(context, resolution.path)
            is NoteLinkResolution.SwitchMachine -> {
                val switched = runCatching { MagicianAccess.activate(context, resolution.origin) }.isSuccess
                if (switched) openFetching(context, resolution.path)
                else _target.value = NoteLinkTarget.OtherWorkspace(link.workspace.orEmpty(), requests.incrementAndGet())
            }
            is NoteLinkResolution.OtherWorkspace ->
                _target.value = NoteLinkTarget.OtherWorkspace(resolution.workspace, requests.incrementAndGet())
        }
        return true
    }

    /** Opens a note path in the current workspace (a published-note row, …). */
    fun open(path: String) {
        val trimmed = path.trim().trimStart('/')
        if (trimmed.isNotEmpty()) _target.value = NoteLinkTarget.Open(trimmed, requests.incrementAndGet())
    }

    /**
     * [open], with the note's file already on its way: a cold start spends
     * seconds before the Notes screen exists, and the fetch need not wait.
     */
    private fun openFetching(context: Context, path: String) {
        val trimmed = path.trim().trimStart('/')
        if (trimmed.isEmpty()) return
        NotePrefetch.start(context, trimmed)
        open(trimmed)
    }

    fun consume(target: NoteLinkTarget) {
        _target.compareAndSet(target, null)
    }

    internal fun parse(raw: String?): NoteLink? {
        val text = raw?.trim()?.takeIf(String::isNotEmpty) ?: return null
        val uri = runCatching { URI(text) }.getOrNull() ?: return null
        val scheme = uri.scheme?.lowercase()
        var origin: String? = null
        when {
            scheme == null -> {
                if (uri.rawAuthority != null || route(uri.rawPath) != "/notes") return null
            }
            MagicanAppLinks.isScheme(scheme) -> {
                if (uri.host?.lowercase() != "notes" || uri.rawPath.orEmpty() !in setOf("", "/")) return null
            }
            scheme == "http" || scheme == "https" -> {
                if (uri.rawUserInfo != null || uri.host.isNullOrBlank() || route(uri.rawPath) != "/notes") return null
                origin = originKey(text) ?: return null
            }
            else -> return null
        }
        val query = queryOf(uri.rawQuery) ?: return null
        val path = query["path"]?.trim()?.trimStart('/')?.takeIf { it.isNotEmpty() && '\u0000' !in it } ?: return null
        val workspace = query["workspace"]?.trim()?.takeIf(String::isNotEmpty)
        return NoteLink(path, workspace, origin)
    }

    internal fun resolve(link: NoteLink, machines: List<PairedMachine>): NoteLinkResolution {
        val active = machines.firstOrNull { it.active }
        if (link.origin != null) {
            val target = machines.firstOrNull { originKey(it.origin) == link.origin } ?: return NoteLinkResolution.NotOurs
            if (link.workspace != null && link.workspace != target.workspace) {
                return NoteLinkResolution.OtherWorkspace(link.workspace)
            }
            return if (target.active) NoteLinkResolution.Open(link.path)
            else NoteLinkResolution.SwitchMachine(target.origin, link.path)
        }
        val workspace = link.workspace
        if (workspace == null || active == null || workspace == active.workspace) return NoteLinkResolution.Open(link.path)
        // A relative link names no machine. Switch only when exactly one other
        // paired machine is signed in to that workspace; two would be a guess.
        val candidates = machines.filter { !it.active && it.workspace == workspace }
        return if (candidates.size == 1) NoteLinkResolution.SwitchMachine(candidates[0].origin, link.path)
        else NoteLinkResolution.OtherWorkspace(workspace)
    }

    private fun route(rawPath: String?): String {
        val path = rawPath.orEmpty()
        return if (path.length > 1 && path.endsWith('/')) path.dropLast(1) else path
    }

    /** `encodeURIComponent` values: a literal `+` is a plus, not a space. */
    private fun queryOf(rawQuery: String?): Map<String, String>? = runCatching {
        val values = linkedMapOf<String, String>()
        rawQuery.orEmpty().split('&').filter(String::isNotBlank).forEach { part ->
            val pieces = part.split('=', limit = 2)
            val name = URLDecoder.decode(pieces[0].replace("+", "%2B"), Charsets.UTF_8.name())
            val value = URLDecoder.decode(pieces.getOrElse(1) { "" }.replace("+", "%2B"), Charsets.UTF_8.name())
            values.putIfAbsent(name, value)
        }
        values
    }.getOrNull()

    /** `scheme://host[:port]`, lowercased, with the scheme's default port dropped. */
    internal fun originKey(raw: String): String? {
        val uri = runCatching { URI(raw.trim()) }.getOrNull() ?: return null
        val scheme = uri.scheme?.lowercase()?.takeIf { it == "http" || it == "https" } ?: return null
        val host = uri.host?.lowercase()?.takeIf(String::isNotBlank) ?: return null
        val defaultPort = if (scheme == "https") 443 else 80
        val port = if (uri.port == -1 || uri.port == defaultPort) "" else ":${uri.port}"
        return "$scheme://$host$port"
    }
}

/**
 * The app-wide link opener for Compose text: note links open the native Notes
 * screen, everything else goes to [fallback]. A link the platform cannot open
 * (a relative path no app handles) is dropped instead of crashing the app.
 */
class NoteLinkUriHandler(private val context: Context, private val fallback: UriHandler) : UriHandler {
    override fun openUri(uri: String) {
        if (NoteLinks.accept(context, uri)) return
        runCatching { fallback.openUri(uri) }
    }
}

/**
 * The file of the note a link just asked for, fetched as the link is accepted
 * instead of once the Notes screen is up. Holds one fetch; the Notes screen
 * [take]s it for the same path on the same machine, or fetches the file itself.
 */
internal object NotePrefetch {
    private const val FRESH_MS = 30_000L

    private class Pending(
        val origin: String?,
        val path: String,
        val at: Long,
        val result: Deferred<Result<NoteDocument>>,
    )

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val pending = AtomicReference<Pending?>(null)

    fun start(context: Context, path: String) {
        val app = context.applicationContext
        val origin = runCatching { MagicianAccess.activeOrigin(app) }.getOrNull()
        val fetch = scope.async {
            val repository = NotesLibraryRepository(app)
            try {
                runCatching { repository.open(path) }
            } finally {
                repository.close()
            }
        }
        pending.getAndSet(Pending(origin, path, SystemClock.elapsedRealtime(), fetch))?.result?.cancel()
    }

    /** The fetched note when [fetch] has already finished well; null otherwise. */
    @OptIn(ExperimentalCoroutinesApi::class)
    fun ready(fetch: Deferred<Result<NoteDocument>>): NoteDocument? =
        if (fetch.isCompleted && !fetch.isCancelled) fetch.getCompleted().getOrNull() else null

    /** The fetch started for [path] on the machine still active, once; null otherwise. */
    fun take(context: Context, path: String): Deferred<Result<NoteDocument>>? {
        val current = pending.get()?.takeIf { it.path == path } ?: return null
        if (!pending.compareAndSet(current, null)) return null
        val origin = runCatching { MagicianAccess.activeOrigin(context.applicationContext) }.getOrNull()
        if (current.origin != origin || SystemClock.elapsedRealtime() - current.at > FRESH_MS) {
            current.result.cancel()
            return null
        }
        return current.result
    }
}
