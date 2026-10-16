package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.tasks.TaskUsage
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The live task card's plumbing: output blocks as the chat wire writes them,
 * the transcript's one-card-per-task coalescing, the Steps helpers, and the
 * card loader's gating (poll only while planning/running and on screen, plan
 * once, store status at most once per interval).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class LiveTaskCardPlumbingTest {
    private fun obj(text: String): JsonObject = Json.parseToJsonElement(text).jsonObject

    private fun dto(text: String): ChatMessageDto = chatJson.decodeFromString(ChatMessageDto.serializer(), text)

    // ── Output blocks ──

    @Test
    fun `chat output blocks carry path, size, display name and source`() {
        val message = dto(
            """
            {"id":"m1","direction":"system","created_at":1759300000000,
             "content":{"type":"task_status_update","task_id":"task_1","status":"completed",
               "output_files":[
                 {"type":"file","relative_path":"outputs/brief.md","absolute_path":"/Users/x/brief.md",
                  "display_name":"brief.md","mime_type":"text/markdown","size":18432,
                  "source":{"type":"session_output","task_id":"task_1"}},
                 {"type":"file","relative_path":"a/b/chart.png","size":12.0},
                 {"type":"file","filename":"x.txt","size":"77"},
                 {"type":"file","filename":"y.txt","size":null}
               ]}}
            """,
        ).project(0)
        val outputs = message.task!!.outputs
        val first = outputs[0]
        assertEquals("outputs/brief.md", first.relativePath)
        assertEquals("/Users/x/brief.md", first.absolutePath)
        assertEquals("brief.md", first.displayName)
        assertEquals(18432L, first.size)
        assertEquals("session_output", first.source?.type)
        assertEquals("task_1", first.source?.taskId)
        assertEquals("brief.md", first.name())
        assertEquals(12L, outputs[1].size)
        assertEquals("chart.png", outputs[1].name())
        assertEquals(77L, outputs[2].size)
        assertNull(outputs[3].size)
        assertEquals(1759300000000L, message.createdAtMs)
    }

    @Test
    fun `a created_at in seconds still dates the message`() {
        val message = dto("""{"id":"m","direction":"assistant","created_at":1759300000,"content":{"type":"text","text":"hi"}}""").project(0)
        assertEquals(1759300000000L, message.createdAtMs)
    }

    @Test
    fun `a local echo is dated now`() {
        val before = System.currentTimeMillis()
        val echo = optimisticUserMessage("l", "hi", false)
        assertTrue((echo.createdAtMs ?: 0) >= before)
    }

    @Test
    fun `an output is read from its own url, else its owner's outputs`() {
        val base = "https://mac.example/"
        val session = ContentBlockRecord(relativePath = "outputs/My Brief.md", source = ContentBlockSource("session_output", "task_1"))
        val sessionHome = LiveTaskCardRules.outputHome(session.source?.type, session.source?.taskId, "task_1", "sess 1")
        assertEquals(
            "https://mac.example/api/magician/v2/chat/sessions/sess%201/outputs/My%20Brief.md",
            LiveTaskOutputLinks.url(base, session, sessionHome),
        )
        val task = ContentBlockRecord(relativePath = "/a/../b/c.png", source = ContentBlockSource("task_output", null))
        val taskHome = LiveTaskCardRules.outputHome("task_output", null, "task_1", "sess")
        assertEquals("https://mac.example/api/magician/v3/tasks/task_1/outputs/a/b/c.png", LiveTaskOutputLinks.url(base, task, taskHome))
        assertEquals("https://mac.example/api/x.png", LiveTaskOutputLinks.url(base, ContentBlockRecord(url = "/api/x.png"), null))
        assertEquals("https://cdn/x.png", LiveTaskOutputLinks.url(base, ContentBlockRecord(url = "https://cdn/x.png"), null))
        assertNull(LiveTaskOutputLinks.url(base, ContentBlockRecord(relativePath = "x.md"), null))
        assertNull(LiveTaskOutputLinks.url(base, ContentBlockRecord(filename = "x.md"), taskHome))
    }

    // ── Transcript coalescing ──

    private fun card(id: String, status: String, at: Long, store: String? = null) = ChatMessage(
        id = id, fromUser = false, text = "", kind = MessageKind.TaskStatus,
        task = TaskStatusCard(
            taskId = "task_1", title = "Write the brief", status = status,
            startedAtMs = at, updatedAtMs = at, storeStatus = store, uiThreadId = if (id == "a") "sess" else null,
        ),
    )

    @Test
    fun `one card per task keeps the earliest start across updates`() {
        val merged = coalesceTaskStatusMessages(
            listOf(card("a", "planning", 1_000, store = "planning"), card("b", "running", 5_000), card("c", "completed", 9_000)),
        )
        assertEquals(1, merged.size)
        val task = merged.single().task!!
        assertEquals("completed", task.status)
        assertEquals(1_000L, task.startedAtMs)
        assertEquals(9_000L, task.updatedAtMs)
        assertEquals("planning", task.storeStatus)
        assertEquals("sess", task.uiThreadId)
    }

    @Test
    fun `a realtime update arriving out of order still keeps the earliest start`() {
        val list = mergeRealtimeMessage(listOf(card("b", "running", 5_000)), card("a", "planning", 1_000))
        assertEquals(1_000L, list.single().task!!.startedAtMs)
    }

    // ── Steps helpers ──

    @Test
    fun `the inline steps show the newest five`() {
        val rows = (1..8).map { ActivityRow(label = "s$it") }
        assertEquals(listOf("s4", "s5", "s6", "s7", "s8"), activityPreviewRows(rows).map { it.label })
        assertEquals(emptyList<ActivityRow>(), activityPreviewRows(rows, 0))
    }

    @Test
    fun `the inspect target needs both a task and an execution, read independently`() {
        assertNull(activityInspectTarget(listOf(ActivityRow(label = "a", executionId = "exec_1"))))
        val target = activityInspectTarget(
            listOf(
                ActivityRow(label = "a", taskId = "task_old", executionId = "exec_old"),
                ActivityRow(label = "b", taskId = "task_new"),
                ActivityRow(label = "c", executionId = "exec_new"),
            ),
        )
        assertEquals(ActivityInspectTarget("task_new", "exec_new"), target)
    }

    @Test
    fun `a finished model call's row carries its usage`() {
        val row = ActivityRow(
            label = "Thinking", detail = "Chose the researcher", status = "done", kind = "llm",
            callUsage = ActivityCallUsage(stepTokens = 9100, costUsd = 0.0123, reportedTokens = true),
        )
        assertEquals("Chose the researcher · 9.1k tok · $0.012", activityRowDetail(row))
        assertEquals("Chose the researcher", activityRowDetail(row.copy(status = "running")))
        assertNull(activityRowDetail(ActivityRow(label = "x")))
        assertEquals("1.8s", activityLatencyText(1_800.0))
        assertEquals("920ms", activityLatencyText(920.0))
    }

    @Test
    fun `steps rows carry the files and tone the accumulator saw`() {
        val accumulator = ChatTurnActivityAccumulator()
        accumulator.ingest(
            obj(
                """{"event_type":"AgentEvent","data":{"event":{"event_type":"artifact.created","agent_id":"magician",
                   "payload":{"output_files":[{"absolute_path":"/tmp/out/report.md","label":"report.md"}]}}}}""",
            ),
        )
        val row = accumulator.snapshot().single()
        assertEquals(listOf(ActivityFileRef("/tmp/out/report.md", "report.md")), row.files)
        assertEquals("tool", row.tone)
    }

    // ── Activity lines ──

    @Test
    fun `activity lines prefer the full log, then recent activity`() {
        assertEquals(
            listOf("Searching", "Reading"),
            LiveTaskActivity.lines(obj("""{"run":{"activity_log":[{"title":"Searching"},{"content":"Reading"},{"title":" "}],"recent_activity":[{"title":"x"}]}}""")),
        )
        assertEquals(listOf("x"), LiveTaskActivity.lines(obj("""{"run":{"recent_activity":[{"title":"x"}]}}""")))
        assertEquals(emptyList<String>(), LiveTaskActivity.lines(obj("{}")))
        assertEquals(listOf("c", "d", "e", "f"), LiveTaskActivity.recent(listOf("a", "b", "c", "d", "e", "f")))
    }

    // ── Loader gating ──

    private class FakeApi {
        val calls = mutableListOf<String>()
        var status = "running"
        fun fetch(path: String): JsonObject? {
            calls += path
            return when {
                path.endsWith("/execution-panel") -> Json.parseToJsonElement(
                    """{"run":{"activity_log":[{"title":"Reading reviews","metadata":{"input_tokens":10,"output_tokens":5}}]},
                        "debug":{"selected_execution":{"step_statuses":[{"name":"Search","status":"completed"},{"name":"Write","status":"in_progress"}]}}}""",
                ).jsonObject
                path.endsWith("/plan") -> Json.parseToJsonElement(
                    """{"plan":{"plan_id":"plan_1","status":"draft","plan_graph":{"steps":[{"id":"1","task":"Search"},{"id":"2","task":"Write"}]}}}""",
                ).jsonObject
                else -> Json.parseToJsonElement(
                    """{"task":{"state":{"status":"$status","active_root_execution_id":"exec_9"}}}""",
                ).jsonObject
            }
        }
        fun count(suffix: String) = calls.count { it.endsWith(suffix) }
    }

    private fun TestScope.loader(api: FakeApi, usage: TaskUsageStore = TaskUsageStore()) =
        LiveTaskCardLoader(this, { api.fetch(it) }, usage) { testScheduler.currentTime }

    @Test
    fun `a running card on screen polls every four seconds and stops when hidden`() = runTest {
        val api = FakeApi()
        val usage = TaskUsageStore()
        val loader = loader(api, usage)
        loader.sync("task_1", LiveTaskCardPhase.Running, visible = true, appActive = true, wantsPlan = false, terminal = false, usagePhase = "running")
        runCurrent()
        assertTrue(loader.isPolling)
        assertEquals(1, api.count("/execution-panel"))
        advanceTimeBy(4_001)
        assertEquals(2, api.count("/execution-panel"))
        val feed = loader.feed.value
        assertEquals(listOf("Reading reviews"), feed.activityLines)
        assertEquals(2, feed.stepRows.size)
        assertEquals("running", feed.storeStatus)
        assertEquals("exec_9", feed.activeRootExecutionId)
        assertEquals(TaskUsage(1, 15, null), usage.total("task_1")?.usage)

        loader.sync("task_1", LiveTaskCardPhase.Running, visible = false, appActive = true, wantsPlan = false, terminal = false, usagePhase = "running")
        assertFalse(loader.isPolling)
        advanceTimeBy(20_000)
        assertEquals(2, api.count("/execution-panel"))
    }

    @Test
    fun `no poll in the background, for a finished card, or without a task id`() = runTest {
        val api = FakeApi()
        val loader = loader(api)
        loader.sync("task_1", LiveTaskCardPhase.Running, visible = true, appActive = false, wantsPlan = false, terminal = false, usagePhase = "running")
        assertFalse(loader.isPolling)
        loader.sync("task_1", LiveTaskCardPhase.Finished, visible = true, appActive = true, wantsPlan = false, terminal = true, usagePhase = "done")
        assertFalse(loader.isPolling)
        loader.sync(null, LiveTaskCardPhase.Running, visible = true, appActive = true, wantsPlan = false, terminal = false, usagePhase = "running")
        assertFalse(loader.isPolling)
        runCurrent()
        // The two visible non-polling syncs read the stored usage once per phase.
        assertEquals(2, api.count("/execution-panel"))
    }

    @Test
    fun `the plan loads once however often the card syncs`() = runTest {
        val api = FakeApi().apply { status = "ready" }
        val loader = loader(api)
        repeat(3) {
            loader.sync("task_1", LiveTaskCardPhase.NotStarted, visible = true, appActive = true, wantsPlan = true, terminal = false, usagePhase = "planned")
            runCurrent()
        }
        assertEquals(1, api.count("/plan"))
        assertEquals(LiveTaskPlan("plan_1", "draft", listOf("Search", "Write")), loader.feed.value.plan)
        // The stored status is read at most once per interval…
        assertEquals(1, api.calls.count { it == "/tasks/task_1" })
        assertEquals("ready", loader.feed.value.storeStatus)
        // …and the usage once per phase.
        assertEquals(1, api.count("/execution-panel"))
        advanceTimeBy(4_000)
        loader.sync("task_1", LiveTaskCardPhase.NotStarted, visible = true, appActive = true, wantsPlan = true, terminal = false, usagePhase = "planned")
        runCurrent()
        assertEquals(2, api.calls.count { it == "/tasks/task_1" })
    }

    @Test
    fun `store wins over a card stuck at draft`() = runTest {
        val api = FakeApi().apply { status = "ready" }
        val loader = loader(api)
        loader.refreshStoreStatus("task_1")
        runCurrent()
        val task = TaskStatusCard(taskId = "task_1", title = "Brief", status = "draft", summary = "Plan drafted")
        val reconciled = LiveTaskCardRules.reconciled(task, loader.feed.value.storeStatus!!)
        val state = LiveTaskCardState.of(LiveTaskCardRules.input(reconciled, "m1"))
        assertEquals(LiveTaskStatus.Queued, state.status)
        assertTrue(state.startable)
    }

    @Test
    fun `the usage store reads a phase once and retries after a failed read`() = runTest {
        var fail = true
        var reads = 0
        val store = TaskUsageStore().apply {
            fetch = {
                reads += 1
                if (fail) null else Json.parseToJsonElement("""{"debug":{"planning_timeline":[{"tokens":120,"cost_usd":0.01}]}}""").jsonObject
            }
        }
        assertFalse(store.loadSnapshot("task_1", "done"))
        fail = false
        assertTrue(store.loadSnapshot("task_1", "done"))
        assertFalse(store.loadSnapshot("task_1", "done"))
        assertEquals(2, reads)
        assertEquals(TaskUsage(1, 120, 0.01), store.total("task_1")?.usage)
        assertEquals(1L, store.revision.value)
    }

    // ── Plan glyph ──

    @Test
    fun `the plan glyph shows for any sign of a plan`() {
        assertFalse(LiveTaskPlanGlyph.shows("running", null, storeHasPlan = false, planStepCount = 0, stepRowCount = 0))
        assertTrue(LiveTaskPlanGlyph.shows("draft", null, false, 0, 0))
        assertTrue(LiveTaskPlanGlyph.shows("running", "eliciting", false, 0, 0))
        assertTrue(LiveTaskPlanGlyph.shows("completed", "completed", true, 0, 0))
        assertTrue(LiveTaskPlanGlyph.shows("completed", null, false, 2, 0))
        assertTrue(LiveTaskPlanGlyph.shows("running", null, false, 0, 3))
    }

    @Test
    fun `the loader reads the store record's plan`() = runTest {
        val loader = LiveTaskCardLoader(this, { path ->
            if (path == "/tasks/task_1") Json.parseToJsonElement(
                """{"task":{"state":{"status":"completed"},"manifest":{"latest_plan_id":"plan_9"}}}""",
            ).jsonObject else null
        }, TaskUsageStore()) { testScheduler.currentTime }
        loader.refreshStoreStatus("task_1")
        runCurrent()
        assertTrue(loader.feed.value.storeHasPlan)
        val card = TaskStatusCard(taskId = "task_1", title = "Brief", status = "completed")
        assertTrue(LiveTaskPlanGlyph.shows(card, loader.feed.value))
    }
}
