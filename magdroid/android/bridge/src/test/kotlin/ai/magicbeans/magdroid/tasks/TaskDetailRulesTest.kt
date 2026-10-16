package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The task detail screen's pure rules; cases mirror iOS `TaskDetailLayoutTests`. */
class TaskDetailRulesTest {
    @Test fun internalWorkUsesFullCorpusQueryAndProtectsAppOwnedCleanup() {
        val query = InternalTaskQuery(50, 100, TaskSortField.Agent, true, "crew/+one", "completed", "notes & quotes")
        assertEquals("/tasks/internal?limit=100&offset=50&sort=agent_id&order=asc&agent_id=crew%2F%2Bone&status=completed&query=notes+%26+quotes", query.requestPath())
        assertEquals("/tasks/internal?limit=500&offset=0&sort=updated_at&order=desc", InternalTaskQuery(-1, 900).requestPath())
        val app = TaskV3(id = "task_app_" + "a".repeat(64), title = "App", status = "completed", lifecycle = "internal")
        assertTrue(app.isAppManaged); assertFalse(app.canCleanUpInternal); assertTrue(app.trailingSwipeActions.isEmpty()); assertTrue(app.leadingSwipeActions.isEmpty()); assertFalse(TaskCardAction.Reset in app.visibleActions); assertFalse(TaskCardAction.RunNow in app.visibleActions)
        assertTrue(app.copy(id = "task_internal").canCleanUpInternal)
        assertEquals("installation.one", appInstallationForTask(app.id, "app:installation.one", "app_action"))
        assertNull(appInstallationForTask(app.id, "app:../escape", "app_action"))
        assertNull(appInstallationForTask(app.id, "app:installation.one", "user"))
        val url = ai.magicbeans.magdroid.work.WorkWebContext(task = app.id, execution = "run/+", internal = true, appInstallation = "installation.one").url("https://console.example", ai.magicbeans.magdroid.work.WorkSpecialist.Apps)!!
        assertTrue(url.startsWith("https://console.example/apps/installation.one?from=")); assertTrue(url.contains("execution_id%3Drun"))
    }
    @Test fun completedReferencesPreserveManualDependenciesAndExactTokens() {
        assertEquals("Use task:task_one_long\n\ntask:task_one task:task_two", taskReferenceDescription("Use task:task_one_long", listOf("task_one", "task_two", "task_two")))
        assertEquals("Use task:task_one", taskReferenceDescription("Use task:task_one", listOf("task_one")))
        assertEquals(listOf("manual", "task_one", "task_two"), taskReferenceDependencies(listOf(" manual ", "task_one", ""), listOf("task_one", "task_two")))
    }
    @Test fun olderRunPagesKeepExactIdsAndServerCursorWithoutDuplicates() {
        val first = json("""{"executions":[{"state":{"execution_id":"run-1"}},{"state":{"execution_id":"run-2"}}],"next_execution_cursor":"next/+"}""")
        val next = json("""{"executions":[{"state":{"execution_id":"run-2"}},{"state":{"execution_id":"run-3"}}]}""")
        val merged = mergeTaskExecutionHistory(first, next)
        assertEquals(3, (merged["executions"] as kotlinx.serialization.json.JsonArray).size)
        assertFalse(merged.containsKey("next_execution_cursor"))
    }

    @Test fun simpleTaskOwnersReadWrappedCrewAndExcludeDisabledOrWorkerDefinitions() {
        val value = Json.parseToJsonElement("""{"agents":[{"definition":{"agent_id":"custom-owner","name":"My primary","kind":"Personal","is_primary":true}},{"definition":{"agent_id":"worker","kind":"Worker"}},{"definition":{"agent_id":"disabled","kind":"Personal","disabled":true}},{"agent_id":"legacy","name":"Legacy"}]}""")
        assertEquals(listOf("custom-owner", "worker", "disabled", "legacy"), decodeTaskAgentOptions(value, personalOnly = false).map { it.id })
        val options = decodeTaskAgentOptions(value)
        assertEquals(listOf("custom-owner", "legacy"), options.map { it.id }); assertTrue(options.first().isPrimary)
        assertTrue(decodeTaskAgentOptions(Json.parseToJsonElement("{\"agents\":[]}")).isEmpty())
        assertTrue(runCatching { decodeTaskAgentOptions(Json.parseToJsonElement("{}")) }.isFailure)
    }

