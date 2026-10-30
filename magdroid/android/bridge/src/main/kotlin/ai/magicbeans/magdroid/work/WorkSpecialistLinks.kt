package ai.magicbeans.magdroid.work

import java.net.URI
import java.net.URLEncoder

/** A web specialist owns its existing controls and browser login. No native
 * credential is ever included in this handoff. Filters mirror the web adapter. */
enum class WorkSpecialist(val path: String, val title: String) {
    Analytics("/llm", "Models and usage"), History("/history", "Execution history"),
    Memory("/memory", "Memory observability"), Budget("/budget", "Budget and tokens"),
    Evals("/evals", "Evaluations"), Apps("/apps", "Apps"), Skills("/skills", "Skills"),
    Runtime("/runtime", "Runtime diagnostics"), Events("/events", "Events"), Observe("/observe", "Performance")
}

data class WorkWebContext(
    val section: WorkSection = WorkSection.All,
    val agent: String = "", val task: String = "", val execution: String = "",
    val internal: Boolean = false, val program: String = "", val run: String = "",
    val appInstallation: String = "",
) {
    fun returnPath(): String {
        val query = mutableListOf<Pair<String, String>>()
        when (section) {
            WorkSection.Crew -> { query += "view" to "crew"; if (agent.isNotBlank()) query += "agent" to agent }
            WorkSection.Programs -> { query += "view" to "programs"; if (program.isNotBlank()) query += "program" to program }
            WorkSection.Quality -> { query += "view" to "quality"; if (run.isNotBlank()) query += "run" to run; if (agent.isNotBlank()) query += "agent" to agent }
            WorkSection.Operations -> query += "view" to "operations"
            WorkSection.Goals -> query += "view" to "goals"
            WorkSection.NeedsYou -> query += "view" to "needs-you"
            WorkSection.All -> {
                if (task.isNotBlank()) query += "selected" to task
                if (internal) query += "type" to "internal"
                if (execution.isNotBlank()) query += "execution_id" to execution
            }
        }
        return "/work" + queryString(query)
    }
    fun url(base: String, destination: WorkSpecialist): String? = runCatching {
        val origin = URI(base)
        require(origin.scheme?.lowercase() in listOf("http", "https") && !origin.host.isNullOrBlank())
        require(origin.rawUserInfo == null && origin.rawQuery == null && origin.rawFragment == null)
        val query = mutableListOf<Pair<String, String>>()
        if (destination in listOf(WorkSpecialist.Analytics, WorkSpecialist.History, WorkSpecialist.Memory, WorkSpecialist.Events) && agent.isNotBlank()) query += "agent_id" to agent
        if (destination in listOf(WorkSpecialist.Analytics, WorkSpecialist.Events) && task.isNotBlank()) query += "task_id" to task
        if (destination == WorkSpecialist.Events && execution.isNotBlank()) query += "execution_id" to execution
        query += "from" to returnPath()
        val path = destination.path + if (destination == WorkSpecialist.Apps && Regex("^[A-Za-z0-9_.-]{1,128}$").matches(appInstallation)) "/$appInstallation" else ""
        base.trimEnd('/') + path + queryString(query) + if (destination == WorkSpecialist.Analytics && (agent.isNotBlank() || task.isNotBlank())) "#usage" else ""
    }.getOrNull()
    fun workURL(base: String): String? = if (url(base, WorkSpecialist.Events) != null) base.trimEnd('/') + returnPath() else null
}

private fun queryString(values: List<Pair<String, String>>): String = if (values.isEmpty()) "" else values.joinToString("&", "?") { (key, value) -> "$key=${URLEncoder.encode(value, "UTF-8")}" }
