package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonObject

/*
 * Off-plan attribution on the Run act: the run's activity rows grouped under
 * the approved-plan step the run was on when each was recorded, and the rows
 * outside every step (before the first, between steps, after the last, after
 * a skipped step). The server decides the attribution (`plan_step_id` /
 * `off_plan` on each row, from the run's durable step boundaries); this only
 * groups. Pure — no Compose.
 *
 * Port of web `planStepActivity.ts` and iOS `PlanStepActivity`
 * (`PlanProgressModel.swift`), with the same test cases.
 * See `docs/plans/2026-10-01-plan-step-progress.md`.
 */

/** One row's attribution as the wire states it. */
data class PlanStepAttribution(val planStepId: String?, val offPlan: Boolean) {
    companion object {
        val NONE = PlanStepAttribution(null, false)

        /**
         * An `activity_log` row carries it in `metadata`; a `debug.timeline`
         * row carries it at the top level.
         */
        fun of(row: JsonObject): PlanStepAttribution {
            val metadata = row["metadata"].jObj()
            val stepId = metadata?.get("plan_step_id").jText() ?: row["plan_step_id"].jText()
            val offPlan = metadata?.get("off_plan").jBool() ?: row["off_plan"].jBool() ?: false
            return PlanStepAttribution(stepId, offPlan)
        }
    }
}

data class PlanStepActivityGroups<T>(
    /** Whether any row carries an attribution; false leaves the act unchanged. */
    val attributed: Boolean,
    /** Each plan step's rows, oldest first, by plan step id. */
    val byStep: Map<String, List<T>>,
    /** The rows outside every plan step, oldest first. */
    val offPlan: List<T>,
) {
    /** A step's row count; 0 for a step with none; null when nothing is attributed. */
    fun count(stepId: String?): Int? =
        if (!attributed || stepId == null) null else byStep[stepId]?.size ?: 0
}

object PlanStepActivity {
    const val OFF_PLAN_EXPLANATION = "Work the agent did between or beyond the planned steps"

    fun offPlanLabel(count: Int): String = "Outside the plan · $count"

    /**
     * Rows with no attribution (lifecycle rows) belong to neither group; the
     * flat "All activity" timeline still shows every row.
     */
    fun <T> group(
        rows: List<T>,
        atMillis: (T) -> Long?,
        attribution: (T) -> PlanStepAttribution,
    ): PlanStepActivityGroups<T> {
        val ordered = rows.withIndex()
            .sortedWith(compareBy<IndexedValue<T>>({ atMillis(it.value) ?: Long.MIN_VALUE }, { it.index }))
            .map { it.value }
        val byStep = linkedMapOf<String, MutableList<T>>()
        val offPlan = mutableListOf<T>()
        var attributed = false
        for (row in ordered) {
            val mark = attribution(row)
            val stepId = mark.planStepId?.trim()?.takeIf { it.isNotEmpty() }
            if (stepId != null) {
                attributed = true
                byStep.getOrPut(stepId) { mutableListOf() }.add(row)
            } else if (mark.offPlan) {
                attributed = true
                offPlan.add(row)
            }
        }
        return if (attributed) PlanStepActivityGroups(true, byStep, offPlan)
        else PlanStepActivityGroups(false, emptyMap(), emptyList())
    }
}
