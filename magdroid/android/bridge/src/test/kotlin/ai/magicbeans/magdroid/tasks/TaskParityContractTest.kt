package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.net.ConnectException
import java.net.SocketTimeoutException

class TaskParityContractTest {
    private fun task(
        id: String = "task-1",
        status: String = "pending",
        title: String = "A task",
        tags: List<TaskTag> = emptyList(),
        due: String? = null,
        agent: String = "personal-assistant",
        lifecycle: String? = null,
        planStatus: String? = null,
        hasPlan: Boolean = false,
        planId: String? = null,
        question: TaskPendingQuestion? = null,
        activeExecution: String? = null,
        updated: String = "2026-08-10T09:00:00Z",
    ) = TaskV3(
        id = id, title = title, status = status, tags = tags, dueDate = due,
        agentId = agent, lifecycle = lifecycle, planStatus = planStatus,
        hasPlan = hasPlan, latestPlanId = planId, pendingQuestion = question,
        activeRootExecutionId = activeExecution, updatedAt = updated,
    )

    @Test fun tolerant_task_decode_keeps_older_rows_usable() {
        val decoded = taskJson.decodeFromString<TaskV3>(
            """{"id":"old","title":"Older","status":"pending"}""",
        )
        assertEquals("", decoded.description)
        assertEquals("general", decoded.uiThreadId)
        assertEquals(emptyList<TaskTag>(), decoded.tags)
        assertEquals(0, decoded.monitorRevision)
    }

    @Test fun active_control_target_is_only_the_backend_active_root() {
        assertEquals("exec-live", task(status = "running", activeExecution = "exec-live").activeExecutionIdForControls)
        assertNull(task(status = "completed", activeExecution = "exec-stale").activeExecutionIdForControls)
        assertNull(task(status = "running").activeExecutionIdForControls)
    }

    @Test fun manual_completion_never_races_an_active_execution() {
        for (status in listOf("queued", "running", "planning", "paused", "executing")) {
            assertFalse(status, task(status = status).canMarkCompleteManually)
        }
        assertTrue(task(status = "ready").canMarkCompleteManually)
    }

    @Test fun card_action_matrix_matches_ios() {
        assertEquals(listOf(TaskCardAction.Preplan, TaskCardAction.RunNow), task(status = "pending").visibleActions)
        assertEquals(listOf(TaskCardAction.RunNow), task(status = "ready").visibleActions)
        assertEquals(
            listOf(TaskCardAction.ViewQuestion, TaskCardAction.Reset),
            task(status = "paused", question = TaskPendingQuestion(question = "Choose" )).visibleActions,
        )
        // Failed with no execution has nothing to reset; Publish becomes the primary.
        assertEquals(listOf(TaskCardAction.PublishToNotes), task(status = "failed").visibleActions)
        assertEquals(
            listOf(TaskCardAction.Reset, TaskCardAction.PublishToNotes),
            task(status = "failed").copy(latestRootExecutionId = "exec-1").visibleActions,
        )
        assertEquals(
            listOf(TaskCardAction.ReviewPlan),
            task(status = "pending", hasPlan = true, planStatus = "draft", planId = "p").visibleActions,
        )
        assertEquals(
            listOf(TaskCardAction.RunPlan),
            task(status = "ready", hasPlan = true, planStatus = "approved", planId = "p").visibleActions,
        )
    }

    @Test fun swipe_matrix_does_not_duplicate_reset() {
        assertEquals(listOf(TaskSwipeAction.MarkComplete), task(status = "pending").leadingSwipeActions)
        assertEquals(emptyList<TaskSwipeAction>(), task(status = "failed").leadingSwipeActions)
        assertEquals(listOf(TaskSwipeAction.MarkNotDone), task(status = "completed").leadingSwipeActions)
        assertEquals(listOf(TaskSwipeAction.Delete), task(status = "completed").trailingSwipeActions)
        assertEquals(listOf(TaskSwipeAction.Cancel, TaskSwipeAction.Delete), task(status = "ready").trailingSwipeActions)
    }

