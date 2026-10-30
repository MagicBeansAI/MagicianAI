package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicanAppLinks
import ai.magicbeans.magdroid.work.WorkSection
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.URI
import java.net.URLDecoder

internal sealed interface WorkLinkTarget {
    data class Library(val section: WorkSection, val route: WorkRoute? = null) : WorkLinkTarget
    data class Thread(val id: String) : WorkLinkTarget
}

/** Shared typed handoff from OS intents and in-app canonical/legacy routes. */
internal object WorkDeepLinks {
    private val pending = MutableStateFlow<WorkLinkTarget?>(null)
    val target = pending.asStateFlow()
    fun accept(raw: String?): Boolean { val value = parse(raw) ?: return false; pending.value = value; return true }
    fun request(value: WorkLinkTarget) { pending.value = value }
    fun consume(value: WorkLinkTarget) { if (pending.value == value) pending.value = null }
    fun clear() { pending.value = null }
    fun parse(raw: String?): WorkLinkTarget? {
        val uri = raw?.let { runCatching { URI(it) }.getOrNull() } ?: return null
        if (uri.scheme != null && uri.scheme !in listOf("http", "https") && !MagicanAppLinks.isScheme(uri.scheme)) return null
        fun decode(value: String) = URLDecoder.decode(value.replace("+", "%2B"), Charsets.UTF_8.name())
        val query = runCatching { uri.rawQuery.orEmpty().split('&').filter(String::isNotBlank).associate { part ->
            val pair = part.split('=', limit = 2); URLDecoder.decode(pair[0], Charsets.UTF_8.name()) to URLDecoder.decode(pair.getOrElse(1) { "" }, Charsets.UTF_8.name())
        } }.getOrNull() ?: return null
        fun q(key: String) = query[key]?.trim()?.takeIf(String::isNotEmpty)
        var path = uri.rawPath.orEmpty().trimEnd('/')
        if (MagicanAppLinks.isScheme(uri.scheme)) { if (uri.host != "work") return null; path = "/work$path" }
        val parts = runCatching { path.split('/').filter(String::isNotBlank).map(::decode) }.getOrNull() ?: return null
        fun library(section: WorkSection, route: WorkRoute? = null) = WorkLinkTarget.Library(section, route)
        if (parts.firstOrNull() == "crew" || parts.take(2) == listOf("work", "crew")) {
            val crew = parts.drop(if (parts.first() == "crew") 1 else 2)
            if (crew.isEmpty()) return library(WorkSection.Crew)
            if (crew == listOf("new")) return library(WorkSection.Crew, q("edit")?.let { WorkRoute("member-edit", it, "Edit member") } ?: WorkRoute("member-create", title = "Add member"))
            if (crew.size == 1) return library(WorkSection.Crew, WorkRoute("member", crew[0], "Crew member"))
            if (crew.size == 2 && crew[1] in listOf("edit", "rules")) return library(WorkSection.Crew, WorkRoute(if (crew[1] == "edit") "member-edit" else "member-rules", crew[0], if (crew[1] == "edit") "Edit member" else "Rules"))
            if (crew.size == 3 && crew[1] == "memory") return library(WorkSection.Crew, WorkRoute("memory-tier", crew[0], "Memory tier", goalId = q("goal_id").orEmpty(), tier = crew[2]))
            return null
        }
        if (parts.size == 3 && parts[0] == "t" && parts[2] == "tasks") return if (q("selected") == null) WorkLinkTarget.Thread(parts[1]) else null
        if (path == "/harness") return library(WorkSection.Operations)
        if (path !in listOf("/work", "/relentless")) return null
        val view = if (path == "/relentless") "quality" else q("view")
        return when (view) {
            "crew" -> library(WorkSection.Crew, q("agent")?.let { WorkRoute("member", it, "Crew member") })
            "programs" -> library(WorkSection.Programs, q("program")?.let { WorkRoute("program", it, "Playbook") })
            "quality" -> library(WorkSection.Quality, when {
                q("run") != null -> WorkRoute("quality-run", q("run")!!, "Quality run")
                q("proposal") != null -> WorkRoute("quality-proposal", q("proposal")!!, "Quality proposal")
                q("campaign") != null -> WorkRoute("quality-campaign", q("campaign")!!, "Capture policy")
                q("task") != null -> WorkRoute("quality-context", q("task")!!, "Quality checks", agentId = q("agent").orEmpty())
                else -> WorkRoute("Quality", title = "Quality checks", agentId = q("agent").orEmpty())
            })
            "operations" -> library(WorkSection.Operations)
            "goals" -> library(WorkSection.Goals, if (q("goals") == "closed") WorkGoalCopy.CLOSED_ROUTE else null)
            "needs-you" -> library(WorkSection.NeedsYou)
            null, "all", "inbox", "today", "overdue", "running", "completed" -> if (q("selected") == null) q("thread")?.let { WorkLinkTarget.Thread(it) } ?: library(WorkSection.All) else null
            else -> null
        }
    }
}
