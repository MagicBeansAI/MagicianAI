package ai.magicbeans.magdroid.vibedev

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The VibeDev wire contract, against the canonical fixtures.
 *
 * The JSON files in `magician/tests/fixtures/vibedev/` are written from the server's own
 * types (`vibedev::wire_fixtures`) and read by `MagiosTests` and the web tests
 * too. A field renamed on the wire would otherwise decode here into a default,
 * silently; these assert the values, not just that decoding did not throw.
 */
class VibeDevFixtureTest {

    private fun fixture(name: String): String = File(fixtureRoot(), name).readText()

    private fun <T> decode(serializer: kotlinx.serialization.KSerializer<T>, name: String): T =
        vibeDevJson.decodeFromString(serializer, fixture(name))

    @Test
    fun `live runs decode with states and the steer target`() {
        val live = decode(VibeDevLiveRuns.serializer(), "live_runs.json")
        assertEquals(4, live.runs.size)
        val ask = live.runs.first { it.runId == "task_ask" }
        assertEquals(VibeDevRunState.NeedsYou, ask.state)
        // A run waiting on a diff approval names the approval to answer directly.
        assertEquals("ccp-6f1d2a90-3b7c-4e55-9a10-2f4c8d7e1b30", ask.approvalId)
        assertEquals(VibeDevWaitingKind.DiffReview, ask.waitingKind)
        // An Autopilot run's briefing is a notice on a done run, never needs-you.
        val briefed = live.runs.first { it.runId == "task_report" }
        assertEquals(VibeDevRunState.Done, briefed.state)
        assertEquals(VibeDevWaitingKind.Notice, briefed.waitingKind)
        assertNull(briefed.approvalId)
        val notice = briefed.notice!!
        assertEquals("acknowledge", notice.optionId)
        assertTrue(notice.requestId.startsWith("vibedev-run-ended:"))
        assertTrue(notice.message.isNotBlank())
        val working = live.runs.first { it.runId == "task_api" }
        assertEquals(VibeDevRunState.Working, working.state)
        assertEquals("task_api", working.liveTaskId)
        assertEquals("API", working.lane)
        assertEquals("crew_7f3a", working.crewId)
        assertEquals("Editing src/api/auth/codes.ts", working.step)
        assertEquals("Shop", working.projectName)
        assertNotNull(isoMillis(working.updatedAt))
        assertEquals(isoMillis("2026-10-13T09:12:00Z"), isoMillis(working.startedAt))
        val done = live.runs.first { it.runId == "task_search" }
        assertEquals(VibeDevRunState.Done, done.state)
        assertTrue(done.summary!!.contains("trigram"))
        assertEquals(0.88, done.costUsd!!, 0.0001)
        assertNull(working.costUsd)
        assertNull(working.approvalId)
        // The lock-screen summary agrees with the server's for the same runs.
        val summary = CodingRunsSummary.fromLive(live.runs, 1L)
        assertEquals(1, summary.needsYou); assertEquals(1, summary.working); assertEquals(2, summary.done)
        assertEquals("Add dark-mode screenshots · Shop", summary.headline)
        assertEquals("Needs you", summary.detail)
        assertTrue(summary.active)
        assertEquals(1, live.counts["needs_you"])
        assertEquals(1, live.counts["working"])
        assertEquals(2, live.counts["done"])
    }

    @Test
    fun `live run waiting kind and notice read leniently`() {
        val odd = vibeDevJson.decodeFromString(
            VibeDevLiveRun.serializer(),
            """{"run_id":"r","live_task_id":"r","project_id":"p","state":"done",
               "waiting_kind":"later_kind","notice":"not an object"}""",
        )
        assertEquals(VibeDevRunState.Done, odd.state)
        assertNull(odd.waitingKind)
        assertNull(odd.notice)
        val old = vibeDevJson.decodeFromString(
            VibeDevLiveRun.serializer(),
            """{"run_id":"r","live_task_id":"r","project_id":"p","state":"needs_you"}""",
        )
        assertNull(old.waitingKind)
        assertNull(old.notice)
    }