    @Test fun published_notes_are_terminal_only() {
        assertTrue(task(status = "completed").canPublishToNotes)
        assertTrue(task(status = "failed").canPublishToNotes)
        assertTrue(task(status = "cancelled").canPublishToNotes)
        assertFalse(task(status = "running").canPublishToNotes)
    }

    @Test fun internal_lifecycle_badges_preserve_origin() {
        assertEquals("persistent", task().lifecycleLabel)
        assertEquals("internal", task(lifecycle = "internal").lifecycleLabel)
        assertEquals("debug", task(lifecycle = "internal").copy(createdBy = "__system__").lifecycleLabel)
        assertEquals("chat", task(lifecycle = "ephemeral_owned_by_chat").copy(chatSessionId = "chat-1").lifecycleLabel)
    }

    @Test fun only_persistent_non_monitors_can_convert() {
        assertTrue(task().canConvertToMonitor)
        assertFalse(task(lifecycle = "internal").canConvertToMonitor)
        assertFalse(task().copy(monitorRevision = 1).canConvertToMonitor)
    }

    @Test fun schedule_projection_reads_external_cron_tag_and_retention() {
        val scheduled = task().copy(schedule = buildJsonObject {
            put("timezone", "Asia/Kolkata")
            putJsonObject("kind") { putJsonObject("Cron") { put("expression", "0 9 * * *"); put("timezone", "Asia/Kolkata") } }
            putJsonObject("execution_history_retention") { put("max_records", 12); put("max_age_days", 30) }
        })
        assertEquals("0 9 * * *", scheduled.scheduleCron)
        assertEquals("Asia/Kolkata", scheduled.scheduleTimezone)
        assertEquals(12, scheduled.scheduleRetentionMaxRecords)
        assertEquals(30, scheduled.scheduleRetentionMaxDays)
        assertEquals("Cron 0 9 * * * (Asia/Kolkata)", scheduled.keptScheduleSummary)
    }

    @Test fun preset_filters_have_the_same_boundaries_as_ios() {
        val date = "2026-08-10"
        assertTrue(TaskFilter.All.matches(task(status = "ready"), date))
        assertFalse(TaskFilter.All.matches(task(status = "completed"), date))
        assertTrue(TaskFilter.Inbox.matches(task(status = "pending"), date))
        assertFalse(TaskFilter.Inbox.matches(task(status = "pending", tags = listOf(TaskTag("x", "x"))), date))
        assertTrue(TaskFilter.Today.matches(task(due = "2026-08-10T12:00:00Z"), date))
        assertTrue(TaskFilter.Overdue.matches(task(due = "2026-08-09"), date))
        assertFalse(TaskFilter.Overdue.matches(task(status = "completed", due = "2026-08-09"), date))
        assertTrue(TaskFilter.Running.matches(task(status = "paused"), date))
    }

    @Test fun server_answered_view_is_not_filtered_again_locally() {
        val surprisingButAuthoritative = task(status = "ready", id = "server-row")
        val state = TasksUiState(
            tasks = listOf(surprisingButAuthoritative),
            filter = TaskFilter.Overdue,
            loadedView = TaskFilter.Overdue,
        )
        assertEquals(listOf("server-row"), state.visibleTasks("2026-08-10").map(TaskV3::id))
    }

    @Test fun stale_page_is_locally_filtered_during_filter_round_trip() {
        val overdue = task(id = "old", due = "2026-08-09")
        val future = task(id = "future", due = "2026-08-11")
        val state = TasksUiState(
            tasks = listOf(overdue, future), filter = TaskFilter.Overdue, loadedView = TaskFilter.All,
        )
        assertEquals(listOf("old"), state.visibleTasks("2026-08-10").map(TaskV3::id))
    }

