package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import ai.magicbeans.magdroid.work.*
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.foundation.layout.Column
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.serialization.json.*
import org.junit.*
import org.junit.runner.RunWith
import java.net.InetAddress
import java.net.ServerSocket
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

/** Actual Compose interaction, backed by a task-owned loopback HTTP transport. */
@RunWith(AndroidJUnit4::class)
class WorkNavigationTest {
    @get:Rule val compose = createComposeRule()
    private lateinit var server: WorkHTTPFixture
    private val themeId = mutableStateOf(Themes.DEFAULT)
    @Before fun start() { server = WorkHTTPFixture() }
    @After fun stop() { server.close() }

    @Test fun aSuccessfulSteerRefreshesControlsAndAllowsTheNextSteer() {
        val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as android.app.Application
        val delegate = ai.magicbeans.magdroid.tasks.TaskRepository(app)
        val posts = AtomicInteger()
        val gets = AtomicInteger()
        val source = object : ai.magicbeans.magdroid.tasks.TasksDataSource by delegate {
            override fun taskEvents() = emptyFlow<ai.magicbeans.magdroid.tasks.TaskRealtimeEvent>()
            override suspend fun agents() = emptyList<ai.magicbeans.magdroid.tasks.AgentOption>()
            override suspend fun listTasks(lane: ai.magicbeans.magdroid.tasks.TaskLane, filter: ai.magicbeans.magdroid.tasks.TaskFilter?, today: String, offset: Int, limit: Int, query: String?) = ai.magicbeans.magdroid.tasks.TaskListResponse()
            override suspend fun listInternalTasks(query: ai.magicbeans.magdroid.tasks.InternalTaskQuery) = ai.magicbeans.magdroid.tasks.TaskListResponse()
            override suspend fun executionControlState(executionId: String): ai.magicbeans.magdroid.tasks.ExecutionControlState {
                gets.incrementAndGet()
                return ai.magicbeans.magdroid.tasks.ExecutionControlState(executionId = executionId, active = true, canSteer = true)
            }
            override suspend fun executionControl(executionId: String, action: ai.magicbeans.magdroid.tasks.ExecutionControlAction, guidance: String?) {
                Assert.assertEquals(ai.magicbeans.magdroid.tasks.ExecutionControlAction.Steer, action)
                Assert.assertEquals("New requirement", guidance)
                posts.incrementAndGet()
            }
        }
        lateinit var model: ai.magicbeans.magdroid.tasks.TasksViewModel
        try {
            compose.runOnUiThread { model = ai.magicbeans.magdroid.tasks.TasksViewModel.usingDataSource(app, source) }
            repeat(2) { index ->
                compose.runOnUiThread { model.executionControl("exec", ai.magicbeans.magdroid.tasks.ExecutionControlAction.Steer, "New requirement") }
                compose.waitUntil(5_000) {
                    gets.get() == index + 1 && model.state.value.executionControls["exec"]?.busy == null
                }
            }
            Assert.assertEquals(2, posts.get())
            Assert.assertTrue(model.state.value.executionControls["exec"]?.state?.canSteer == true)
        } finally { delegate.close() }
    }
    private fun repository() = WorkRepository.usingConnection(MagicianConnectionSnapshot(server.origin, mapOf("Authorization" to "Bearer native-fixture")))
    private fun mount(section: WorkSection = WorkSection.Crew, route: WorkRoute? = null, fontScale: Float? = null, openTask: OpenTaskDetail? = null) {
        val factory: (android.content.Context) -> WorkResource = { context -> WorkResource(context, ::repository, { true }, emptyFlow()) }
        compose.setContent {
            MagicanTheme(themeId.value, activePalette) {
                CompositionLocalProvider(LocalWorkResourceFactory provides factory, LocalOpenTaskDetail provides openTask,
                    LocalDensity provides Density(LocalDensity.current.density, fontScale ?: LocalDensity.current.fontScale)) {
                    WorkLibraryScreen(section, rememberTasksScreenActions(), {}, initialRoute = route)
                }
            }
        }
    }

    @Test fun appHistoryPagesKeepExactRunAndOriginatingAppWithoutLifecycleEdits() {
        val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as android.app.Application
        val delegate = ai.magicbeans.magdroid.tasks.TaskRepository(app)
        val appID = "task_app_" + "a".repeat(64)
        val task = ai.magicbeans.magdroid.tasks.TaskV3(id = appID, title = "Recurring App", status = "completed", lifecycle = "internal", uiThreadId = "app:install.one", createdBy = "app_action", latestRootExecutionId = "latest")
        val page = Json.parseToJsonElement("""{"task":{"manifest":{"title":"Recurring App","ui_thread_id":"app:install.one","created_by":"app_action","lifecycle":"internal"}},"executions":[{"state":{"execution_id":"latest","status":"completed"}}],"next_execution_cursor":"cursor/+","recurring_schedule":{"behavior_id":"behavior","interval_seconds":60}}""").jsonObject
        val requests = mutableListOf<String>()
        val source = object : ai.magicbeans.magdroid.tasks.TasksDataSource by delegate {
            override fun taskEvents() = emptyFlow<ai.magicbeans.magdroid.tasks.TaskRealtimeEvent>()
            override suspend fun agents() = emptyList<ai.magicbeans.magdroid.tasks.AgentOption>()
            override suspend fun listTasks(lane: ai.magicbeans.magdroid.tasks.TaskLane, filter: ai.magicbeans.magdroid.tasks.TaskFilter?, today: String, offset: Int, limit: Int, query: String?) = ai.magicbeans.magdroid.tasks.TaskListResponse()
            override suspend fun listInternalTasks(query: ai.magicbeans.magdroid.tasks.InternalTaskQuery) = ai.magicbeans.magdroid.tasks.TaskListResponse(listOf(task))
            override suspend fun taskDetail(taskId: String, executionId: String?): ai.magicbeans.magdroid.tasks.TaskDetailBundle {
                requests += executionId.orEmpty()
                return ai.magicbeans.magdroid.tasks.TaskDetailBundle(task = page["task"]!!.jsonObject, panel = Json.parseToJsonElement("""{"overview":{"title":"Recurring App","status":"completed","execution_id":"${executionId ?: "latest"}"},"debug":{"selected_execution":{"execution_id":"${executionId ?: "latest"}"}}}""").jsonObject, details = page)
            }
            override suspend fun taskDetailsPage(taskId: String, cursor: String): JsonObject { Assert.assertEquals(appID, taskId); Assert.assertEquals("cursor/+", cursor); return Json.parseToJsonElement("""{"executions":[{"state":{"execution_id":"older","status":"completed"}}]}""").jsonObject }
        }
        lateinit var model: ai.magicbeans.magdroid.tasks.TasksViewModel
        try {
            compose.runOnUiThread { model = ai.magicbeans.magdroid.tasks.TasksViewModel.usingDataSource(app, source); model.selectLane(ai.magicbeans.magdroid.tasks.TaskLane.Internal) }
            compose.setContent { MagicanTheme(themeId.value, activePalette) { TasksScreen(model) } }
            waitFor("Recurring App"); compose.onNodeWithText("Recurring App").performClick()
            scrollTo("Technical details"); compose.onNodeWithText("Technical details").performClick()
            scrollTo("Load older runs"); compose.onNodeWithText("Load older runs").performClick()
            compose.waitUntil(5_000) { model.state.value.detail.historyExpanded }
            scrollTo("older"); compose.onNodeWithContentDescription("Show run older").performClick()
            compose.waitUntil(5_000) { requests.lastOrNull() == "older" && !model.state.value.detail.loading }
            scrollTo("Related tools"); compose.onNodeWithText("Related tools").performClick()
            scrollTo("Originating App"); compose.onNodeWithText("Originating App").assertIsDisplayed()
            compose.runOnIdle {
                Assert.assertNull(model.state.value.detail.bundle?.details?.get("next_execution_cursor")); Assert.assertNotNull(model.state.value.detail.bundle?.details?.get("recurring_schedule"))
                Assert.assertEquals(2, (model.state.value.detail.bundle?.details?.get("executions") as JsonArray).size)
            }
        } finally { delegate.close() }
        Assert.assertEquals(0, server.writes.get())
    }

