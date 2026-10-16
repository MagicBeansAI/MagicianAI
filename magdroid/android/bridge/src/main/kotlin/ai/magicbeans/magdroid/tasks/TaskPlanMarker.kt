package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonObject

/*
 * Whether a task was planned, for the small plan glyph beside its title (the
 * Tasks list row, the task detail header and its compact title bar). Pure.
 *
 * Planned means any sign of a plan on the row or record: `has_plan`, a plan
 * status, a `latest_plan_id` / `approved_plan_id`, or plan steps. Fields are
 * the V3 list row's (`magician-api/src/task_api_v3.rs`), all optional.
 */
object TaskPlanMarker {
    fun hasPlan(
        hasPlan: Boolean?,
        planStatus: String?,
        latestPlanId: String?,
        approvedPlanId: String?,
        planStepCount: Int = 0,
    ): Boolean =
        hasPlan == true ||
            !planStatus.isNullOrBlank() ||
            !latestPlanId.isNullOrBlank() ||
            !approvedPlanId.isNullOrBlank() ||
            planStepCount > 0

    /** A Tasks list row. */
    fun hasPlan(task: TaskV3): Boolean =
        hasPlan(task.hasPlan, task.planStatus, task.latestPlanId, task.approvedPlanId)

    /**
     * A raw task record or `/plan` payload: the same fields, plus plan steps
     * (`plan_steps`, `plan.steps`, or `plan_graph.steps`, at the top level or
     * under `plan`).
     */
    fun hasPlan(record: JsonObject?): Boolean {
        val row = record ?: return false
        val plan = row["plan"].jObj()
        val steps = listOfNotNull(
            row["plan_steps"].jArr(),
            plan?.get("steps").jArr(),
            row["plan_graph"].jObj()?.get("steps").jArr(),
            plan?.get("plan_graph").jObj()?.get("steps").jArr(),
        ).maxOfOrNull { it.size } ?: 0
        return hasPlan(
            hasPlan = row["has_plan"].jBool(),
            planStatus = row["plan_status"].jText() ?: plan?.get("status").jText(),
            latestPlanId = row["latest_plan_id"].jText() ?: plan?.get("plan_id").jText(),
            approvedPlanId = row["approved_plan_id"].jText(),
            planStepCount = steps,
        )
    }
}