    @Test fun tag_and_preset_are_mutually_exclusive_and_search_stacks() {
        val one = task(id = "one", title = "Launch brief", tags = listOf(TaskTag("launch", "launch")))
        val two = task(id = "two", title = "Other", tags = listOf(TaskTag("finance", "finance")))
        val state = TasksUiState(
            tasks = listOf(one, two), selectedTag = "launch", filter = TaskFilter.Completed,
            searchQuery = "brief",
        )
        assertEquals(listOf("one"), state.visibleTasks("2026-08-10").map(TaskV3::id))
    }

    @Test fun internal_status_and_agent_filters_apply_to_the_internal_page() {
        val a = task(id = "a", status = "running", agent = "alpha", lifecycle = "internal")
        val b = task(id = "b", status = "running", agent = "beta", lifecycle = "internal")
        val state = TasksUiState(
            internalTasks = listOf(a, b), lane = TaskLane.Internal,
            filter = TaskFilter.Running, internalAgentFilter = "beta",
        )
        assertEquals(listOf("b"), state.visibleTasks("2026-08-10").map(TaskV3::id))
    }

    @Test fun internal_lane_does_not_inherit_regular_task_preset_or_tag() {
        val state = TasksUiState(
            internalTasks = listOf(
                task(id = "done", status = "completed", lifecycle = "internal"),
                task(id = "failed", status = "failed", lifecycle = "internal"),
            ),
            lane = TaskLane.Internal,
            filter = TaskFilter.Running,
            selectedTag = "regular-task-only",
        )
        assertEquals(setOf("done", "failed"), state.visibleTasks("2026-08-10").map(TaskV3::id).toSet())
        assertEquals(
            listOf("done"),
            state.copy(internalStatusFilter = "completed").visibleTasks("2026-08-10").map(TaskV3::id),
        )
    }

    @Test fun sort_defaults_to_updated_descending_and_toggles() {
        val old = task(id = "old", updated = "2026-08-09T00:00:00Z")
        val fresh = task(id = "fresh", updated = "2026-08-10T00:00:00Z")
        assertEquals(listOf("fresh", "old"), TasksUiState(tasks = listOf(old, fresh)).visibleTasks().map(TaskV3::id))
        assertEquals(
            listOf("old", "fresh"),
            TasksUiState(tasks = listOf(old, fresh), sortAscending = true).visibleTasks().map(TaskV3::id),
        )
    }

    @Test fun unknown_counts_remain_unknown_but_reported_zero_is_visible() {
        assertNull(TasksUiState().laneCount(TaskFilter.All))
        assertEquals(0, TasksUiState(laneCounts = mapOf("all" to 0)).laneCount(TaskFilter.All))
    }

    @Test fun load_errors_are_scoped_to_the_selected_lane() {
        val tasksError = TaskUserError("Tasks unavailable", "Try again.")
        val internalError = TaskUserError("Internal tasks unavailable", "Try again.")
        val state = TasksUiState(loadErrors = mapOf(
            TaskLane.Tasks to tasksError,
            TaskLane.Internal to internalError,
        ))
        assertEquals(tasksError, state.activeLoadError)
        assertEquals(internalError, state.copy(lane = TaskLane.Internal).activeLoadError)
        assertNull(state.copy(lane = TaskLane.Monitors).activeLoadError)
    }

    @Test fun connection_failures_produce_actionable_offline_copy() {
        val error = ConnectException("Connection refused").taskUserError(
            fallbackTitle = "Tasks unavailable",
            fallbackMessage = "Tasks could not be loaded.",
        )
        assertEquals("Magician is offline", error.title)
        assertEquals("Start Magician or check this device’s connection, then try again.", error.message)
    }

    @Test fun timeout_failures_do_not_claim_that_magician_is_offline() {
        val error = SocketTimeoutException("read timed out").taskUserError(
            fallbackTitle = "Tasks unavailable",
            fallbackMessage = "Tasks could not be loaded.",
        )
        assertEquals("Magician took too long", error.title)
        assertTrue(error.message.contains("timed out"))
    }

