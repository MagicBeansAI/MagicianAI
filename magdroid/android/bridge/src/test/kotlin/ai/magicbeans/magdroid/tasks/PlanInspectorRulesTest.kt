package ai.magicbeans.magdroid.tasks

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The Plan Inspector's state rules: who may edit, the unsaved-changes guard,
 * the approval bar, save/restore round trips, Approve & Run's order, and live
 * run marks. One-for-one port of iOS `PlanInspectorModelTests`.
 */
class PlanInspectorRulesTest {
    private val base = "/api/magician/v3/tasks/task_1/plan"
    private lateinit var transport: StubPlanTransport

    @Before fun setUp() {
        transport = StubPlanTransport()
    }

    private val graphJSON: Map<String, Any?> = mapOf(
        "steps" to listOf(
            mapOf("id" to "a", "task" to "A", "tool" to "t", "parameters" to emptyMap<String, Any>(),
                "expected_outputs" to emptyList<Any>(), "confidence" to 0.9, "metadata" to emptyMap<String, Any>()),
            mapOf("id" to "b", "task" to "B", "tool" to null, "parameters" to emptyMap<String, Any>(),
                "expected_outputs" to emptyList<Any>(), "confidence" to 0.3, "metadata" to emptyMap<String, Any>()),
        ),
        "edges" to listOf(mapOf("from" to "a", "to" to "b", "reason" to "r")),
        "unresolved_inputs" to emptyList<Any>(), "confidence" to 0.6,
        "provenance" to mapOf("strategy" to "S", "generator" to null, "notes" to null),
    )

    private fun model(status: String = "draft", planId: String = "plan_1"): PlanInspectorState {
        transport.on("GET", base, body = mapOf("plan" to mapOf("plan_id" to planId, "status" to status, "plan_graph" to graphJSON)))
        return PlanInspectorState(PlanInspectorRoute("task_1"), PlanInspectorClient("task_1", transport))
    }

    @Test fun editing_is_open_for_draft_eliciting_and_approved_only() {
        for (status in listOf(PlanStatus.Draft, PlanStatus.Eliciting, PlanStatus.Approved)) {
            assertTrue(status.wire, PlanInspectorRules.canEdit(status, hasGraph = true))
        }
        for (status in listOf(PlanStatus.Planning, PlanStatus.Rejected, PlanStatus.Failed)) {
            assertFalse(status.wire, PlanInspectorRules.canEdit(status, hasGraph = true))
        }
        assertFalse(PlanInspectorRules.canEdit(PlanStatus.Draft, hasGraph = false))
        assertFalse(PlanInspectorRules.canEdit(null, hasGraph = true))
    }

    @Test fun approval_bar_only_for_a_draft_with_a_plan_id_outside_edit_mode() {
        assertTrue(PlanInspectorRules.showsApprovalBar(PlanStatus.Draft, "p", editing = false))
        assertFalse(PlanInspectorRules.showsApprovalBar(PlanStatus.Draft, "p", editing = true))
        assertFalse(PlanInspectorRules.showsApprovalBar(PlanStatus.Draft, null, editing = false))
        assertFalse(PlanInspectorRules.showsApprovalBar(PlanStatus.Approved, "p", editing = false))
    }

    @Test fun unsaved_guard_tracks_real_changes_only() = runBlocking<Unit> {
        val model = model()
        model.load()
        assertTrue(model.canEdit)
        model.beginEditing()
        assertTrue(model.isEditing)
        assertFalse("entering edit mode is not a change", model.isDirty)
        model.addStep()
        assertTrue(model.isDirty)
        assertEquals(3, model.shownGraph?.steps?.size)
        assertEquals("the saved plan is untouched", 2, model.graph?.steps?.size)
        model.discardEdits()
        assertFalse(model.isEditing)
        assertFalse(model.isDirty)
        assertEquals(2, model.shownGraph?.steps?.size)
    }

    @Test fun a_rejected_plan_cannot_enter_edit_mode() = runBlocking<Unit> {
        val model = model(status = "rejected")
        model.load()
        model.beginEditing()
        assertFalse(model.isEditing)
    }

    @Test fun save_puts_the_draft_leaves_edit_mode_and_reloads() = runBlocking<Unit> {
        val model = model()
        model.load()
        model.beginEditing()
        assertTrue(model.setDependency("b", "a", false))
        transport.on("PUT", base, body = mapOf("plan" to mapOf("plan_id" to "plan_1", "status" to "draft", "plan_graph" to graphJSON)))
        transport.on("GET", "$base/versions", body = mapOf("versions" to emptyList<Any>(), "has_more" to false))
        val saved = model.save("trim b")
        assertTrue(saved)
        assertFalse(model.isEditing)
        val put = transport.requests.first { it.method == "PUT" }
        assertEquals(JsonPrimitive("trim b"), put.body.jObj()?.get("label"))
        assertEquals(JsonArray(emptyList()), put.body.jObj()?.get("plan").jObj()?.get("edges"))
        assertEquals("Plan saved.", model.notice)
    }

    @Test fun a_failed_save_keeps_the_draft() = runBlocking<Unit> {
        val model = model()
        model.load()
        model.beginEditing()
        model.addStep()
        transport.on("PUT", base, status = 409, body = mapOf("error" to "plan_version_conflict"))
        val saved = model.save(null)
        assertFalse(saved)
        assertTrue(model.isEditing)
        assertTrue(model.isDirty)
        assertEquals("plan_version_conflict", model.errorMessage)
    }