    @Test fun crewCycleUsesExactExecutionPanelWithoutTaskRequests() {
        val paths = taskDetailRequestPaths("agent-cycle:writer:cycle", "run/+&one")
        assertEquals(mapOf("run" to "/executions/run%2F%2B%26one/execution-panel"), paths)
        val panel = Json.parseToJsonElement("{\"overview\":{\"status\":\"running\"}}").jsonObject
        val bundle = assembleTaskDetail(mapOf("run" to Result.success(panel)))
        assertEquals(panel, bundle.panel); assertTrue(bundle.unavailableSections.isEmpty())
        assertEquals("/tasks/task%2Fone/execution-panel?execution_id=run%2F%2B%26one", taskDetailRequestPaths("task/one", "run/+&one")["run"])
        val cycle = TaskV3(id = "agent-cycle:writer:cycle", title = "Cycle", status = "completed")
        assertTrue(cycle.isExecutionOnly); assertFalse(cycle.canPublishToNotes); assertFalse(cycle.canConvertToMonitor)
        assertTrue(runCatching { taskDetailRequestPaths(cycle.id, null) }.isFailure)
        val record = Json.parseToJsonElement("{\"definition\":{\"agent_id\":\"writer\"},\"current_cycle_id\":\"cycle\",\"current_execution_id\":\"run/+&one\"}").jsonObject
        val member = ai.magicbeans.magdroid.work.WorkCrewMember.decode(record)
        assertEquals(cycle.id, member.cycleTaskId); assertEquals("run/+&one", member.executionId)
        assertNull(ai.magicbeans.magdroid.work.WorkCrewMember.decode(JsonObject(record - "current_execution_id")).cycleTaskId)
    }

    private fun json(text: String): JsonObject = Json.parseToJsonElement(text).jsonObject

    @Test
    fun `polls only while resumed and planning or running`() {
        assertTrue(TaskDetailRules.shouldPoll(resumed = true, status = "running", planStatus = null))
        assertTrue(TaskDetailRules.shouldPoll(resumed = true, status = "Planning", planStatus = null))
        assertTrue(TaskDetailRules.shouldPoll(resumed = true, status = "pending", planStatus = "planning"))
        assertTrue(TaskDetailRules.shouldPoll(resumed = true, status = "executing", planStatus = "approved"))
        assertFalse(TaskDetailRules.shouldPoll(resumed = false, status = "running", planStatus = null))
        assertFalse(TaskDetailRules.shouldPoll(resumed = true, status = "completed", planStatus = "approved"))
        assertFalse(TaskDetailRules.shouldPoll(resumed = true, status = "ready", planStatus = "draft"))
        assertFalse(TaskDetailRules.shouldPoll(resumed = true, status = null, planStatus = null))
        assertEquals(4_000L, TaskDetailRules.POLL_INTERVAL_MILLIS)
    }

    @Test
    fun `exactly one act opens, focus wins when the act exists`() {
        val all = listOf(TaskDetailAct.Plan, TaskDetailAct.Run, TaskDetailAct.Output)
        assertEquals(TaskDetailAct.Output, TaskDetailRules.initialOpenAct(all, TaskVerdictState.Finished, null, null))
        assertEquals(TaskDetailAct.Run, TaskDetailRules.initialOpenAct(all, TaskVerdictState.Running, null, null))
        assertEquals(TaskDetailAct.Plan, TaskDetailRules.initialOpenAct(all, TaskVerdictState.Queued, null, null))
        assertEquals(TaskDetailAct.Plan, TaskDetailRules.initialOpenAct(all, TaskVerdictState.Finished, null, TaskDetailAct.Plan))
        // A focus on an act the task lacks falls back to the default.
        val noPlan = listOf(TaskDetailAct.Run, TaskDetailAct.Output)
        assertEquals(TaskDetailAct.Output, TaskDetailRules.initialOpenAct(noPlan, TaskVerdictState.Finished, null, TaskDetailAct.Plan))
        // A plan approval waiting opens the Plan act.
        assertEquals(
            TaskDetailAct.Plan,
            TaskDetailRules.initialOpenAct(all, TaskVerdictState.Waiting, TaskAttentionSource.PlanApproval, null),
        )
    }