    @Test fun internalSearchRequeriesBeyondFirstPageAndCleanupRetainsOnlyFailures() {
        val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as android.app.Application
        val delegate = ai.magicbeans.magdroid.tasks.TaskRepository(app)
        val appID = "task_app_" + "a".repeat(64)
        val records = (List(60) { ai.magicbeans.magdroid.tasks.TaskV3(id = "noise-$it", title = "Noise $it", status = "completed", lifecycle = "internal") } + listOf(
            ai.magicbeans.magdroid.tasks.TaskV3(id = "good", title = "needle Good", status = "completed", lifecycle = "internal"),
            ai.magicbeans.magdroid.tasks.TaskV3(id = "bad", title = "needle Bad", status = "completed", lifecycle = "internal"),
            ai.magicbeans.magdroid.tasks.TaskV3(id = appID, title = "needle App", status = "completed", lifecycle = "internal"))).toMutableList()
        val regular = List(60) { ai.magicbeans.magdroid.tasks.TaskV3(id = "regular-$it", title = "Regular $it", status = "pending") } + ai.magicbeans.magdroid.tasks.TaskV3(id = "task_tagged", title = "Beyond loaded page", status = "pending", tags = listOf(ai.magicbeans.magdroid.tasks.TaskTag("only-tag", "only-tag")))
        val taskWrites = mutableListOf<Pair<String, JsonObject>>()
        val monitorQueries = mutableListOf<Pair<Int, String?>>()
        val queries = mutableListOf<ai.magicbeans.magdroid.tasks.InternalTaskQuery>(); val deletes = mutableListOf<String>()
        val source = object : ai.magicbeans.magdroid.tasks.TasksDataSource by delegate {
            override fun taskEvents() = emptyFlow<ai.magicbeans.magdroid.tasks.TaskRealtimeEvent>()
            override suspend fun agents() = emptyList<ai.magicbeans.magdroid.tasks.AgentOption>()
            override suspend fun listTasks(lane: ai.magicbeans.magdroid.tasks.TaskLane, filter: ai.magicbeans.magdroid.tasks.TaskFilter?, today: String, offset: Int, limit: Int, query: String?): ai.magicbeans.magdroid.tasks.TaskListResponse = ai.magicbeans.magdroid.tasks.TaskListResponse(regular.drop(offset).take(50), ai.magicbeans.magdroid.tasks.TaskListPagination(regular.size, 50, offset, offset + 50 < regular.size))
            override suspend fun listMonitors(limit: Int, cursor: String?, state: String?): ai.magicbeans.magdroid.tasks.MonitorListPage {
                monitorQueries += limit to cursor; val offset = cursor?.toInt() ?: 0; val end = (offset + limit).coerceAtMost(260)
                return ai.magicbeans.magdroid.tasks.MonitorListPage((offset until end).map { ai.magicbeans.magdroid.tasks.MonitorListItem(taskId = "monitor-$it", title = "Monitor $it") }, if (end < 260) end.toString() else null, limit, 260)
            }
            override suspend fun listInternalTasks(query: ai.magicbeans.magdroid.tasks.InternalTaskQuery): ai.magicbeans.magdroid.tasks.TaskListResponse {
                queries += query
                val matches = records.filter { it.title.contains(query.search, true) }
                return ai.magicbeans.magdroid.tasks.TaskListResponse(matches.drop(query.offset).take(query.limit), ai.magicbeans.magdroid.tasks.TaskListPagination(matches.size, query.limit, query.offset, query.offset + query.limit < matches.size))
            }
            override suspend fun taskThread(id: String) = ai.magicbeans.magdroid.tasks.TaskThreadChoice(id, id, archived = id == "archived")
            override suspend fun updateTask(taskId: String, fields: JsonObject) { taskWrites += taskId to fields }
            override suspend fun deleteTask(task: ai.magicbeans.magdroid.tasks.TaskV3, removeFiles: Boolean) { deletes += task.id; if (task.id == "bad") error("record busy"); records.removeAll { it.id == task.id } }
        }
        lateinit var model: ai.magicbeans.magdroid.tasks.TasksViewModel
        try {
            compose.runOnUiThread { model = ai.magicbeans.magdroid.tasks.TasksViewModel.usingDataSource(app, source); model.selectLane(ai.magicbeans.magdroid.tasks.TaskLane.Internal) }
            compose.setContent { MagicanTheme(themeId.value, activePalette) { TasksScreen(model) } }
            compose.waitUntil(5_000) { model.state.value.internalLoadState == ai.magicbeans.magdroid.tasks.TaskLoadState.Loaded }
            compose.runOnIdle { Assert.assertEquals(50, model.state.value.internalTasks.size); Assert.assertTrue(model.state.value.internalTasks.none { it.id == "good" }); model.setSearch("needle") }
            waitFor("needle Good")
            compose.runOnIdle { Assert.assertEquals("needle", queries.last().search); Assert.assertEquals(3, model.state.value.internalTasks.size) }
            compose.onNodeWithText("Select cleanup").performClick(); compose.onNodeWithText("Select loaded").performClick()
            compose.onNodeWithText("Delete selected (2)").performClick(); compose.onNodeWithText("Delete selected records").performClick()
            compose.waitUntil(5_000) { model.state.value.cleanupProgress?.startsWith("Deleted") == true }
            compose.runOnIdle { Assert.assertEquals(listOf("good", "bad"), deletes); Assert.assertEquals(setOf("bad"), model.state.value.selectedInternalIDs); Assert.assertEquals("record busy", model.state.value.cleanupFailures["bad"]); Assert.assertTrue(records.any { it.id == appID }) }
            compose.runOnIdle { model.selectLane(ai.magicbeans.magdroid.tasks.TaskLane.Tasks); model.setSearch("only-tag") }
            waitFor("Beyond loaded page"); compose.runOnIdle { Assert.assertEquals(listOf("task_tagged"), model.state.value.visibleTasks().map { it.id }); Assert.assertFalse(model.state.value.taskHasMore) }
            compose.runOnIdle { model.updateTitle(regular.last(), "Renamed") }
            compose.waitUntil(5_000) { taskWrites.size == 1 && model.state.value.mutatingTaskId == null }
            compose.runOnIdle { Assert.assertEquals("Renamed", (taskWrites.last().second["title"] as JsonPrimitive).content); model.moveTask(regular.last(), "archived") }
            compose.waitUntil(5_000) { model.state.value.actionError != null && model.state.value.mutatingTaskId == null }
            compose.runOnIdle { Assert.assertEquals(1, taskWrites.size); model.clearError(); model.moveTask(regular.last(), "launch") }
            compose.waitUntil(5_000) { taskWrites.size == 2 && model.state.value.mutatingTaskId == null }
            compose.runOnIdle { Assert.assertEquals("launch", (taskWrites.last().second["ui_thread_id"] as JsonPrimitive).content); Assert.assertEquals(1, taskWrites.last().second.size) }
            compose.runOnIdle { model.selectLane(ai.magicbeans.magdroid.tasks.TaskLane.Monitors); model.setMonitorPageSize(200) }
            compose.waitUntil(5_000) { model.state.value.monitors.size == 200 }
            compose.runOnIdle { model.loadMore() }; compose.waitUntil(5_000) { model.state.value.monitors.size == 260 }
            compose.runOnIdle { model.refresh() }; compose.waitUntil(5_000) { monitorQueries.takeLast(2) == listOf(200 to null, 60 to "200") && model.state.value.monitorLoadState == ai.magicbeans.magdroid.tasks.TaskLoadState.Loaded }
            compose.runOnIdle { Assert.assertEquals(260, model.state.value.monitors.size); Assert.assertNull(model.state.value.monitorNextCursor) }
        } finally { delegate.close() }
        Assert.assertEquals(0, server.writes.get())
    }