    /**
     * The transport statuses defer to the shared classifier, so a stopped
     * server reads the same here as on Attention and in Chat. The
     * task-specific ones — a task that moved on, a server asking to be left
     * alone — keep their own copy, because nothing generic can say those.
     */
    @Test fun http_failures_distinguish_auth_conflict_busy_and_service_outage() {
        val auth = TaskApiError("secret provider text", 401).taskUserError("Failed", "Try again.")
        assertEquals("Magician refused the connection", auth.title)
        assertTrue("says where the fix is", auth.message.contains("Settings"))

        assertEquals(
            "This task changed",
            TaskApiError("conflict", 409).taskUserError("Failed", "Try again.").title,
        )
        assertEquals(
            "Magician is busy",
            TaskApiError("busy", 429).taskUserError("Failed", "Try again.").title,
        )

        val outage = TaskApiError("internal path leaked", 503).taskUserError("Failed", "Try again.")
        assertEquals("Can't reach Magician", outage.title)

        // The point of the odd bodies above: whatever the server said about its
        // own internals is not what the owner is shown.
        listOf(auth, outage).forEach { shown ->
            assertFalse(shown.message.contains("secret provider text"))
            assertFalse(shown.message.contains("internal path leaked"))
        }
    }

    @Test fun missing_host_and_unknown_failures_do_not_surface_raw_internals() {
        val missing = TaskApiError("No Magician host configured yet.").taskUserError("Failed", "Try again.")
        assertEquals("Connect to Magician", missing.title)
        assertTrue(missing.message.contains("Settings"))

        val unknown = IllegalStateException("/private/service/token").taskUserError(
            fallbackTitle = "Tasks unavailable",
            fallbackMessage = "Tasks could not be loaded. Try again.",
        )
        assertEquals("Tasks unavailable", unknown.title)
        assertEquals("Tasks could not be loaded. Try again.", unknown.message)
    }

    @Test fun error_classification_is_iterative_and_cycle_safe() {
        val outer = IllegalStateException("outer")
        val inner = IllegalArgumentException("inner")
        outer.initCause(inner)
        inner.initCause(outer)

        val error = outer.taskUserError("Tasks unavailable", "Tasks could not be loaded. Try again.")
        assertEquals("Tasks unavailable", error.title)
        assertEquals("Tasks could not be loaded. Try again.", error.message)
    }

    @Test fun completion_grace_reconciles_the_visible_lane_and_count() {
        val completed = task(id = "just-done", status = "completed")
        val state = TasksUiState(
            tasks = emptyList(), filter = TaskFilter.All, loadedView = TaskFilter.All,
            laneCounts = mapOf("all" to 3), gracedRows = listOf(completed),
        )
        assertEquals(listOf("just-done"), state.visibleTasks().map(TaskV3::id))
        assertEquals(4, state.laneCount(TaskFilter.All))
    }

    @Test fun monitor_validation_requires_objective_and_a_source() {
        assertEquals("Describe what to monitor.", MonitorDraft().validationError())
        assertEquals("Add at least one URL, domain, or search phrase.", MonitorDraft(objective = "Watch").validationError())
        assertNull(MonitorDraft(objective = "Watch", domains = listOf("example.com")).validationError())
    }

    @Test fun monitor_validation_rejects_bad_urls_and_cron() {
        assertEquals(
            "One of the URLs is not a valid http(s) URL.",
            MonitorDraft(objective = "Watch", urls = listOf("file:///tmp/x")).validationError(),
        )
        assertEquals(
            "Schedule must be a five-field cron expression.",
            MonitorDraft(objective = "Watch", domains = listOf("example.com"), cron = "0 9 *").validationError(),
        )
    }

    @Test fun monitor_validation_normalizes_sources_before_deciding_they_exist() {
        assertEquals(
            "Add at least one URL, domain, or search phrase.",
            MonitorDraft(objective = "Watch", domains = listOf("   "), querySeeds = listOf("\t")).validationError(),
        )
        assertNull(
            MonitorDraft(objective = "Watch", urls = listOf("HTTPS://example.com/releases")).validationError(),
        )
    }

