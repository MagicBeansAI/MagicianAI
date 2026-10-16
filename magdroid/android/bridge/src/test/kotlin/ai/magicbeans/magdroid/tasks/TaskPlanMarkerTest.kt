package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TaskPlanMarkerTest {
    private fun json(text: String) = Json.parseToJsonElement(text).jsonObject

    @Test
    fun `any sign of a plan marks the task planned`() {
        assertTrue(TaskPlanMarker.hasPlan(TaskV3(id = "a", hasPlan = true)))
        assertTrue(TaskPlanMarker.hasPlan(TaskV3(id = "b", planStatus = "draft")))
        assertTrue(TaskPlanMarker.hasPlan(TaskV3(id = "c", latestPlanId = "plan-1")))
        assertTrue(TaskPlanMarker.hasPlan(TaskV3(id = "d", approvedPlanId = "plan-2")))
        assertTrue(TaskPlanMarker.hasPlan(null, null, null, null, planStepCount = 3))
        assertFalse(TaskPlanMarker.hasPlan(TaskV3(id = "e")))
        assertFalse(TaskPlanMarker.hasPlan(TaskV3(id = "f", planStatus = " ", latestPlanId = "")))
    }

    @Test
    fun `the list row decodes approved_plan_id`() {
        val row = taskJson.decodeFromString(TaskV3.serializer(), """{"id":"t","approved_plan_id":"p9"}""")
        assertTrue(TaskPlanMarker.hasPlan(row))
    }

    @Test
    fun `raw records read flags, ids, status and steps`() {
        assertTrue(TaskPlanMarker.hasPlan(json("""{"has_plan":true}""")))
        assertTrue(TaskPlanMarker.hasPlan(json("""{"plan":{"status":"approved"}}""")))
        assertTrue(TaskPlanMarker.hasPlan(json("""{"plan":{"plan_graph":{"steps":[{"id":"s1"}]}}}""")))
        assertTrue(TaskPlanMarker.hasPlan(json("""{"plan_steps":[{"name":"x"}]}""")))
        assertFalse(TaskPlanMarker.hasPlan(json("""{"has_plan":false,"plan_steps":[],"plan":null}""")))
        assertFalse(TaskPlanMarker.hasPlan(null))
    }
}