    @Test fun completedTaskReferencesRemainRemovableAndSerializeDependencies() {
        var created: ai.magicbeans.magdroid.tasks.TaskCreateDraft? = null
        val completed = ai.magicbeans.magdroid.tasks.TaskV3(id = "task_previous", title = "Previous deliverable", status = "completed")
        compose.setContent { MagicanTheme(themeId.value, activePalette) {
            TaskCreateSheet(agents = listOf(ai.magicbeans.magdroid.tasks.AgentOption("custom-owner", "Owner", true)), busy = false, onDismiss = {}, onCreate = { created = it }, referenceState = ai.magicbeans.magdroid.tasks.TasksUiState(completedReferences = listOf(completed)))
        } }
        compose.onNode(hasSetTextAction() and hasText("Title")).performTextInput("Continue work")
        compose.onNodeWithText("Reference completed work (0)").performClick()
        scrollTo("Previous deliverable"); compose.onNodeWithText("Previous deliverable").performClick()
        scrollTo("Remove reference: Previous deliverable"); compose.onNodeWithText("Remove reference: Previous deliverable").assertIsDisplayed()
        scrollTo("Create Task"); compose.onNodeWithText("Create Task").performClick()
        compose.runOnIdle { Assert.assertEquals(listOf("task_previous"), created?.dependsOn); Assert.assertEquals("task:task_previous", created?.description) }
        Assert.assertEquals(0, server.writes.get())
    }
    @Test fun taskTitleAndLoadedThreadControlsSaveExactFieldsAndCloseAfterMove() {
        fun scrollActions(text: String) { compose.onNodeWithTag("task-actions-scroll").performScrollToNode(hasText(text)); compose.onNodeWithText(text).performScrollTo() }
        val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as android.app.Application
        val delegate = ai.magicbeans.magdroid.tasks.TaskRepository(app)
        var task = ai.magicbeans.magdroid.tasks.TaskV3(id = "task_edit", title = "Editable work", status = "ready", uiThreadId = "general", schedule = Json.parseToJsonElement("""{"kind":{"Cron":{"expression":"0 9 * * *","timezone":"UTC"}},"max_runs":3,"future_limit":18446744073709551615}"""))
        val writes = mutableListOf<JsonObject>()
        val checkedThreads = mutableListOf<String>()
        val source = object : ai.magicbeans.magdroid.tasks.TasksDataSource by delegate {
            override fun taskEvents() = emptyFlow<ai.magicbeans.magdroid.tasks.TaskRealtimeEvent>()
            override suspend fun agents() = emptyList<ai.magicbeans.magdroid.tasks.AgentOption>()
            override suspend fun listTasks(lane: ai.magicbeans.magdroid.tasks.TaskLane, filter: ai.magicbeans.magdroid.tasks.TaskFilter?, today: String, offset: Int, limit: Int, query: String?) = ai.magicbeans.magdroid.tasks.TaskListResponse(if (lane == ai.magicbeans.magdroid.tasks.TaskLane.Tasks) listOf(task) else emptyList())
            override suspend fun listInternalTasks(query: ai.magicbeans.magdroid.tasks.InternalTaskQuery) = ai.magicbeans.magdroid.tasks.TaskListResponse()
            override suspend fun listMonitors(limit: Int, cursor: String?, state: String?) = ai.magicbeans.magdroid.tasks.MonitorListPage()
            override suspend fun taskThread(id: String): ai.magicbeans.magdroid.tasks.TaskThreadChoice { checkedThreads += id; return ai.magicbeans.magdroid.tasks.TaskThreadChoice(id, id) }
            override suspend fun taskSchedule(taskId: String) = task.schedule as? JsonObject
            override suspend fun updateTask(taskId: String, fields: JsonObject) {
                Assert.assertEquals(task.id, taskId); writes += fields
                task = task.copy(title = (fields["title"] as? JsonPrimitive)?.content ?: task.title, uiThreadId = (fields["ui_thread_id"] as? JsonPrimitive)?.content ?: task.uiThreadId, schedule = fields["schedule"] ?: task.schedule)
            }
        }
        lateinit var model: ai.magicbeans.magdroid.tasks.TasksViewModel
        val factory: (android.content.Context) -> WorkResource = { context -> WorkResource(context, ::repository, { true }, emptyFlow()) }
        try {
            compose.runOnUiThread { model = ai.magicbeans.magdroid.tasks.TasksViewModel.usingDataSource(app, source) }
            compose.setContent { MagicanTheme(themeId.value, activePalette) { CompositionLocalProvider(LocalWorkResourceFactory provides factory) { TasksScreen(model) } } }
            waitFor("Editable work"); compose.onNodeWithText("Actions").performClick()
            scrollActions("Task title"); compose.onNode(hasSetTextAction() and hasText("Task title")).performTextReplacement("Renamed work")
            scrollActions("Save title"); compose.onNodeWithText("Save title").assertIsEnabled().performClick()
            compose.waitUntil(5_000) { model.state.value.tasks.firstOrNull()?.title == "Renamed work" }
            compose.onNodeWithText("Save title").assertIsNotEnabled()
            scrollActions("Pause schedule"); compose.onNodeWithText("Pause schedule").performClick()
            compose.waitUntil(5_000) { model.state.value.tasks.firstOrNull()?.schedulePaused == true }
            scrollActions("Resume schedule"); compose.onNodeWithText("Resume schedule").assertIsDisplayed()
            compose.runOnIdle { Assert.assertEquals("18446744073709551615", ((task.schedule as JsonObject)["future_limit"] as JsonPrimitive).content); Assert.assertEquals("3", ((task.schedule as JsonObject)["max_runs"] as JsonPrimitive).content) }
            scrollActions("Move to another thread"); compose.onNodeWithText("Move to another thread").performClick()
            waitFor("Launch"); scrollActions("Move task"); compose.onNodeWithText("Move task").assertIsNotEnabled()
            compose.onNodeWithText("Archived").assertDoesNotExist()
            scrollActions("Find destination thread"); compose.onNode(hasSetTextAction() and hasText("Find destination thread")).performTextInput("Launch")
            compose.onNodeWithText("General", substring = false).assertDoesNotExist()
            val launchChoice = hasText("Launch") and !hasSetTextAction()
            compose.onNodeWithTag("task-actions-scroll").performScrollToNode(launchChoice)
            compose.onNode(launchChoice).performScrollTo().performClick()
            scrollActions("Move task"); compose.onNodeWithText("Move task").assertIsEnabled().performClick()
            compose.waitUntil(5_000) { model.state.value.tasks.firstOrNull()?.uiThreadId == "launch" }
            compose.onNodeWithText("Task actions").assertDoesNotExist()
            compose.runOnIdle {
                Assert.assertEquals(listOf("launch"), checkedThreads)
                Assert.assertEquals(buildJsonObject { put("title", "Renamed work") }, writes.first())
                Assert.assertEquals(setOf("schedule"), writes[1].keys)
                Assert.assertEquals(buildJsonObject { put("ui_thread_id", "launch") }, writes.last())
            }
        } finally { delegate.close() }
        Assert.assertEquals(0, server.writes.get())
    }
    @Test fun simpleTaskCreationDefaultsToRealPrimaryOwner() {
        var created: ai.magicbeans.magdroid.tasks.TaskCreateDraft? = null
        compose.setContent { MagicanTheme(themeId.value, activePalette) {
            TaskCreateSheet(agents = listOf(ai.magicbeans.magdroid.tasks.AgentOption("secondary", "Secondary"), ai.magicbeans.magdroid.tasks.AgentOption("custom-owner", "My primary", true)), busy = false, onDismiss = {}, onCreate = { created = it })
        } }
        compose.onNode(hasSetTextAction() and hasText("Title")).performTextInput("Simple work")
        compose.onNodeWithText("Create", substring = false).assertIsDisplayed().assertIsEnabled().performClick()
        compose.runOnIdle { Assert.assertEquals("custom-owner", created?.agentId); Assert.assertEquals("Simple work", created?.title) }
        Assert.assertEquals(0, server.writes.get())
    }