    @Test fun monitor_validation_bounds_collections_before_network_dispatch() {
        assertEquals(
            "Keep it to 100 URLs or fewer.",
            MonitorDraft(
                objective = "Watch",
                urls = (1..101).map { "https://example.com/$it" },
            ).validationError(),
        )
        assertEquals(
            "Keep each list entry under 500 characters.",
            MonitorDraft(
                objective = "Watch",
                domains = listOf("example.com"),
                includeRules = listOf("x".repeat(501)),
            ).validationError(),
        )
        assertEquals(
            "Keep each list to 50 entries or fewer.",
            MonitorDraft(
                objective = "Watch",
                domains = listOf("example.com"),
                querySeeds = (1..51).map { "query-$it" },
            ).validationError(),
        )
    }

    @Test fun monitor_spec_normalizes_lists_without_changing_order() {
        val spec = MonitorDraft(
            objective = " Watch releases ",
            domains = listOf(" example.com ", "example.com"),
            querySeeds = listOf(" beta ", "stable"),
        ).spec()
        assertEquals("Watch releases", spec.objective)
        assertEquals(listOf("example.com"), spec.sources.domains)
        assertEquals(listOf("beta", "stable"), spec.querySeeds)
    }

    @Test fun cursor_page_distinguishes_absent_total_from_zero() {
        val old = taskJson.decodeFromString<MonitorListPage>(
            """{"items":[],"next_cursor":null,"limit":50}""",
        )
        val counted = taskJson.decodeFromString<MonitorListPage>(
            """{"items":[],"next_cursor":null,"limit":50,"total":0,"offset":0}""",
        )
        assertNull(old.total)
        assertEquals(0, counted.total)
    }

    @Test fun monitor_run_keeps_source_diagnostics_and_finding_evidence() {
        val run = taskJson.decodeFromString<MonitorRun>("""{
          "monitor_task_id":"m1","execution_id":"e1","status":"degraded",
          "complete_scan":false,"counts":{"scanned":1},
          "source_outcomes":[{"source":"example.com","status":"auth_failed","complete":false,"items_scanned":0}],
          "findings":[{"title":"Change","evidence":[{"kind":"quote","value":"updated","url":"https://example.com"}]}],
          "access_problem":{"source":"example.com","kind":"login","message":"Sign in again","since":"2026-08-10T00:00:00Z"}
        }""")
        assertEquals("auth_failed", run.sourceOutcomes.single().status)
        assertEquals("updated", run.findings.single().evidence.single().value)
        assertEquals("Sign in again", run.accessProblem?.message)
    }

    @Test fun detail_requires_one_authoritative_identity_run_or_history_section() {
        val failed = Result.failure<kotlinx.serialization.json.JsonObject>(IllegalStateException("offline"))
        val results = mapOf(
            "task" to failed, "run" to failed, "output" to Result.success(buildJsonObject {}),
            "history" to failed, "plan" to Result.success(buildJsonObject {}),
        )
        val error = runCatching { assembleTaskDetail(results) }.exceptionOrNull()
        assertTrue(error is TaskApiError)
        assertEquals("Task details could not be loaded.", error?.message)
    }

    @Test fun detail_discloses_only_failed_user_visible_sections_and_keeps_successes() {
        val failed = Result.failure<kotlinx.serialization.json.JsonObject>(IllegalStateException("offline"))
        val task = buildJsonObject { put("id", "task-1") }
        val history = buildJsonObject { put("count", 1) }
        val bundle = assembleTaskDetail(mapOf(
            "task" to Result.success(task), "run" to failed, "output" to failed,
            "history" to Result.success(history), "plan" to failed,
        ))
        assertEquals(task, bundle.task)
        assertEquals(history, bundle.details)
        assertEquals(setOf("live run", "outputs"), bundle.unavailableSections)
    }