    @Test
    fun `projects decode with trust, crews and links`() {
        val projects = decode(VibeDevProjects.serializer(), "projects.json").projects
        assertTrue("the fixture should carry projects", projects.isNotEmpty())
        projects.forEach { project ->
            assertTrue(project.projectId.isNotBlank())
            assertTrue(project.name.isNotBlank())
        }
        // Trust is absent for sandboxed and `trusted` otherwise.
        projects.forEach { assertTrue(it.trust == null || it.trust == "trusted" || it.trust == "sandboxed") }
        val raw = vibeDevJson.parseToJsonElement(fixture("projects.json")).toString()
        if (raw.contains("\"trusted\"")) assertTrue(projects.any { it.isTrusted })
        if (raw.contains("\"repo_display_path\"")) assertTrue(projects.any { it.repoDisplayPath.isNotBlank() })
        if (raw.contains("\"crews\"")) assertTrue(projects.any { it.crews.isNotEmpty() })
        if (raw.contains("\"run_links\"")) assertTrue(projects.any { it.runLinks.isNotEmpty() })
        val shop = projects.single { it.projectId == "proj_shop" }
        assertEquals("Shop", shop.name)
        assertTrue(shop.isTrusted)
        assertEquals("main", shop.baseBranch)
        assertEquals("~/code/shop", shop.repoLabel)
        assertEquals("/Users/owner/code/shop", shop.repoAbsolutePath)
        assertEquals(listOf("task_search", "task_api"), shop.runTaskIds)
        assertFalse(shop.archived)
        assertEquals(1_760_346_000_000, shop.updatedAtMs)
        assertEquals("proj_shop", decode(VibeDevProjects.serializer(), "projects.json").activeProjectId)
    }

    @Test
    fun `a runs page decodes and folds turns into their runs`() {
        val page = decode(VibeDevRunsPage.serializer(), "runs_page.json")
        assertTrue("the fixture page should carry tasks", page.tasks.isNotEmpty())
        page.tasks.forEach { assertTrue(it.id.isNotBlank()) }
        val rows = page.runRows()
        assertTrue(rows.isNotEmpty())
        assertEquals(page.tasks.size, rows.sumOf { it.turns.size })
        // A root is never a threaded turn unless its parent is off the page.
        rows.forEach { row -> assertEquals(row.root, row.turns.first()) }
        if (page.tasks.any { it.isThreaded }) assertTrue(rows.any { it.turns.size > 1 })
        assertTrue(page.pagination.totalRuns >= page.pagination.runs)
        assertEquals(rows.size, page.pagination.runs)
        assertEquals(listOf("task_api", "task_search"), rows.map { it.root.id })
        val api = rows[0]
        assertEquals("API", api.lane)
        assertEquals("crew_7f3a", api.crewId)
        assertEquals(VibeDevRunState.Working, api.state)
        val search = rows[1]
        assertEquals(listOf("task_search", "task_search_2"), search.turns.map { it.id })
        assertEquals("task_search", search.turns[1].parentFromDescription)
        // The newest turn is still going, so the run is.
        assertEquals(VibeDevRunState.Working, search.state)
        assertFalse(page.pagination.hasMore)
        assertNull(page.pagination.nextCursor)
    }

    @Test
    fun `crews decode with lanes, roles and questions`() {
        val crews = decode(VibeDevCrews.serializer(), "crews.json").crews
        assertEquals(2, crews.size)
        val split = crews.first { it.crewId == "crew_7f3a" }
        assertEquals("running", split.status)
        assertEquals("split", split.mode)
        assertTrue(split.review)
        assertEquals(6.0, split.budgetUsd!!, 0.001)
        assertEquals(1.48, split.spentUsd!!, 0.001)
        assertEquals(listOf("API", "Login form"), split.lanes.map { it.lane })
        assertEquals(listOf("done", "running"), split.lanes.map { it.status })
        assertEquals("task_api", split.lanes[0].runTaskId)
        assertEquals(listOf("src/api/auth"), split.lanes[0].owns)
        assertEquals("work", split.lanes[0].role)
        assertTrue(split.isRunning)
        assertNull(VibeDevCrewQuestion.of(split))

        val race = crews.first { it.crewId == "crew_race" }
        assertTrue(race.isRace)
        assertEquals("choosing", race.status)
        assertEquals("vibedev-crew-crew_race-choose", race.chooseRequestId)
        val question = VibeDevCrewQuestion.of(race)!!
        assertEquals(VibeDevCrewQuestionKind.Choose, question.kind)
        // Only the finished attempt can be kept, by its index in `lanes`.
        assertEquals(listOf("Keep Attempt 1 · claude-code", "Keep none"), question.options.map { it.label })
        assertEquals(VibeDevCrewDecision.Keep(0, "Attempt 1 · claude-code"), question.options[0].decision)
    }

