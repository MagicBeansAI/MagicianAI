package ai.magicbeans.magdroid.vibedev

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** A scripted server: canned reads, and every write recorded. */
private class FakeVibeDev : VibeDevSource {
    var live = VibeDevLiveRuns()
    var projectList = VibeDevProjects()
    var pages = mutableListOf<VibeDevRunsPage>()
    var controlOutcome = VibeDevControlOutcome.Delivered
    val calls = mutableListOf<String>()
    var reads = 0

    override suspend fun liveRuns(): VibeDevLiveRuns { reads++; return live }
    override suspend fun projects(): VibeDevProjects = projectList
    override suspend fun projectRuns(projectId: String, limit: Int, offset: Int, query: String?): VibeDevRunsPage {
        calls += "runs:$offset:${query.orEmpty()}"
        return if (offset == 0) pages.first() else pages[1]
    }
    override suspend fun crews(projectId: String) = projectList.projects.first { it.projectId == projectId }.crews
    override suspend fun runTurns(runId: String) = VibeDevRunTurns(runId, turns = listOf(VibeDevRunTurn(runId, status = "running")))
    override suspend fun timeline(turnTaskId: String, limit: Int) = VibeDevTimeline(turnTaskId)
    var changesFor: (String, String?) -> VibeDevRunChanges = { runId, _ -> VibeDevRunChanges(runId) }
    /** How long one file's changes read takes, in virtual time. */
    var changesDelayMs: (String?) -> Long = { 0L }
    override suspend fun changes(runId: String, turn: String?, path: String?): VibeDevRunChanges {
        changesDelayMs(path).takeIf { it > 0 }?.let { delay(it) }
        return changesFor(runId, path)
    }
    override suspend fun repoSuggestions() = emptyList<VibeDevRepoSuggestion>()
    override suspend fun codingProfiles() = listOf(VibeDevCodingProfile("pi", "Pi", isDefault = true))
    override suspend fun startRun(request: VibeDevRunRequest): VibeDevStartedRun {
        calls += "start:${request.clientRunId}:${request.parentTaskId}"
        return VibeDevStartedRun("task_new")
    }
    override suspend fun createProject(name: String, repoPath: String) = VibeDevProject("p_new", name)
    override suspend fun updateProject(projectId: String, trusted: Boolean?, baseBranch: String?): VibeDevProject {
        calls += "patch:$projectId:$trusted:$baseBranch"
        return VibeDevProject(projectId, trust = if (trusted == true) "trusted" else null)
    }
    override suspend fun control(liveTaskId: String, action: String, message: String?): VibeDevControlOutcome {
        calls += "control:$liveTaskId:$action:${message.orEmpty()}"
        return controlOutcome
    }
    override suspend fun createCrew(request: VibeDevCrewRequest) = VibeDevCrew("c_new", request.goal)
    override suspend fun crewAction(projectId: String, crewId: String, action: String): VibeDevCrew {
        calls += "crew:$crewId:$action"
        return VibeDevCrew(crewId)
    }
    override suspend fun chooseCrew(projectId: String, crewId: String, laneIndex: Int?): VibeDevCrew {
        calls += "choose:$crewId:$laneIndex"
        return VibeDevCrew(crewId)
    }
    override suspend fun answerQuestion(requestId: String, optionId: String) { calls += "answer:$requestId:$optionId" }
    override suspend fun merge(rootTaskId: String) { calls += "merge:$rootTaskId" }
    override suspend fun openPullRequest(rootTaskId: String, title: String, body: String) { calls += "pr:$rootTaskId" }
    override suspend fun discard(rootTaskId: String) { calls += "discard:$rootTaskId" }
    override suspend fun answerDiffApproval(taskIds: Collection<String>, approve: Boolean, approvalId: String?) {
        calls += "approve:$approve:${approvalId.orEmpty()}"
    }
}