    @Test fun restore_refetches_and_shows_steps() = runBlocking<Unit> {
        val model = model()
        model.load()
        transport.on("GET", "$base/versions/7", body = mapOf("version" to mapOf("plan_graph" to graphJSON)))
        transport.on("POST", "$base/versions/7/restore", body = mapOf("plan" to mapOf("plan_id" to "plan_1", "status" to "draft")))
        transport.on("GET", "$base/versions", body = mapOf("versions" to emptyList<Any>(), "has_more" to false))
        model.tab = PlanInspectorTab.History
        model.previewVersion(PlanVersionSummary(7, null, 2, null))
        assertEquals(2, model.previewGraph.value?.steps?.size)
        val restored = model.restorePreview()
        assertTrue(restored)
        assertEquals(PlanInspectorTab.Steps, model.tab)
        assertNull(model.preview)
        assertEquals(2, transport.calls.count { it == "GET $base" })
    }

    @Test fun approve_and_run_approves_then_starts_the_run() = runBlocking<Unit> {
        val model = model()
        model.load()
        transport.on("POST", "$base/approve", body = mapOf("ok" to true))
        transport.on("POST", "/api/magician/v3/tasks/task_1/execute", status = 202, body = mapOf("execution_id" to "e"))
        transport.on("GET", "/api/magician/v3/tasks/task_1/execution-panel", body = mapOf("debug" to emptyMap<String, Any>()))
        model.decide(PlanDecision.ApproveAndRun)
        val posts = transport.requests.filter { it.method == "POST" }
        assertEquals(listOf("$base/approve", "/api/magician/v3/tasks/task_1/execute"), posts.map { it.path })
        assertEquals(listOf("plan_id" to "plan_1"), posts.first().query)
        assertEquals(PlanApprovalOutcome.ApprovedAndStarted.message, model.notice)
    }

    @Test fun a_failed_start_keeps_the_approval_and_says_so() = runBlocking<Unit> {
        val model = model()
        model.load()
        transport.on("POST", "$base/approve", body = mapOf("ok" to true))
        transport.on("POST", "/api/magician/v3/tasks/task_1/execute", status = 409, body = mapOf("message" to "Already running"))
        model.decide(PlanDecision.ApproveAndRun)
        assertEquals(PlanApprovalOutcome.ApprovedRunNotStarted("Already running").message, model.notice)
    }

    @Test fun a_failed_approval_does_not_start_a_run() = runBlocking<Unit> {
        val model = model()
        model.load()
        transport.on("POST", "$base/approve", status = 409, body = mapOf("error" to "plan_version_conflict"))
        model.decide(PlanDecision.ApproveAndRun)
        assertFalse(transport.calls.any { it.endsWith("/execute") })
        assertEquals("plan_version_conflict", model.notice)
    }

    @Test fun run_marks_map_by_step_id_then_by_position() = runBlocking<Unit> {
        val model = model(status = "approved")
        model.load()
        model.apply(planObj("debug" to mapOf("selected_execution" to mapOf(
            "status" to "running",
            "step_statuses" to listOf(
                mapOf("number" to 1, "name" to "A", "status" to "completed", "step_id" to "a"),
                mapOf("number" to 2, "name" to "B", "status" to "in_progress", "step_id" to "unknown-id"),
            ),
        ))))
        assertEquals(mapOf("a" to PlanStepMark.Done, "b" to PlanStepMark.InProgress), model.runMarks)
        assertTrue(model.runActive)

        model.apply(planObj("debug" to mapOf("selected_execution" to mapOf(
            "status" to "completed",
            "step_statuses" to listOf(
                mapOf("number" to 1, "status" to "completed", "step_id" to "a"),
                mapOf("number" to 2, "status" to "pending", "step_id" to "b"),
            ),
        ))))
        assertFalse(model.runActive)
        assertEquals(PlanStepMark.NotReached, model.runMarks["b"])
    }

    @Test fun answering_a_question_clears_its_text_and_reloads() = runBlocking<Unit> {
        transport.on("GET", base, body = mapOf("plan" to mapOf("plan_id" to "plan_1", "status" to "eliciting",
            "plan_graph" to graphJSON, "pending_questions" to listOf(mapOf("id" to "q1", "question_text" to "Who?")))))
        val model = PlanInspectorState(PlanInspectorRoute("task_1"), PlanInspectorClient("task_1", transport))
        model.load()
        assertTrue(model.canResume)
        transport.on("POST", "/api/magician/v2/hitl/q1/respond", body = mapOf("accepted" to true))
        transport.on("GET", "$base/clarifications", body = mapOf("clarification_history" to emptyList<Any>()))
        model.answers["q1"] = "  Jordan "
        model.answer(model.pendingQuestions[0])
        val post = transport.requests.first { it.method == "POST" }
        assertEquals("Jordan", post.body.jObj()?.get("value").jObj()?.get("value").jStr())
        assertNull(model.answers["q1"])
        assertTrue(model.answering.isEmpty())
        assertEquals(PlanLoadable.Loaded(emptyList<PlanClarificationEntry>()), model.clarifications)
    }
}