    @Test fun programProgressKeepsExactTaskAndExecutionEvidence() {
        var target: String? = null; var execution: String? = null
        mount(section = WorkSection.Programs, route = WorkRoute("program", "fixture.md", "Program"), openTask = OpenTaskDetail { task, run, _ -> target = task; execution = run })
        scrollTo("Responsible crew and progress"); waitFor("Current phase: Verify migration")
        scrollTo("Task and execution evidence"); compose.onNodeWithText("Task and execution evidence").performClick()
        compose.runOnIdle { Assert.assertEquals("fixture-detail-done", target); Assert.assertEquals("exec-fixture", execution) }
        Assert.assertTrue(server.readTargets.any { it.contains("/agents/fixture-crew/harness-overview") })
    }

    @Test fun crewCycleOpensExactSharedExecutionTarget() {
        var target: String? = null; var execution: String? = null
        mount(openTask = OpenTaskDetail { task, run, _ -> target = task; execution = run })
        waitFor("Fixture crew"); compose.onNodeWithText("Fixture crew").performClick()
        scrollTo("Inspect current cycle")
        compose.onNodeWithTag("work-crew-current-cycle").performClick()
        compose.runOnIdle {
            Assert.assertEquals("agent-cycle:fixture-crew:fixture-cycle", target)
            Assert.assertEquals("exec-fixture", execution)
        }
        Assert.assertTrue(server.readTargets.none { it.contains("/tasks/agent-cycle") })
    }

