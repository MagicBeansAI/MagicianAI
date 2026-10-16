package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Off-plan attribution grouping. The same cases as web
 * `planStepActivity.test.ts` and iOS `PlanProgressModelTests` (off-plan section).
 */
class PlanStepActivityTest {
    private data class Row(val id: String, val at: Long, val step: String?, val offPlan: Boolean = false)

    private fun group(rows: List<Row>) =
        PlanStepActivity.group(rows, { it.at }) { PlanStepAttribution(it.step, it.offPlan) }

    @Test
    fun anUnattributedRunIsLeftUnchanged() {
        val groups = group(listOf(Row("a", 1, null), Row("b", 2, null)))
        assertFalse(groups.attributed)
        assertTrue(groups.byStep.isEmpty())
        assertTrue(groups.offPlan.isEmpty())
        assertFalse(group(emptyList()).attributed)
    }

    @Test
    fun groupsRowsUnderTheirStepAndTheRestOutsideThePlanOldestFirst() {
        val groups = group(
            listOf(
                Row("after", 9, null, offPlan = true),
                Row("s2-call", 7, "s2"),
                Row("s1-second", 3, "s1"),
                Row("before", 1, null, offPlan = true),
                Row("s1-first", 2, "s1"),
                Row("between", 5, null, offPlan = true),
            ),
        )
        assertTrue(groups.attributed)
        assertEquals(listOf("s1-first", "s1-second"), groups.byStep["s1"]?.map { it.id })
        assertEquals(listOf("s2-call"), groups.byStep["s2"]?.map { it.id })
        assertEquals(listOf("before", "between", "after"), groups.offPlan.map { it.id })
    }

    @Test
    fun ignoresRowsThatCarryNoAttributionInAnAttributedRun() {
        val groups = group(listOf(Row("lifecycle", 1, null), Row("s1", 2, "s1")))
        assertTrue(groups.attributed)
        assertTrue(groups.offPlan.isEmpty())
        assertEquals(1, groups.byStep["s1"]?.size)
    }

    @Test
    fun countsAStepWithNoRowsAsZeroAndNothingWhenUnattributed() {
        val groups = group(listOf(Row("s1", 2, "s1")))
        assertEquals(1, groups.count("s1"))
        assertEquals(0, groups.count("s3"))
        assertNull(group(emptyList()).count("s1"))
    }

    @Test
    fun labelsTheOffPlanGroupWithItsCountAndExplanation() {
        assertEquals("Outside the plan · 3", PlanStepActivity.offPlanLabel(3))
        assertEquals("Work the agent did between or beyond the planned steps", PlanStepActivity.OFF_PLAN_EXPLANATION)
    }

    @Test
    fun readsTheAttributionFromAnActivityRowOrATimelineRow() {
        val activity = JsonObject(
            mapOf("metadata" to JsonObject(mapOf("plan_step_id" to JsonPrimitive("s1")))),
        )
        assertEquals(PlanStepAttribution("s1", false), PlanStepAttribution.of(activity))
        val timeline = JsonObject(mapOf("off_plan" to JsonPrimitive(true)))
        assertEquals(PlanStepAttribution(null, true), PlanStepAttribution.of(timeline))
        assertEquals(PlanStepAttribution.NONE, PlanStepAttribution.of(JsonObject(emptyMap())))
    }

    @Test
    fun readsEachStepsActivityCount() {
        val rows = PlanSteps.rows(
            listOf(
                JsonObject(mapOf("number" to JsonPrimitive(1), "name" to JsonPrimitive("A"), "status" to JsonPrimitive("completed"), "step_id" to JsonPrimitive("s1"), "activity_count" to JsonPrimitive(4))),
                JsonObject(mapOf("number" to JsonPrimitive(2), "name" to JsonPrimitive("B"), "status" to JsonPrimitive("pending"), "step_id" to JsonPrimitive("s2"))),
            ),
            runEnded = false,
        )
        assertEquals(listOf(4, null), rows.map { it.activityCount })
    }
}