    @Test
    fun `toggling keeps at most one act open`() {
        assertEquals(TaskDetailAct.Run, TaskDetailRules.toggle(null, TaskDetailAct.Run))
        assertNull(TaskDetailRules.toggle(TaskDetailAct.Run, TaskDetailAct.Run))
        assertEquals(TaskDetailAct.Plan, TaskDetailRules.toggle(TaskDetailAct.Run, TaskDetailAct.Plan))
        assertTrue(TaskDetailRules.keepsSummaryWhenOpen(TaskDetailAct.Plan))
        assertFalse(TaskDetailRules.keepsSummaryWhenOpen(TaskDetailAct.Run))
    }

    @Test
    fun `brief is the first paragraph without markdown markers`() {
        assertEquals("A one-page brief for the team.", TaskDetailRules.brief("## A one-page **brief** for the team.\n\nMore detail."))
        assertEquals("line one line two", TaskDetailRules.brief("line one\nline two"))
        assertNull(TaskDetailRules.brief("   "))
        assertNull(TaskDetailRules.brief(null))
    }

    @Test
    fun `ids and names shorten for the chips and rows`() {
        assertEquals("fixture-deta…", TaskDetailRules.shortId("fixture-detail-done"))
        assertEquals("short-id", TaskDetailRules.shortId("short-id"))
        val long = "launch-brief-for-the-mobile-app-final.md"
        val shown = TaskDetailRules.middleEllipsize(long, 24)
        assertEquals(24, shown.length)
        assertTrue(shown.startsWith("launch-brief"))
        assertTrue(shown.endsWith("final.md"))
        assertTrue(shown.contains("…"))
        assertEquals("notes.md", TaskDetailRules.middleEllipsize("notes.md", 24))
    }

    @Test
    fun `previewable deliverables and their meta`() {
        assertTrue(TaskDetailRules.previewable(null, "outputs/brief.md"))
        assertTrue(TaskDetailRules.previewable("image/png", "outputs/chart"))
        assertTrue(TaskDetailRules.previewable(null, "data.csv"))
        assertTrue(TaskDetailRules.previewable("application/json", "x"))
        assertFalse(TaskDetailRules.previewable("application/pdf", "report.pdf"))
        assertFalse(TaskDetailRules.previewable(null, "archive.zip"))
        val files = listOf("a.zip", "b.pdf", "c.md", "d.png")
        assertEquals("c.md", TaskDetailRules.defaultPreview(files, { null }, { it }))
        assertEquals("Markdown · 5 KB", TaskDetailRules.fileMeta(null, "brief.md", 5_120))
        assertEquals("Text · 912 bytes", TaskDetailRules.fileMeta(null, "numbers.csv", 912))
        assertEquals("Image · 1.2 MB", TaskDetailRules.fileMeta("image/png", "chart.png", 1_234_567))
        assertEquals("File", TaskDetailRules.fileMeta(null, "blob.bin", null))
    }

    @Test
    fun `copy path prefers the absolute path`() {
        val file = TaskOutputFile(id = "1", relativePath = "outputs/a.md", absolutePath = "/Users/me/tasks/t/outputs/a.md")
        assertEquals("/Users/me/tasks/t/outputs/a.md", TaskDetailRules.copyPath(file))
        assertEquals("outputs/a.md", TaskDetailRules.copyPath(file.copy(absolutePath = null)))
    }

    @Test
    fun `plan graph steps come from the plan payload in graph order`() {
        val payload = json(
            """{"plan":{"status":"approved","plan_graph":{"steps":[
                {"id":"s1","task":"Gather the facts"},
                {"id":"s2","metadata":{"description":"Draft the brief"}},
                {"id":"s3"}
            ]}}}""",
        )
        assertEquals(listOf("Gather the facts", "Draft the brief", "Step s3"), TaskDetailRules.planGraphSteps(payload))
        assertEquals(listOf("A"), TaskDetailRules.planGraphSteps(json("""{"plan_graph":{"steps":[{"task":"A"}]}}""")))
        assertEquals(emptyList<String>(), TaskDetailRules.planGraphSteps(json("""{"plan":{"plan_graph":null}}""")))
        assertEquals(emptyList<String>(), TaskDetailRules.planGraphSteps(null))
    }

    @Test
    fun `plan steps fall back from the graph to the run rows`() {
        val rows = listOf(PlanStepRow(1, "From run", PlanStepMark.Done, null))
        assertEquals(listOf("G"), TaskDetailRules.planSteps(listOf("G"), rows, listOf("S")))
        assertEquals(listOf("From run"), TaskDetailRules.planSteps(emptyList(), rows, listOf("S")))
        assertEquals(listOf("S"), TaskDetailRules.planSteps(emptyList(), emptyList(), listOf("S")))
    }