    @Test fun crewActionsStayInsideThePhoneAtDoubleTextSize() {
        mount(fontScale = 2f); waitFor("Fixture crew"); compose.onNodeWithText("Fixture crew").performClick()
        waitFor("Run once"); scrollTo("Run once")
        val root = compose.onRoot().getUnclippedBoundsInRoot()
        listOf("Pause", "Resume", "Run once").forEach { label ->
            val button = compose.onNodeWithText(label)
            button.assertIsDisplayed()
            val bounds = button.getUnclippedBoundsInRoot()
            Assert.assertTrue("$label must fit horizontally", bounds.left >= root.left && bounds.right <= root.right)
        }
        compose.onNodeWithText("Run once").performClick()
        compose.onNodeWithText("Cancel").performClick()
        scrollTo("Edit definition"); compose.onNodeWithText("Edit definition").performClick()
        waitFor("Advanced JSON"); compose.onNodeWithText("Advanced JSON").performClick()
        compose.onNodeWithTag("work-crew-editor-scroll").performScrollToNode(hasTestTag("work-crew-definition-editor"))
        compose.onNodeWithTag("work-crew-definition-editor").performClick().performTextInput(" ")
        compose.waitUntil(5_000) { compose.onNodeWithText("Cancel").isDisplayed() }
        compose.onNodeWithText("Cancel").assertIsDisplayed().performClick()
        Assert.assertEquals(0, server.writes.get())
    }
    private fun waitFor(text: String) {
        try { compose.waitUntil(8_000) { compose.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty() } }
        catch (error: Throwable) { throw AssertionError("Waiting for '$text'\n" + compose.onRoot().printToString(), error) }
    }
    private fun scrollTo(text: String) {
        compose.onAllNodes(hasScrollAction()).onFirst().performScrollToNode(hasText(text))
        compose.onNodeWithText(text).performScrollTo()
    }

