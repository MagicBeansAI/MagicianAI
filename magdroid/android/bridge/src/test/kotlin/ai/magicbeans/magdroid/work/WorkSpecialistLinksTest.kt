package ai.magicbeans.magdroid.work

import org.junit.Assert.*
import org.junit.Test
import java.net.URI
import java.net.URLDecoder

class WorkSpecialistLinksTest {
    private fun query(url: String): Map<String, String> = URI(url).rawQuery.split('&').associate { val pair = it.split('=', limit = 2); pair[0] to URLDecoder.decode(pair[1], "UTF-8") }
    @Test fun retainsExactInternalExecutionAndOnlySupportedFilters() {
        val context = WorkWebContext(agent = "owner + / café", task = "task/+one", execution = "run + / two", internal = true)
        val events = requireNotNull(context.url("https://paired.example", WorkSpecialist.Events))
        assertEquals("/events", URI(events).path)
        assertEquals(context.agent, query(events)["agent_id"])
        assertEquals(context.task, query(events)["task_id"])
        assertEquals(context.execution, query(events)["execution_id"])
        val from = requireNotNull(query(events)["from"])
        assertEquals(context.task, query("https://paired.example$from")["selected"])
        assertEquals(context.execution, query("https://paired.example$from")["execution_id"])
        assertEquals("internal", query("https://paired.example$from")["type"])
        val budget = query(requireNotNull(context.url("https://paired.example", WorkSpecialist.Budget)))
        assertEquals(setOf("from"), budget.keys)
        assertEquals("usage", URI(context.url("https://paired.example", WorkSpecialist.Analytics)!!).fragment)
    }
    @Test fun keepsCrewProgramAndQualityReturnsWithoutCredentials() {
        for (context in listOf(WorkWebContext(WorkSection.Crew, agent = "forge"), WorkWebContext(WorkSection.Programs, program = "planning/+launch.md"), WorkWebContext(WorkSection.Quality, run = "run/+one"), WorkWebContext(WorkSection.Operations))) {
            for (destination in WorkSpecialist.entries) {
                val url = requireNotNull(context.url("https://paired.example", destination))
                assertEquals(context.returnPath(), query(url)["from"])
                assertNull(URI(url).userInfo)
            }
        }
        for (base in listOf("javascript:alert(1)", "https://token@paired.example", "https://paired.example?bearer=secret", "https://paired.example#secret", "//paired.example")) assertNull(WorkWebContext().url(base, WorkSpecialist.Events))
    }
}