    @Test fun realtime_panel_is_scope_checked_and_extracts_the_inspected_execution() {
        val raw = """{
          "event_type":"ExecutionPanelDelta",
          "data":{"principal":"anonymous","workspace":"default","timestamp":123,"state":{
            "overview":{"task_id":"task-1","execution_id":"exec-1"},
            "debug":{"selected_execution":{"execution_id":"exec-fallback"}}
          }}
        }"""
        val event = parseTaskRealtimeEvent(raw, "anonymous", "default")
        assertNotNull(event)
        assertEquals("task-1", event?.taskId)
        assertEquals("exec-1", event?.executionId)
        assertEquals(123L, event?.eventTimestamp)
        assertNotNull(event?.panel)
        assertNull(parseTaskRealtimeEvent(raw, "someone-else", "default"))
        assertNull(parseTaskRealtimeEvent(raw, "anonymous", "another-workspace"))
    }

    @Test fun realtime_ignores_unrelated_event_families() {
        assertNull(parseTaskRealtimeEvent(
            """{"event_type":"ChatMessageCreated","data":{}}""",
            "anonymous",
            "default",
        ))
    }

    @Test fun verdict_prioritizes_human_attention_over_failure() {
        val verdict = TaskVerdict.derive(TaskVerdictInput(
            status = "failed",
            attention = TaskAttention(TaskAttentionSource.Escalation),
            error = "boom",
        ))
        assertEquals(TaskVerdictState.Waiting, verdict.state)
        assertEquals("Waiting on you", verdict.headline)
        assertEquals("The run got stuck and needs a decision", verdict.detail)
    }

    @Test fun verdict_reports_running_progress_and_stalls_only_after_real_silence() {
        val running = TaskVerdict.derive(TaskVerdictInput(
            status = "running", currentStep = 4, totalSteps = 7,
            currentStepLabel = "Searching memory",
        ))
        assertEquals("Running · step 4 of 7", running.headline)
        assertEquals("Searching memory", running.detail)
        val stalled = TaskVerdict.derive(TaskVerdictInput(
            status = "running", currentStep = 4, totalSteps = 7,
            currentStepLabel = "Searching memory", lastProgressAtMillis = 0,
            nowMillis = 6 * 60 * 1_000L,
        ))
        assertEquals(TaskVerdictState.Stalled, stalled.state)
        assertEquals("Stalled · no progress for 6m", stalled.headline)
        assertEquals("Still on step 4: Searching memory", stalled.detail)
    }

    @Test fun duration_formatting_rejects_unknowns_and_uses_two_units_at_most() {
        assertNull(TaskVerdict.durationIfKnown(null))
        assertNull(TaskVerdict.durationIfKnown(-1.0))
        assertNull(TaskVerdict.durationIfKnown(Double.NaN))
        assertNull(TaskVerdict.durationIfKnown(Double.POSITIVE_INFINITY))
        assertEquals("0s", TaskVerdict.durationIfKnown(0.0))
        assertEquals("3m 12s", TaskVerdict.durationIfKnown(192.0))
        assertEquals("1h 20m", TaskVerdict.durationIfKnown(80.0 * 60 + 5))
    }

    @Test fun detail_acts_are_ordered_and_default_to_the_answering_surface() {
        val acts = TaskVerdict.acts(hasPlan = true, hasRun = true, hasOutput = true)
        assertEquals(listOf(TaskDetailAct.Plan, TaskDetailAct.Run, TaskDetailAct.Output), acts)
        assertEquals(
            TaskDetailAct.Plan,
            TaskVerdict.defaultOpenAct(TaskVerdictState.Waiting, acts, TaskAttentionSource.PlanApproval),
        )
        assertEquals(
            TaskDetailAct.Run,
            TaskVerdict.defaultOpenAct(TaskVerdictState.Waiting, acts, TaskAttentionSource.DiffApproval),
        )
        assertEquals(
            TaskDetailAct.Run,
            TaskVerdict.defaultOpenAct(
                TaskVerdictState.Finished,
                listOf(TaskDetailAct.Plan, TaskDetailAct.Run),
                null,
            ),
        )
    }
}