    @Test fun crewEditCancelAndConfirmedCycleKeepIdentity() {
        mount(); waitFor("Fixture crew"); compose.onNodeWithText("Fixture crew").performClick()
        waitFor("Pause"); scrollTo("Edit definition"); compose.onNodeWithText("Edit definition").performClick()
        compose.onNodeWithText("Guided").assertExists()
        compose.onNodeWithText("Cancel").performClick(); Assert.assertEquals(0, server.writes.get())
        scrollTo("Pause"); compose.onNodeWithText("Pause").performClick()
        compose.onNodeWithText("Confirm").performClick()
        compose.waitUntil(5_000) { server.writes.get() == 1 }
        Assert.assertEquals("/api/magician/v2/agents/fixture-crew/pause", server.lastWrite)
        compose.onNodeWithText("‹ Back").performClick(); waitFor("Find crew")
    }
    @Test fun quickCreationFromCrewReturnsToCanonicalTaskWorkspace() {
        val factory: (android.content.Context) -> WorkResource = { context -> WorkResource(context, ::repository, { true }, emptyFlow()) }
        compose.setContent {
            MagicanTheme(themeId.value, activePalette) {
                CompositionLocalProvider(LocalWorkResourceFactory provides factory) {
                    val actions = rememberTasksScreenActions()
                    Column {
                        TasksTopBar({}, actions::showCreate, actions::refresh)
                        WorkScreen(actions, {}, {}, {}, {})
                    }
                }
            }
        }
        compose.onNodeWithTag("work-view-All").assertIsDisplayed()
        compose.onNodeWithTag("work-view-NeedsYou").assertIsDisplayed()
        compose.onNodeWithTag("work-view-Crew").assertIsDisplayed().performClick(); waitFor("Fixture crew")
        compose.onNodeWithContentDescription("New work").performClick()
        compose.onNodeWithText("New Task").assertExists()
        compose.onNodeWithText("Cancel").performClick()
        compose.onNodeWithTag("work-view-All").assertExists()
        Assert.assertEquals(0, server.writes.get())
    }
    @Test fun programHistoryConfirmationCanBeCancelled() {
        mount(WorkSection.Programs); waitFor("Fixture program"); compose.onNodeWithText("Fixture program").performClick()
        waitFor("Responsible crew and progress"); scrollTo("Restore version"); compose.onNodeWithText("Restore version").performClick()
        compose.onNodeWithText("Keep current").performClick()
        Assert.assertEquals(0, server.writes.get())
        compose.onNodeWithText("‹ Back").performClick(); waitFor("Fixture program")
    }
    @Test fun deepLinkDoesNotReopenCrewWhenChoosingPrograms() {
        val factory: (android.content.Context) -> WorkResource = { context -> WorkResource(context, ::repository, { true }, emptyFlow()) }
        compose.setContent {
            MagicanTheme(themeId.value, activePalette) {
                CompositionLocalProvider(LocalWorkResourceFactory provides factory) {
                    WorkScreen(rememberTasksScreenActions(), {}, {}, {}, {})
                }
            }
        }
        compose.onNodeWithTag("work-view-All").assertExists()
        compose.runOnIdle { WorkDeepLinks.request(WorkLinkTarget.Library(WorkSection.Crew, WorkRoute("member", "fixture-crew", "Crew member"))) }
        waitFor("‹ Back"); scrollTo("Definition, rules and model pins")
        compose.onNodeWithText("‹ Back").performClick(); waitFor("Find crew")
        compose.onNodeWithTag("work-more").performClick()
        compose.onNodeWithText("Playbooks").performClick(); waitFor("Fixture program")
        compose.onNodeWithText("Definition, rules and model pins").assertDoesNotExist()
        Assert.assertEquals(0, server.writes.get())
    }
    @Test fun qualityFromCrewKeepsTheSelectedMemberThroughReviewAndStart() {
        mount(); waitFor("Fixture crew"); compose.onNodeWithText("Fixture crew").performClick()
        waitFor("Quality checks"); scrollTo("Quality checks"); compose.onNodeWithText("Quality checks").performClick()
        waitFor("New quality work")
        compose.waitUntil(5_000) { server.readTargets.any { it.contains("/relentless/catalog?") && it.contains("agent_id=fixture-crew") } }
        compose.onNodeWithText("New quality work").performClick()
        waitFor("Outcome")
        compose.onNode(hasSetTextAction() and hasText("Outcome")).performTextInput("A reviewed local draft")
        scrollTo("Quality requirements"); compose.onNode(hasSetTextAction() and hasText("Quality requirements")).performTextInput("Complete and factual")
        scrollTo("Crew member: fixture-crew"); compose.onNodeWithText("Crew member: fixture-crew").assertIsDisplayed()
        scrollTo("Review contract"); compose.onNodeWithText("Review contract").performClick()
        waitFor("Start reviewed work"); scrollTo("Start reviewed work"); compose.onNodeWithText("Start reviewed work").performClick()
        compose.waitUntil(5_000) { server.lastWrite == "/api/magician/v2/relentless/runs" }
        Assert.assertTrue(server.lastBody.contains("\"agent_id\":\"fixture-crew\""))
        Assert.assertTrue(server.lastBody.contains("\"expected_contract_hash\":\"fixture-contract\""))
    }
    @Test fun guidedCrewSavePinsVersionAndCreationPostsTheFullDefinition() {
        mount(); waitFor("Fixture crew"); compose.onNodeWithText("Fixture crew").performClick()
        scrollTo("Edit definition"); compose.onNodeWithText("Edit definition").performClick()
        val name = compose.onNodeWithTag("work-crew-field-name")
        compose.onNodeWithTag("work-crew-editor-scroll").performScrollToNode(hasTestTag("work-crew-field-name")); name.performScrollTo().performTextReplacement("Reviewed member")
        compose.onNodeWithText("Save").performClick()
        compose.waitUntil(5_000) { server.writes.get() == 1 }
        Assert.assertEquals("v1", server.lastIfMatch)
        Assert.assertEquals(JsonObject(mapOf("name" to JsonPrimitive("Reviewed member"))), Json.parseToJsonElement(server.lastBody))
        waitFor("Edit definition"); compose.onNodeWithText("‹ Back").performClick(); waitFor("Add member")
        scrollTo("Add member"); compose.onNodeWithText("Add member").performClick(); waitFor("Identity")
        compose.onNodeWithTag("work-crew-field-name").performScrollTo().performTextInput("New member")
        scrollTo("Create member"); compose.onNodeWithText("Create member").performClick()
        compose.waitUntil(5_000) { server.writes.get() == 2 }
        Assert.assertEquals("/api/magician/v2/agents", server.lastWrite)
        val definition = Json.parseToJsonElement(server.lastBody) as JsonObject
        Assert.assertEquals("New member", definition.workText("name")); Assert.assertEquals("worker", definition.workText("kind"))
        Assert.assertNotNull(definition.workText("persona"))
    }
    @Test fun memoryGoalsAndIsolatedUserMergeUseRealTierAndOwner() {
        mount(WorkSection.Crew, WorkRoute("memory-tier", "fixture-crew", "Memory tier", goalId = "goal/+launch", tier = "goal-memory"))
        waitFor("Persisted memory"); scrollTo("Persisted memory")
        waitFor("Exact goal memory")
        Assert.assertTrue(server.readTargets.any { it.contains("/memory/goal-memory?goal_id=goal%2F%2Blaunch") })
        compose.onNodeWithText("‹ Back").performClick(); waitFor("Fixture crew"); compose.onNodeWithText("Fixture crew").performClick()
        scrollTo("User memory and sharing"); compose.onNodeWithText("User memory and sharing").performClick()
        waitFor("Merge into shared user memory"); scrollTo("Merge into shared user memory"); compose.onNodeWithText("Merge into shared user memory").assertIsEnabled().performClick()
        compose.onNodeWithText("Merge reviewed memory").performClick()
        compose.waitUntil(5_000) { server.writes.get() == 1 }
        Assert.assertEquals("/api/magician/v2/agents/fixture-crew/consolidate-user-memory", server.lastWrite)
    }
    @Test fun noteAttachmentFreezesProviderBytesIntoTheReviewedContract() {
        mount(WorkSection.Quality, WorkRoute("quality-compose", title = "Quality work", agentId = "fixture-crew"))
        waitFor("Outcome"); compose.onNode(hasSetTextAction() and hasText("Outcome")).performTextInput("Reviewed source")
        scrollTo("Find a reference"); compose.onNode(hasSetTextAction() and hasText("Find a reference")).performTextInput("launch")
        scrollTo("Search notes"); compose.onNodeWithText("Search notes").assertIsEnabled().performClick()
        waitFor("Launch evidence"); scrollTo("Launch evidence"); compose.onNodeWithText("Launch evidence").performClick()
        waitFor("Remove Launch evidence")
        scrollTo("Quality requirements"); compose.onNode(hasSetTextAction() and hasText("Quality requirements")).performTextInput("Ground every claim")
        scrollTo("Review contract"); compose.onNodeWithText("Review contract").performClick()
        compose.waitUntil(5_000) { server.lastWrite == "/api/magician/v2/relentless/preview" }
        val proposal = Json.parseToJsonElement(server.lastBody) as JsonObject
        Assert.assertTrue(proposal.workField("sources").workText("notes:launch").orEmpty().contains("Exact provider source\nsecond line"))
        Assert.assertTrue(server.writeBodies.any { it.first.endsWith("/notes/search") && (Json.parseToJsonElement(it.second) as JsonObject).workText("provider") == "fixture-notes" })
    }
    @Test fun comparisonKeepsPreviousAndCurrentBytesSeparateWithoutApproving() {
        mount(WorkSection.Quality, WorkRoute("artifact", "fixture-run/fixture-hash", "Artifact"))
        waitFor("Compare previous attempt"); scrollTo("Compare previous attempt"); compose.onNodeWithText("Compare previous attempt").performClick()
        waitFor("Compare exact attempts"); waitFor("Previous attempt · previous-hash")
        scrollTo("Previous attempt · previous-hash"); compose.onNodeWithText("Previous attempt · previous-hash").performClick()
        waitFor("Previous fixture output")
        scrollTo("Current attempt · fixture-hash"); compose.onNodeWithText("Current attempt · fixture-hash").performClick()
        waitFor("Exact fixture output")
        compose.onNodeWithText("I reviewed this file").assertDoesNotExist()
        Assert.assertEquals(0, server.writes.get())
    }
    @Test fun exactArtifactApprovalRequiresReviewedFiles() {
        mount(WorkSection.Quality, WorkRoute("artifact", "fixture-run/fixture-hash", "Artifact"))
        waitFor("Approve exact draft"); scrollTo("Approve exact draft")
        compose.onNodeWithText("Approve exact draft").assertIsNotEnabled()
        scrollTo("Mark files reviewed"); compose.onNodeWithText("Mark files reviewed").performClick()
        compose.onNodeWithText("Approve exact draft").assertIsEnabled().performClick()
        compose.waitUntil(5_000) { server.writes.get() == 1 }
        Assert.assertEquals("/api/magician/v2/relentless/runs/fixture-run/review", server.lastWrite)
        Assert.assertTrue(server.lastBody.contains("\"artifact_hash\":\"fixture-hash\""))
    }
    @Test fun operatorDecisionUsesReviewedProposalIdentity() {
        mount(WorkSection.Crew, WorkRoute("operator", "fixture-crew", "Operator view"))
        waitFor("Approve"); scrollTo("Approve"); compose.onNodeWithText("Approve").performClick()
        compose.onNodeWithText("Confirm").performClick()
        compose.waitUntil(5_000) { server.writes.get() == 1 }
        Assert.assertEquals("/api/magician/v2/proposals/fixture-proposal/resolve", server.lastWrite)
        Assert.assertTrue(server.lastBody.contains("\"decision\":\"approve\""))
    }
    @Test fun captureBindingRequiresReviewedBytesAndExactPolicyVersion() {
        mount(WorkSection.Quality, WorkRoute("capture-bind", "fixture-capture-run", "Local capture", "fixture-hash", "fixture-active"))
        waitFor("Bind local capture · fixture-recipient"); scrollTo("Bind local capture · fixture-recipient")
        compose.onNodeWithText("Bind local capture · fixture-recipient").assertIsNotEnabled()
        scrollTo("Inspect exact accepted artifact"); compose.onNodeWithText("Inspect exact accepted artifact").performClick()
        waitFor("Mark exact files reviewed"); compose.onAllNodes(hasScrollAction()).onLast().performScrollToNode(hasText("Mark exact files reviewed")); compose.onNodeWithText("Mark exact files reviewed").performClick()
        compose.onNodeWithText("‹ Capture").performClick()
        scrollTo("Bind local capture · fixture-recipient"); compose.onNodeWithText("Bind local capture · fixture-recipient").assertIsEnabled().performClick()
        compose.waitUntil(5_000) { server.writes.get() == 1 }
        Assert.assertEquals("/api/magician/v2/relentless/campaigns/fixture-campaign/captures", server.lastWrite)
        Assert.assertTrue(server.lastBody.contains("\"expected_version\":\"fixture-policy-version\""))
        Assert.assertTrue(server.lastBody.contains("\"artifact_hash\":\"fixture-hash\""))
        Assert.assertTrue(server.lastBody.contains("\"identity_id\":\"fixture-recipient\""))
        Assert.assertTrue(server.lastBody.contains("\"run_id\":\"fixture-capture-run\""))
    }
    @Test fun adoptedMonitoringRejectsEvidenceWithChangedHash() {
        mount(WorkSection.Quality, WorkRoute("learning-policies", title = "Learning policies"))
        waitFor("Adopted run fixture-run"); scrollTo("Sealed monitoring evidence")
        compose.onNodeWithText("Sealed monitoring evidence").performClick()
        waitFor("Evidence changed or is not sealed. Refresh the recorded policy result.")
        compose.onNodeWithText("untrusted-fixture-marker").assertDoesNotExist()
        Assert.assertEquals(0, server.writes.get())
    }
    @Test fun manualCompanySequenceDispatchesTheLiveFocusAreas() {
        mount(WorkSection.Operations); waitFor("Start team sequence")
        scrollTo("Start team sequence"); compose.onNodeWithText("Start team sequence").performClick()
        compose.onNodeWithText("Dispatch reviewed sequence").performClick()
        compose.waitUntil(8_000) { server.writes.get() == 1 }
        Assert.assertEquals(listOf("fixture-crew"), server.writePaths.map { it.substringBeforeLast('/').substringAfterLast('/') })
        Assert.assertTrue(server.lastBody.contains("harness:fixture-crew:launch"))
        Assert.assertTrue(server.lastBody.contains("manual.harness.company_loop"))
    }
    @Test fun everyNativeThemeKeepsCrewControlsInteractive() {
        mount(); waitFor("Fixture crew")
        val previousFamily = Themes.DEFAULT
        try {
            Themes.families.flatMap { listOf(it.id to AppearanceMode.Day, it.id to AppearanceMode.Night) }.forEach { (id, mode) ->
                compose.runOnIdle { themeId.value = Themes.resolve(id, mode, false); applyTheme(id, mode, false) }
                compose.onNodeWithText("Find crew").assertIsDisplayed()
                compose.onNodeWithText("Fixture crew").assertIsDisplayed()
            }
        } finally { compose.runOnIdle { themeId.value = previousFamily; applyTheme(previousFamily, AppearanceMode.Day, false) } }
    }
    @Test fun connectionChangeRejectsPendingConfirmationAndClearsContent() = runBlocking {
        val current = AtomicBoolean(true); val changes = MutableSharedFlow<String>()
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val resource = withContext(Dispatchers.Main) { WorkResource(context, ::repository, { current.get() }, changes).also { it.load("/api/magician/v2/agents") } }
        repeat(100) { if (resource.state.value.loading) delay(10) }
        Assert.assertNotNull(resource.state.value.value)
        val oldIdentity = resource.state.value.identity
        current.set(false); changes.emit("changed"); delay(30)
        val result = withContext(Dispatchers.Main) { resource.perform("/api/magician/v2/agents/fixture-crew/pause", oldIdentity) }
        Assert.assertNull(result); Assert.assertNull(resource.state.value.value); Assert.assertEquals(0, server.writes.get())
        current.set(true); withContext(Dispatchers.Main) { resource.refresh() }
        Assert.assertNull(resource.state.value.value)
        withContext(Dispatchers.Main) { resource.close() }
    }
}