    @Test
    fun `a run has ended with an end time or a terminal status`() {
        assertTrue(TaskDetailRules.runHasEnded("running", "2026-10-01T10:00:00Z"))
        assertTrue(TaskDetailRules.runHasEnded("failed", null))
        assertTrue(TaskDetailRules.runHasEnded("Canceled", null))
        assertFalse(TaskDetailRules.runHasEnded("running", null))
        assertFalse(TaskDetailRules.runHasEnded(null, ""))
    }

    @Test
    fun `draft plans offer the decision while the task can still run`() {
        assertTrue(TaskDetailRules.showsDraftActions("draft", "pending"))
        assertTrue(TaskDetailRules.showsDraftActions("Draft", "paused"))
        assertFalse(TaskDetailRules.showsDraftActions("draft", "completed"))
        assertFalse(TaskDetailRules.showsDraftActions("approved", "ready"))
        assertTrue(TaskDetailRules.planningActivityIsLive("planning"))
        assertFalse(TaskDetailRules.planningActivityIsLive("draft"))
    }

    @Test
    fun `run act line uses the live step summary and closes with usage`() {
        val rows = listOf(
            PlanStepRow(1, "Gather", PlanStepMark.Done, null),
            PlanStepRow(2, "Draft", PlanStepMark.InProgress, null),
            PlanStepRow(3, "Check", PlanStepMark.Pending, null),
        )
        val input = RunSummaryInput(steps = 3, retries = 0, elapsedSeconds = 600.0, currentStep = null, totalSteps = 3, events = 3)
        assertEquals("Step 2 of 3 · Draft", TaskDetailRules.runActSummary(rows, live = true, input = input, usage = null))
        assertEquals("3 steps · 3 events · 10m", TaskDetailRules.runActSummary(rows, live = false, input = input, usage = null))
        assertEquals(
            "Step 2 of 3 · Draft · 2 calls · 1.5k tok · $0.040",
            TaskDetailRules.runActSummary(rows, live = true, input = input, usage = TaskUsage(2, 1_500, 0.04)),
        )
        assertEquals("Plan · approved 15m ago", TaskDetailRules.planChipText("approved 15m ago"))
        assertEquals("Plan", TaskDetailRules.planChipText(""))
    }

    private fun menu(
        status: String,
        plan: String? = null,
        hasPlan: Boolean = plan != null,
        live: Boolean = false,
        synthesisFailed: Boolean = false,
    ) = TaskDetailRules.menuActions(TaskDetailMenuInput(status, plan, hasPlan, live, synthesisFailed, synthesisPending = false))

    @Test
    fun `menu offers the lifecycle actions the old header buttons had`() {
        assertEquals(
            listOf(TaskDetailAction.Run, TaskDetailAction.Plan, TaskDetailAction.MarkComplete, TaskDetailAction.CancelTask, TaskDetailAction.Delete),
            menu("pending"),
        )
        assertEquals(
            listOf(TaskDetailAction.Run, TaskDetailAction.Replan, TaskDetailAction.MarkComplete, TaskDetailAction.CancelTask, TaskDetailAction.Delete),
            menu("ready", plan = "approved"),
        )
        // A draft is decided first: no Run until it is approved.
        val draft = menu("pending", plan = "draft")
        assertFalse(TaskDetailAction.Run in draft)
        assertTrue(TaskDetailAction.ApprovePlan in draft)
        assertTrue(TaskDetailAction.RejectPlan in draft)
        // A live run: no Run, no Reset, no Mark complete.
        val running = menu("running", plan = "approved", live = true)
        assertFalse(TaskDetailAction.Run in running)
        assertFalse(TaskDetailAction.MarkComplete in running)
        assertTrue(TaskDetailAction.CancelTask in running)
        // Finished: Mark not done; no Cancel.
        val done = menu("completed", plan = "approved")
        assertTrue(TaskDetailAction.MarkNotDone in done)
        assertFalse(TaskDetailAction.CancelTask in done)
        // Failed: Reset, and Retry synthesis when that failed.
        val failed = menu("failed", synthesisFailed = true)
        assertTrue(TaskDetailAction.Reset in failed)
        assertTrue(TaskDetailAction.RetrySynthesis in failed)
        // Planning: no Replan mid-plan.
        assertFalse(TaskDetailAction.Replan in menu("planning", plan = "planning"))
        assertEquals(TaskDetailAction.Delete, menu("completed").last())
    }
}