    @Test
    fun `run turns decode with engine, cost and commit`() {
        val turns = decode(VibeDevRunTurns.serializer(), "run_turns.json")
        assertEquals("task_search", turns.runId)
        assertEquals("proj_shop", turns.projectId)
        assertTrue(turns.trusted)
        assertEquals("vibedev/shop-search01", turns.branch)
        assertEquals(2, turns.turns.size)
        val root = turns.turns[0]
        assertEquals("build", root.mode)
        assertEquals("codex", root.engine)
        assertEquals(0.61, root.costUsd!!, 0.001)
        assertEquals("4be1c09d2a7f", root.commit)
        assertEquals(2, root.filesChanged)
        assertEquals(VibeDevRunState.Done, root.state)
        val follow = turns.turns[1]
        assertEquals(VibeDevRunState.Working, follow.state)
        assertNull(follow.costUsd)
        assertEquals("task_search_2", turns.liveTaskId)
        assertEquals(0.61, turns.costUsd!!, 0.001)
    }

    @Test
    fun `a turn timeline decodes into its cards`() {
        val timeline = decode(VibeDevTimeline.serializer(), "turn_timeline.json")
        assertEquals("task_search", timeline.taskId)
        assertFalse(timeline.truncated)
        val kinds = timeline.cards.map { it.kind }.toSet()
        val known = setOf("message", "reasoning", "action", "error", "completed", "notice")
        assertTrue("unknown kinds in $kinds", known.containsAll(kinds))
        assertTrue("reasoning" in kinds)
        assertTrue("action" in kinds)
        assertTrue("completed" in kinds)
        val actions = timeline.cards.filter { it.kind == "action" }
        assertTrue(actions.any { it.status == "error" })
        assertTrue(actions.any { it.status == "ok" })
        assertEquals(timeline.cards.map { it.index }, timeline.cards.map { it.index }.sorted())
        assertTrue(timeline.cards.all { it.atMs > 0 && it.title.isNotBlank() })
    }

    @Test
    fun `run changes decode and the patch parses`() {
        val changes = decode(VibeDevRunChanges.serializer(), "run_changes.json")
        assertEquals("task_search", changes.runId)
        assertEquals("branch", changes.source)
        assertEquals(listOf("db/migrations/0042_title_trigram.sql", "src/api/search.ts"), changes.files.map { it.path })
        assertEquals(listOf("A", "M"), changes.files.map { it.status })
        assertEquals(5, changes.files[1].additions)
        assertEquals(2, changes.files[1].deletions)
        val patch = changes.patch!!
        assertEquals("src/api/search.ts", patch.path)
        assertFalse(patch.truncated)
        val diff = UnifiedDiffParser.parse(patch.unifiedDiff)
        assertEquals(1, diff.hunks.size)
        // The parsed counts agree with the server's.
        assertEquals(changes.files[1].additions, diff.additions)
        assertEquals(changes.files[1].deletions, diff.deletions)
        assertEquals(10, diff.hunks[0].oldStart)
        assertEquals(4, diff.hunks[0].oldCount)
        assertEquals(7, diff.hunks[0].newCount)
        // The header agrees with the body it heads.
        val hunk = diff.hunks[0]
        assertEquals(hunk.oldCount, hunk.lines.count { it.kind != DiffLineKind.Added && it.kind != DiffLineKind.Note })
        assertEquals(hunk.newCount, hunk.lines.count { it.kind != DiffLineKind.Removed && it.kind != DiffLineKind.Note })
        assertEquals("export async function search(q: string) {", diff.hunks[0].section)
    }

    @Test
    fun `repo suggestions decode`() {
        val repos = decode(VibeDevRepoSuggestions.serializer(), "repos_suggest.json").repos
        assertEquals(listOf("shop", "billing"), repos.map { it.name })
        assertEquals("Shop", repos[0].project)
        assertNull(repos[1].project)
        assertTrue(repos.all { it.path.startsWith("/") })
    }

    private fun fixtureRoot(): File {
        var directory: File? = File(System.getProperty("user.dir") ?: ".").absoluteFile
        while (directory != null) {
            val candidate = File(directory, "magician/tests/fixtures/vibedev")
            if (candidate.isDirectory) return candidate
            directory = directory.parentFile
        }
        error("could not locate the VibeDev fixtures from ${System.getProperty("user.dir")}")
    }
}