/** Minimal HTTP fixture; never persists enrollment or exposes a runtime token. */
private class WorkHTTPFixture : AutoCloseable {
    private val socket = ServerSocket(0, 16, InetAddress.getByName("127.0.0.1"))
    val origin = "http://127.0.0.1:${socket.localPort}"
    val writes = AtomicInteger()
    val writePaths = java.util.concurrent.CopyOnWriteArrayList<String>()
    val readTargets = java.util.concurrent.CopyOnWriteArrayList<String>()
    @Volatile var lastWrite = ""
    @Volatile var lastBody = ""
    @Volatile var lastIfMatch = ""
    val writeBodies = java.util.concurrent.CopyOnWriteArrayList<Pair<String, String>>()
    private val worker = Thread {
        while (!socket.isClosed) try { socket.accept().use { client ->
            client.soTimeout = 5_000
            val reader = client.getInputStream().bufferedReader()
            val request = reader.readLine() ?: return@use
            var length = 0
            var ifMatch = ""
            while (true) {
                val line = reader.readLine() ?: break
                if (line.isEmpty()) break
                if (line.startsWith("Content-Length:", true)) length = line.substringAfter(':').trim().toInt()
                if (line.startsWith("If-Match:", true)) ifMatch = line.substringAfter(':').trim()
            }
            val chars = CharArray(length); var read = 0
            while (read < length) { val count = reader.read(chars, read, length - read); if (count < 0) break; read += count }
            val path = request.split(' ')[1].substringBefore('?')
            if (request.startsWith("GET ")) readTargets.add(request.split(' ')[1])
            if (!request.startsWith("GET ")) { lastWrite = path; lastBody = String(chars); lastIfMatch = ifMatch; writePaths.add(path); writeBodies.add(path to lastBody); writes.incrementAndGet() }
            val response = payload(path).toByteArray()
            client.getOutputStream().write("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ${response.size}\r\nConnection: close\r\n\r\n".toByteArray())
            client.getOutputStream().write(response); client.getOutputStream().flush()
        } } catch (_: Exception) { }
    }.apply { isDaemon = true; start() }
    override fun close() { socket.close(); worker.join(1_000) }
    private fun payload(path: String) = when {
        path.endsWith("/ui-threads") -> """{"threads":[{"id":"general","name":"General"},{"id":"launch","name":"Launch"},{"id":"archived","name":"Archived","archived":true}]}"""
        path.endsWith("/notes/tree") -> """{"provider":"fixture-notes"}"""
        path.endsWith("/notes/search") -> """{"hits":[{"title":"Launch evidence","relative_path":"launch.md","source_ref":"notes:launch","provider":"fixture-notes"}]}"""
        path.endsWith("/notes/file") -> """{"title":"Launch evidence","relative_path":"launch.md","provider":"fixture-notes","markdown":"Exact provider source\nsecond line"}"""
        path.endsWith("/memory/goal-memory") -> """{"tier":{"scope":"goal","description":"Goal evidence"},"goal_id":"goal/+launch","data":"Exact goal memory","shared":false}"""
        path.endsWith("/memory") -> """{"tiers":[{"tier_name":"goal-memory","scope":"goal","has_data":true,"available_goal_ids":["goal/+launch"]},{"tier_name":"user-memory","scope":"user","has_data":true,"shared":false}]}"""
        path.endsWith("/harness-overview") -> """{"context":{"program":{"relative_path":"fixture.md"}},"evaluation":{"active_program_state":{"program":{"relative_path":"fixture.md"},"current_phase":"Verify migration","current_step":"Keep task controls","update_history":[{"reason":"Checked exact task evidence","task_id":"fixture-detail-done","execution_id":"exec-fixture"}]}}}"""
        path.endsWith("/llm/routing") -> """{"models":[{"id":"m-a","provider":"openai","label":"Actor model","choices":{"auto":"fixture-actor"}},{"id":"m-b","provider":"anthropic","label":"Critic model","choices":{"auto":"fixture-critic"}}]}"""
        path.endsWith("/relentless/catalog") -> """{"items":[],"pagination":{"has_more":false}}"""
        path.endsWith("/relentless/profiles") -> """{"profiles":[{"name":"fixture-actor","family":"actor"},{"name":"fixture-critic","family":"critic"}],"actor_default":"fixture-actor","critic_default":"fixture-critic"}"""
        path.endsWith("/relentless/preview") -> """{"contract_hash":"fixture-contract","budget_can_admit_attempt":true}"""
        path.endsWith("/relentless/runs") -> """{"id":"fixture-run"}"""
        path.endsWith("/agents") -> """{"agents":[{"definition":{"agent_id":"fixture-crew","name":"Fixture crew","kind":"Personal","persona":"Native fixture assistant","trust_level":"local","harness":{},"autonomous_config":{"schedule":"0 */4 * * *","max_tasks_per_cycle":3,"max_steps_per_plan":10,"focus_areas":[{"name":"Launch","description":"Prepare launch","program":"fixture.md","priority":"medium"}]},"is_primary":true},"etag":"v1","status":"active","current_cycle_id":"fixture-cycle","current_execution_id":"exec-fixture"}]}"""
        path.endsWith("/agents/fixture-crew") -> """{"definition":{"agent_id":"fixture-crew","name":"Fixture crew","kind":"Personal","persona":"Native fixture assistant","trust_level":"local","harness":{},"autonomous_config":{"schedule":"0 */4 * * *","max_tasks_per_cycle":3,"max_steps_per_plan":10,"focus_areas":[{"name":"Launch","description":"Prepare launch","program":"fixture.md","priority":"medium"}]},"user_memory_isolation":"fully_isolated","model_pins":{"planning":"fixture-model"},"future_extension":{"budget":18446744073709551615},"is_primary":true},"etag":"v1","status":"active","current_cycle_id":"fixture-cycle","current_execution_id":"exec-fixture"}"""
        path.endsWith("/proposals") -> """{"proposals":[{"id":"fixture-proposal","source":"harness:fixture-crew","status":"pending","summary":"Reviewed structural fixture"}]}"""
        path.endsWith("/programs") -> """{"programs":[{"name":"fixture.md","title":"Fixture program"}]}"""
        path.endsWith("/programs/fixture.md") -> """{"name":"fixture.md","title":"Fixture program","content":"# Direction\nKeep full task controls.","history":[{"snapshot":"fixture-v1","modified":"2026-10-06"}]}"""
        path.endsWith("/runs/fixture-run") -> """{"id":"fixture-run","status":"accepted","contract":{"goal":"Fixture quality work"},"attempts":[{"number":1,"artifact":{"hash":"previous-hash"}},{"number":2,"artifact":{"hash":"fixture-hash"}}],"routing":{"route":"human_review","artifact_hash":"fixture-hash"},"owner_approved":null,"handoff":null}"""
        path.endsWith("/runs/fixture-capture-run") -> """{"id":"fixture-capture-run","status":"accepted","contract":{"goal":"Fixture capture work"},"handoff":{"kind":"capture","artifact_hash":"fixture-hash"}}"""
        path.endsWith("/artifacts/fixture-hash") -> """{"files":{"draft.md":"Exact fixture output"},"binary_files":{}}"""
        path.endsWith("/artifacts/previous-hash") -> """{"files":{"draft.md":"Previous fixture output"},"binary_files":{}}"""
        path.endsWith("/campaigns") -> """{"campaigns":[{"state":{"campaign":{"id":"fixture-campaign","version_hash":"fixture-policy-version","revoked_at":null,"spec":{"name":"Fixture capture policy","targets":[{"task_id":"fixture-active","identity_id":"fixture-recipient"}]}}}}],"pagination":{"has_more":false}}"""
        path.endsWith("/guidance-policies") -> """{"policies":[{"policy":{"id":"fixture-learning","version_hash":"fixture-learning-version","spec":{"name":"Fixture learning policy","targets":[]}},"adoptions":{"slot":{"run_id":"fixture-run"}},"monitored":{"fixture-run":{"evidence_hash":"recorded-monitor-hash"}}}],"pagination":{"has_more":false}}"""
        path.endsWith("/evidence/fixture-run") -> """{"sealed":true,"content_hash":"changed-monitor-hash","evidence":{"marker":"untrusted-fixture-marker"}}"""
        else -> "{}"
    }
}
