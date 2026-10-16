package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.access.MagicanAppLinks
import java.net.URI
import java.net.URLDecoder
import java.util.concurrent.atomic.AtomicReference

/**
 * Where a `magican://vibe?project=…&run=…` (or `&crew=…`) link lands inside
 * VibeDev: the run, or the crew's project. Accepted with the link and taken
 * once when the shell opens VibeDev, on top of Now, so Back returns there.
 */
object VibeDevLinks {
    private val pending = AtomicReference<VibeDevRoute?>(null)

    fun accept(raw: String?) = pending.set(parse(raw))

    fun take(): VibeDevRoute? = pending.getAndSet(null)

    internal fun parse(raw: String?): VibeDevRoute? {
        val uri = raw?.let { runCatching { URI(it) }.getOrNull() } ?: return null
        if (!MagicanAppLinks.isScheme(uri.scheme) || uri.host != "vibe") return null
        val query = runCatching {
            uri.rawQuery.orEmpty().split('&').filter(String::isNotBlank).associate { part ->
                val pair = part.split('=', limit = 2)
                URLDecoder.decode(pair[0], Charsets.UTF_8.name()) to URLDecoder.decode(pair.getOrElse(1) { "" }, Charsets.UTF_8.name())
            }
        }.getOrNull() ?: return null
        fun q(key: String) = query[key]?.trim()?.takeIf(String::isNotEmpty)
        q("run")?.let { return VibeDevRoute.Run(it, "Run") }
        if (q("crew") != null) q("project")?.let { return VibeDevRoute.Project(it, "") }
        return null
    }
}