private fun run(id: String, state: String, updated: String = "2026-10-13T09:00:00Z", project: String = "p1") =
    VibeDevLiveRun(runId = id, liveTaskId = "${id}_live", projectId = project, projectName = "Shop", title = "Run $id", stateWire = state, updatedAt = updated)

@OptIn(ExperimentalCoroutinesApi::class)
class VibeDevViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @Before fun setUp() { Dispatchers.setMain(dispatcher) }
    @After fun tearDown() { Dispatchers.resetMain() }

    private fun TestScope.now(fake: FakeVibeDev, live: kotlinx.coroutines.flow.Flow<VibeDevSignal> = emptyFlow()) =
        VibeDevNowViewModel({ fake }, live, clock = { testScheduler.currentTime })

    @Test
    fun `needs you lists crew questions first, then waiting runs, newest first`() {
        val older = VibeDevCrew("c_old", "Old goal", status = "proposed", updatedAtMs = 1_000)
        val newer = VibeDevCrew("c_new", "New goal", status = "choosing", mode = "race", updatedAtMs = 5_000,
            lanes = listOf(VibeDevCrewLane("Attempt 1", status = "done"), VibeDevCrewLane("Attempt 2", status = "done")))
        val quiet = VibeDevCrew("c_run", "Running", status = "running", updatedAtMs = 9_000)
        val merged = VibeDevCrew("c_pr", "Merged", status = "integrated", branch = "vibedev/x",
            pullRequestRequestId = "vibedev-crew-c_pr-pr", updatedAtMs = 2_000)
        val projects = listOf(VibeDevProject("p1", "Shop", crews = listOf(older, newer, quiet, merged)))
        val runs = listOf(
            run("r_old", "needs_you", "2026-10-13T08:00:00Z"),
            run("r_work", "working"),
            run("r_new", "needs_you", "2026-10-13T10:00:00Z"),
        )
        val order = VibeDevNeedsYou.order(projects, runs).map { it.key }
        assertEquals(
            listOf(
                "crew:c_new:Choose", "crew:c_pr:PullRequest", "crew:c_old:Start",
                "run:r_new", "run:r_old",
            ),
            order,
        )
    }

    @Test
    fun `archived projects ask nothing`() {
        val projects = listOf(VibeDevProject("p1", archived = true, crews = listOf(VibeDevCrew("c", status = "proposed"))))
        assertTrue(VibeDevNeedsYou.order(projects, emptyList()).isEmpty())
    }

    @Test
    fun `stop waits out the undo window and undo sends nothing`() = runTest(dispatcher) {
        val fake = FakeVibeDev().apply { live = VibeDevLiveRuns(listOf(run("r1", "working"))) }
        val model = now(fake)
        model.refresh(); advanceUntilIdle()
        val target = model.data.value.working.single()

        model.stop(target)
        runCurrent()
        assertTrue("the row leaves at once", model.data.value.working.isEmpty())
        assertEquals("Stopping Run r1", model.undo.pending.value?.message)
        advanceTimeBy(1_500)
        model.undo.undo()
        advanceTimeBy(5_000); advanceUntilIdle()
        assertTrue(fake.calls.none { it.startsWith("control") })
        assertNull(model.undo.pending.value)
        assertEquals(listOf("r1"), model.data.value.working.map { it.runId })
    }

    @Test
    fun `stop is sent to the live turn after two seconds`() = runTest(dispatcher) {
        val fake = FakeVibeDev().apply { live = VibeDevLiveRuns(listOf(run("r1", "working"))) }
        val model = now(fake)
        model.refresh(); advanceUntilIdle()
        model.stop(model.data.value.working.single())
        advanceTimeBy(1_999); runCurrent()
        assertTrue(fake.calls.none { it.startsWith("control") })
        advanceTimeBy(2); advanceUntilIdle()
        assertEquals(listOf("control:r1_live:stop:"), fake.calls.filter { it.startsWith("control") })
        assertNull(model.undo.pending.value)
    }

    @Test
    fun `a second destructive action sends the first at once`() = runTest(dispatcher) {
        val fake = FakeVibeDev().apply { live = VibeDevLiveRuns(listOf(run("r1", "working"), run("r2", "working"))) }
        val model = now(fake)
        model.refresh(); advanceUntilIdle()
        val (first, second) = model.data.value.working
        model.stop(first)
        runCurrent()
        model.stop(second)
        runCurrent()
        assertEquals(listOf("control:r1_live:stop:"), fake.calls.filter { it.startsWith("control") })
        model.undo.undo()
        advanceUntilIdle()
        assertEquals(1, fake.calls.count { it.startsWith("control") })
    }

    @Test
    fun `keep none hides the crew during the window and undo brings it back`() = runTest(dispatcher) {
        val race = VibeDevCrew("c1", "Race", status = "choosing", mode = "race",
            lanes = listOf(VibeDevCrewLane("Attempt 1", status = "done")))
        val fake = FakeVibeDev().apply { projectList = VibeDevProjects(listOf(VibeDevProject("p1", "Shop", crews = listOf(race)))) }
        val model = now(fake)
        model.refresh(); advanceUntilIdle()
        model.decide("p1", race, VibeDevCrewDecision.KeepNone)
        runCurrent()
        assertEquals(setOf("c1"), model.hiddenCrewIds.value)
        model.undo.undo()
        advanceUntilIdle()
        assertTrue(model.hiddenCrewIds.value.isEmpty())
        assertTrue(fake.calls.none { it.startsWith("choose") })

        model.decide("p1", race, VibeDevCrewDecision.KeepNone)
        advanceTimeBy(2_100); advanceUntilIdle()
        assertEquals(listOf("choose:c1:null"), fake.calls.filter { it.startsWith("choose") })
    }

    @Test
    fun `crew decisions reach their endpoint or question`() = runTest(dispatcher) {
        val crew = VibeDevCrew("c1", status = "integrated", pullRequestRequestId = "q-pr", resolveRequestId = "q-res",
            startRequestId = "q-start")
        val fake = FakeVibeDev()
        val model = now(fake)
        model.decide("p1", crew, VibeDevCrewDecision.Start); advanceUntilIdle()
        model.decide("p1", crew, VibeDevCrewDecision.Keep(2, "Attempt 3")); advanceUntilIdle()
        model.decide("p1", crew, VibeDevCrewDecision.NotNow); advanceUntilIdle()
        model.decide("p1", crew, VibeDevCrewDecision.ResolveMyself); advanceUntilIdle()
        model.decide("p1", crew, VibeDevCrewDecision.OpenPullRequest); advanceUntilIdle()
        assertEquals(
            listOf("crew:c1:start", "choose:c1:2", "answer:q-pr:later", "answer:q-res:manual", "crew:c1:pull-request"),
            fake.calls,
        )
    }

    @Test
    fun `a steer between turns says to send a follow-up`() = runTest(dispatcher) {
        val fake = FakeVibeDev().apply { controlOutcome = VibeDevControlOutcome.BetweenTurns }
        val model = now(fake)
        model.steer(run("r1", "working"), "  use postgres  ")
        advanceUntilIdle()
        assertEquals(listOf("control:r1_live:steer:use postgres"), fake.calls)
        // The notice was shown and the re-read cleared nothing else.
        assertFalse(model.loadStatus.value.busy.contains("run:r1"))
    }

    @Test
    fun `live events re-read once per debounce window, and only while attached`() = runTest(dispatcher) {
        val events = MutableSharedFlow<VibeDevSignal>(extraBufferCapacity = 16)
        val fake = FakeVibeDev()
        val model = now(fake, events)
        model.attach(); advanceUntilIdle()
        val afterAttach = fake.reads
        repeat(5) { events.tryEmit(VibeDevSignal.Event); advanceTimeBy(100) }
        advanceTimeBy(1_100); advanceUntilIdle()
        assertEquals(afterAttach + 1, fake.reads)
        model.detach()
        events.tryEmit(VibeDevSignal.Event)
        advanceTimeBy(2_000); advanceUntilIdle()
        assertEquals(afterAttach + 1, fake.reads)
    }

    @Test
    fun `a failed re-read keeps the last data and its time`() = runTest(dispatcher) {
        var fail = false
        val fake = object : VibeDevSource by FakeVibeDev() {
            override suspend fun liveRuns(): VibeDevLiveRuns {
                if (fail) throw VibeDevError(0, "offline")
                return VibeDevLiveRuns(listOf(run("r1", "working")))
            }
            override suspend fun projects() = VibeDevProjects()
        }
        val model = VibeDevNowViewModel({ fake }, emptyFlow(), clock = { testScheduler.currentTime })
        model.refresh(); advanceUntilIdle()
        val stamped = model.loadStatus.value.updatedAtMs
        fail = true
        advanceTimeBy(60_000)
        model.refresh(); advanceUntilIdle()
        assertEquals("offline", model.loadStatus.value.error)
        assertEquals(stamped, model.loadStatus.value.updatedAtMs)
        assertEquals(1, model.data.value.working.size)
    }

    @Test
    fun `project runs page, fold turns, and search from the first page`() = runTest(dispatcher) {
        val root = VibeDevTaskItem("t1", "Root", status = "completed")
        val turn = VibeDevTaskItem("t2", "Follow", status = "running", tags = listOf(VibeDevTag("vibedev-threaded")))
        val older = VibeDevTaskItem("t3", "Older", status = "completed")
        val fake = FakeVibeDev().apply {
            projectList = VibeDevProjects(listOf(VibeDevProject("p1", "Shop", trust = "trusted")))
            pages = mutableListOf(
                VibeDevRunsPage(listOf(root, turn), pagination = VibeDevRunsPagination(runs = 1, totalRuns = 2, offset = 0, hasMore = true)),
                VibeDevRunsPage(listOf(older), pagination = VibeDevRunsPagination(runs = 1, totalRuns = 2, offset = 1, hasMore = false)),
            )
        }
        val model = VibeDevProjectViewModel("p1", { fake }, emptyFlow(), clock = { testScheduler.currentTime })
        model.refresh(); advanceUntilIdle()
        val first = model.data.value
        assertEquals(1, first.runs.size)
        assertEquals(2, first.runs.single().turns.size)
        assertEquals(VibeDevRunState.Working, first.runs.single().state)
        assertTrue(first.hasMore)
        model.loadMore(); advanceUntilIdle()
        assertEquals(listOf("t1", "t3"), model.data.value.runs.map { it.root.id })
        assertFalse(model.data.value.hasMore)
        model.search("auth"); advanceTimeBy(500); advanceUntilIdle()
        assertEquals("runs:0:auth", fake.calls.last())
    }

    @Test
    fun `a new sheet mints a new client run id and a retry keeps it`() = runTest(dispatcher) {
        var next = 0
        val fake = object : VibeDevSource by FakeVibeDev() {
            var attempts = 0
            val ids = mutableListOf<String>()
            override suspend fun startRun(request: VibeDevRunRequest): VibeDevStartedRun {
                ids += request.clientRunId
                if (attempts++ == 0) throw VibeDevError(502, "Bad gateway")
                return VibeDevStartedRun("task_x")
            }
        }
        val composer = VibeDevComposerViewModel({ fake }, newId = { "id-${next++}" })
        composer.open(); advanceUntilIdle()
        composer.startRun("Do it", "p1", "build", null, null, null); advanceUntilIdle()
        assertEquals("Bad gateway", composer.state.value.error)
        composer.startRun("Do it", "p1", "build", null, null, null); advanceUntilIdle()
        assertEquals(fake.ids[0], fake.ids[1])
        assertTrue(composer.state.value.result is VibeDevComposed.RunStarted)
        composer.open()
        composer.startRun("Again", "p1", "build", null, null, null); advanceUntilIdle()
        assertTrue(fake.ids[2] != fake.ids[1])
    }

    @Test
    fun `a trusted new project is patched after it is created`() = runTest(dispatcher) {
        val fake = FakeVibeDev()
        val composer = VibeDevComposerViewModel({ fake })
        composer.createProject("Billing", "/code/billing", trusted = true, baseBranch = " main ")
        advanceUntilIdle()
        assertEquals(listOf("patch:p_new:true:main"), fake.calls)
        val created = composer.state.value.result as VibeDevComposed.ProjectCreated
        assertTrue(created.project.isTrusted)
    }

    private val patchText = "@@ -1 +1 @@\n-old\n+new\n"

    @Test
    fun `a diff stays on its file by path when the changed list shifts`() = runTest(dispatcher) {
        val files = listOf(VibeDevChangedFile("a.kt"), VibeDevChangedFile("b.kt"), VibeDevChangedFile("c.kt"))
        val fake = FakeVibeDev().apply {
            // a.kt was reverted by a later turn: c.kt moves from third to second.
            changesFor = { runId, path ->
                VibeDevRunChanges(runId, files = files.drop(1), patch = VibeDevFilePatch(path!!, patchText))
            }
        }
        val model = VibeDevDiffViewModel("r1", null, files, "c.kt", { fake }, emptyFlow())
        model.refresh(); advanceUntilIdle()
        val data = model.data.value
        assertEquals(1, data.index)
        assertEquals("c.kt", data.file?.path)
        assertEquals("c.kt", data.patch?.path)
        assertTrue(data.diff != null)
        assertNull(model.loadStatus.value.error)
    }

    @Test
    fun `a diff whose file left the changes says so instead of loading forever`() = runTest(dispatcher) {
        val files = listOf(VibeDevChangedFile("src/a.kt"), VibeDevChangedFile("b.kt"))
        val fake = FakeVibeDev().apply {
            changesFor = { runId, _ -> VibeDevRunChanges(runId, files = listOf(VibeDevChangedFile("b.kt"))) }
        }
        val model = VibeDevDiffViewModel("r1", null, files, "src/a.kt", { fake }, emptyFlow())
        model.refresh(); advanceUntilIdle()
        val status = model.loadStatus.value
        assertFalse(status.loading)
        assertEquals("a.kt is no longer among this run's changes.", status.error)
        assertEquals(listOf("b.kt"), model.data.value.files.map { it.path })
        assertEquals(0, model.data.value.index)
        assertNull(model.data.value.diff)
    }

    @Test
    fun `a slow read does not pull the reader back from the cached file they stepped to`() = runTest(dispatcher) {
        val files = listOf(VibeDevChangedFile("a.kt"), VibeDevChangedFile("b.kt"), VibeDevChangedFile("c.kt"))
        val fake = FakeVibeDev().apply {
            changesFor = { runId, path -> VibeDevRunChanges(runId, files = files, patch = VibeDevFilePatch(path!!, patchText)) }
            changesDelayMs = { path -> if (path == "c.kt") 5_000L else 0L }
        }
        val model = VibeDevDiffViewModel("r1", null, files, "b.kt", { fake }, emptyFlow())
        model.refresh(); advanceUntilIdle()
        // Next to c.kt starts a slow read; Previous back to the cached b.kt.
        model.step(1); runCurrent()
        model.step(-1); runCurrent()
        assertEquals("b.kt", model.data.value.patch?.path)
        advanceUntilIdle()
        val data = model.data.value
        assertEquals("b.kt", data.file?.path)
        assertEquals("b.kt", data.patch?.path)
        assertTrue(data.diff != null)
        // c.kt's read landed in the cache: Next shows it without a read.
        model.step(1)
        assertEquals("c.kt", model.data.value.patch?.path)
    }

    @Test
    fun `a late read for a vanished file leaves the file showing alone`() = runTest(dispatcher) {
        val files = listOf(VibeDevChangedFile("a.kt"), VibeDevChangedFile("b.kt"), VibeDevChangedFile("c.kt"))
        var cGone = false
        val fake = FakeVibeDev().apply {
            changesFor = { runId, path ->
                val listed = if (cGone) files.dropLast(1) else files
                VibeDevRunChanges(runId, files = listed, patch = path?.takeIf { p -> listed.any { it.path == p } }?.let { VibeDevFilePatch(it, patchText) })
            }
            changesDelayMs = { path -> if (path == "c.kt") 5_000L else 0L }
        }
        val model = VibeDevDiffViewModel("r1", null, files, "b.kt", { fake }, emptyFlow())
        model.refresh(); advanceUntilIdle()
        model.step(1); runCurrent()
        model.step(-1); runCurrent()
        cGone = true
        advanceUntilIdle()
        val data = model.data.value
        assertEquals("b.kt", data.file?.path)
        assertEquals("b.kt", data.patch?.path)
        assertTrue(data.diff != null)
        assertNull(model.loadStatus.value.error)
    }

    @Test
    fun `a read that outlives its machine reaches neither the card nor the cache`() = runTest(dispatcher) {
        val fake = FakeVibeDev().apply { live = VibeDevLiveRuns(listOf(run("r1", "working"))) }
        val folder = java.nio.file.Files.createTempDirectory("vibedev-cache").toFile()
        var machine = "a"
        val reported = mutableListOf<List<VibeDevLiveRun>>()
        val model = VibeDevNowViewModel(
            { fake }, emptyFlow(), clock = { testScheduler.currentTime },
            cache = { VibeDevCache(java.io.File(folder, machine)) },
            onRuns = { reported += it },
            // The switch lands while the read is out.
            identity = { machine.also { machine = "b" } },
        )
        model.refresh(); advanceUntilIdle()
        assertTrue(reported.isEmpty())
        assertTrue(model.data.value.runs.isEmpty())
        assertFalse(java.io.File(folder, "b").exists())
        assertFalse(java.io.File(folder, "a").exists())
        folder.deleteRecursively()
    }

    @Test
    fun `a read on a steady machine reaches the card`() = runTest(dispatcher) {
        val fake = FakeVibeDev().apply { live = VibeDevLiveRuns(listOf(run("r1", "working"))) }
        val reported = mutableListOf<List<VibeDevLiveRun>>()
        val model = VibeDevNowViewModel({ fake }, emptyFlow(), clock = { testScheduler.currentTime },
            onRuns = { reported += it }, identity = { "a" })
        model.refresh(); advanceUntilIdle()
        assertEquals(listOf("r1"), reported.single().map { it.runId })
        assertEquals(1, model.data.value.runs.size)
    }

    @Test
    fun `a run read that outlives its machine is dropped`() = runTest(dispatcher) {
        var machine = "a"
        val model = VibeDevRunViewModel("r1", { FakeVibeDev() }, emptyFlow(), clock = { testScheduler.currentTime },
            identity = { machine.also { machine = "b" } })
        model.refresh(); advanceUntilIdle()
        assertNull(model.data.value.turns)
    }

    @Test
    fun `approve and reject are offered only for a diff approval`() {
        val diff = run("r1", "needs_you").copy(approvalId = "ccp-1")
        val question = run("r2", "needs_you")
        assertTrue(diff.hasApproval)
        assertFalse(question.hasApproval)
        assertFalse(question.copy(approvalId = " ").hasApproval)
        assertTrue(VibeDevRunData(live = diff).canAnswerApproval)
        // A question, a plan or a sandbox ask: answered in Attention.
        assertFalse(VibeDevRunData(live = question).canAnswerApproval)
        // An approval id on a run no longer waiting is not answerable.
        assertFalse(VibeDevRunData(live = diff.copy(stateWire = "working")).canAnswerApproval)
    }
}
