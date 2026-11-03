package ai.magicbeans.magdroid.work

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Test

class TeamRoutinesTest {
    private fun agent(id: String, harness: Boolean, vararg focus: String): String {
        val areas = focus.joinToString(",") { """{"name":"$it","description":"","priority":"high"}""" }
        val harnessField = if (harness) """"harness":{},""" else ""
        return """{"definition":{"agent_id":"$id",$harnessField"autonomous_config":{"schedule":"0 9 * * *","focus_areas":[$areas]}}}"""
    }

    @Test fun eachHarnessAgentsLiveFocusAreasInAgentOrder() {
        val payload = Json.parseToJsonElement(
            """{"agents":[${agent("cro", true, "prolific investors")},${agent("ceo", true, "accelerators", "Weekly Replan", "weekly replan")},${agent("cpo", true)},${agent("creative-mind", false, "general")}]}"""
        )
        assertEquals(
            listOf(
                TeamRoutineTarget("ceo", "harness:ceo:accelerators", "accelerators"),
                TeamRoutineTarget("ceo", "harness:ceo:weekly-replan", "Weekly Replan"),
                TeamRoutineTarget("cro", "harness:cro:prolific-investors", "prolific investors"),
            ),
            teamRoutineTargets(payload),
        )
    }

    @Test fun slugsLikeTheBackend() {
        assertEquals("weekly-replan", harnessGoalSlug("  Weekly Replan!! "))
        assertEquals("unnamed", harnessGoalSlug("***"))
        assertEquals("caf", harnessGoalSlug("Café"))
        val long = (0 until 12).joinToString(" ") { "word$it" }
        assertEquals("word0-word1-word2-word3-word4-word5-word6-word7-word8-word9", harnessGoalSlug(long))
        assertEquals("x".repeat(64), harnessGoalSlug("x".repeat(70)))
    }
}
