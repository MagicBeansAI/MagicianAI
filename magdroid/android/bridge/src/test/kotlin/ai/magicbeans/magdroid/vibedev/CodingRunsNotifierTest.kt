package ai.magicbeans.magdroid.vibedev

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CodingRunsNotifierTest {
    private val push = mapOf(
        "kind" to "coding_runs",
        "working" to "2",
        "needs_you" to "1",
        "done_count" to "3",
        "failed" to "0",
        "headline" to "API lane · Shop",
        "detail" to "Needs you",
        "active" to "true",
        // A delivery flag, not the count.
        "done" to "false",
        "event_timestamp" to "1760346000123",
        "deep_link" to "magican://vibe",
    )

    @Test
    fun `a coding runs data message parses with done_count as the count`() {
        val summary = CodingRunsSummary.fromPush(push)!!
        assertEquals(
            CodingRunsSummary(2, 1, 3, 0, "API lane · Shop", "Needs you", true, 1_760_346_000_123, deepLink = "magican://vibe"),
            summary,
        )
    }

    @Test
    fun `other kinds, missing timestamps and junk counts are handled`() {
        assertNull(CodingRunsSummary.fromPush(push + ("kind" to "task_progress")))
        assertNull(CodingRunsSummary.fromPush(push - "event_timestamp"))
        val junk = CodingRunsSummary.fromPush(push + ("working" to "x") + ("failed" to "-2") - "active")!!
        assertEquals(0, junk.working)
        assertEquals(0, junk.failed)
        // Without `active`, a waiting run still counts as active.
        assertTrue(junk.active)
        val ended = CodingRunsSummary.fromPush(push + ("active" to "false") + ("done" to "true"))!!
        assertFalse(ended.active)
        assertEquals(3, ended.done)
    }

    @Test
    fun `an active summary is an ongoing card with the counts that goes stale after thirty minutes`() {
        val card = codingRunsCard(CodingRunsSummary.fromPush(push)!!)!!
        assertEquals("API lane · Shop", card.title)
        assertEquals("Needs you", card.text)
        assertEquals("2 running · 1 needs you · 3 done", card.counts)
        assertTrue(card.ongoing)
        // An older Magician's summary has no featured item: no list, no buttons.
        assertTrue(card.lines.isEmpty())
        assertTrue(card.actions.isEmpty())
        // Longer than the server's ten-minute heartbeat, so only a missed
        // end lets it run out.
        assertEquals(30 * 60_000L, card.timeoutMs)
        assertTrue(card.timeoutMs!! > 10 * 60_000L)
    }

    @Test
    fun `a live card counts only while it is fresh and still in the shade`() {
        assertTrue(codingRunsShowingActive(activeUntilMs = 2_000, nowMs = 1_000, cardPresent = true))
        // The system cannot say: trust the window.
        assertTrue(codingRunsShowingActive(activeUntilMs = 2_000, nowMs = 1_000, cardPresent = null))
        // Stale, never posted, or dismissed / cleared by a reboot.
        assertFalse(codingRunsShowingActive(activeUntilMs = 2_000, nowMs = 2_000, cardPresent = true))
        assertFalse(codingRunsShowingActive(activeUntilMs = 0, nowMs = 1_000, cardPresent = null))
        assertFalse(codingRunsShowingActive(activeUntilMs = 2_000, nowMs = 1_000, cardPresent = false))
    }

    @Test
    fun `an inactive summary with nothing showing posts nothing`() {
        // A server restart's first sweep sends "not active" to every route.
        val ended = CodingRunsSummary(0, 0, 0, 0, "", "", false, 500)
        val showing = codingRunsShowingActive(activeUntilMs = 0, nowMs = 1_000, cardPresent = false)
        assertEquals(CodingRunsAction.Ignore, codingRunsAction(ended, enabled = true, lastSameSource = 0, showingActive = showing))
        // A card that went stale is not "showing" either.
        val stale = codingRunsShowingActive(activeUntilMs = 900, nowMs = 1_000, cardPresent = true)
        assertEquals(CodingRunsAction.Ignore, codingRunsAction(ended, enabled = true, lastSameSource = 0, showingActive = stale))
    }

    @Test
    fun `an ended summary shows what just ended briefly and nothing ends the card`() {
        assertNull(codingRunsCard(CodingRunsSummary(0, 0, 3, 1, "", "", false, 5)))
        val justDone = CodingRunsSummary(
            0, 0, 1, 0, "Search · Shop", "", false, 5,
            featured = WorkSummaryLine("run:a", "coding_run", "done", "Search"),
        )
        val card = codingRunsCard(justDone)!!
        assertEquals("Search", card.title)
        assertEquals("Done", card.text)
        assertFalse(card.ongoing)
        assertEquals(2 * 60_000L, card.timeoutMs)
    }

    @Test
    fun `blank headline and detail fall back`() {
        val card = codingRunsCard(CodingRunsSummary(1, 0, 0, 0, "", "", true, 5))!!
        assertEquals("Magician at work", card.title)
        assertEquals("1 running", card.text)
    }

    @Test
    fun `the work summary push parses the featured item, next and actions`() {
        val work = push + mapOf(
            "featured_id" to "run:task_1",
            "featured_kind" to "coding_run",
            "featured_state" to "needs_you",
            "featured_title" to "Add clearCompleted",
            "featured_context" to "Shop",
            "featured_step" to "Waiting on approval",
            "action_kind" to "approve_diff",
            "action_id" to "prop_1",
            "more" to "2",
            "next" to """[{"id":"crew:crew_1","kind":"crew","state":"working","title":"Checkout"},""" +
                """{"id":"task:t","kind":"work_task","state":"paused","title":""},{"id":"x","kind":"","state":"","title":"third"}]""",
            "deep_link" to "magican://vibe?project=p&run=task_1",
        )
        val summary = CodingRunsSummary.fromPush(work)!!
        assertEquals(WorkSummaryLine("run:task_1", "coding_run", "needs_you", "Add clearCompleted"), summary.featured)
        assertEquals(2, summary.next.size)
        assertEquals(2, summary.more)
        val card = codingRunsCard(summary)!!
        assertEquals("Add clearCompleted", card.title)
        assertEquals("Needs you", card.text)
        assertEquals(
            listOf(
                "Add clearCompleted · Shop — Needs you",
                "Checkout — Working",
                "Task — Paused",
                "+2 more",
                "2 running · 1 needs you · 3 done",
            ),
            card.lines,
        )
        assertEquals(listOf(WorkCardAction.Approve, WorkCardAction.Open, WorkCardAction.Follow), card.actions)
        // Followed already: no Follow.
        assertEquals(listOf(WorkCardAction.Approve, WorkCardAction.Open), codingRunsCard(summary, setOf("run:task_1"))!!.actions)
        // A broken `next` is no list, not a failure.
        assertTrue(CodingRunsSummary.fromPush(work + ("next" to "not json"))!!.next.isEmpty())
    }

    @Test
    fun `the needs-you heads-up rises only when more need the owner`() {
        val one = CodingRunsSummary(1, 1, 0, 0, "h", "d", true, 5)
        assertTrue(shouldAlertNeedsYou(one, lastNeedsYou = 0))
        assertFalse(shouldAlertNeedsYou(one, lastNeedsYou = 1))
        assertTrue(shouldAlertNeedsYou(one.copy(needsYou = 2), lastNeedsYou = 1))
        assertFalse(shouldAlertNeedsYou(one.copy(active = false), lastNeedsYou = 0))
    }

    @Test
    fun `work links open runs in VibeDev and tasks by their task link`() {
        assertEquals("magican://vibe?project=p&run=r", workItemLink("run:r", "magican://vibe?project=p&run=r"))
        assertEquals("magican://task/task_9", workItemLink("task:task_9", "magican://work?task=task_9"))
        assertEquals("magican://task/task_9", workItemLink(null, "magican://work?task=task_9"))
        assertEquals("magican://vibe", workItemLink("crew:c", null))
        assertEquals("magican://vibe", workItemLink("crew:c", "https://elsewhere.example"))
    }

    @Test
    fun `the summary from a work live read features the ranked item`() {
        val live = WorkLive(
            featured = WorkItem("crew:c1", "crew", "Checkout crew", "Shop", "working", step = "Merging lanes"),
            next = listOf(WorkItem("task:t1", "work_task", "Weekly report", "", "paused")),
            counts = WorkCounts(needsYou = 0, working = 2, failed = 0, done = 1),
            items = listOf(
                WorkItem("crew:c1", "crew", "Checkout crew"),
                WorkItem("task:t1", "work_task", "Weekly report"),
                WorkItem("run:r", "coding_run", "Old", state = "done"),
            ),
        )
        val summary = CodingRunsSummary.fromWork(live, 7)
        assertTrue(summary.active)
        assertEquals("Checkout crew · Shop", summary.headline)
        assertEquals("Merging lanes", summary.featuredStep)
        assertEquals(1, summary.more)
        assertEquals("Merging lanes", codingRunsCard(summary)!!.text)
    }

    @Test
    fun `a followed item push parses, and an ended one stops being ongoing`() {
        val data = mapOf(
            "kind" to "work_item", "item_id" to "crew:c1", "item_kind" to "crew", "state" to "needs_you",
            "title" to "Checkout crew", "context" to "Shop", "step" to "", "action_kind" to "approve_diff",
            "action_id" to "prop_2", "deep_link" to "magican://vibe?project=p&crew=c1", "done" to "false",
            "urgent" to "true", "event_timestamp" to "9",
        )
        val update = FollowedWorkUpdate.fromPush(data)!!
        val card = followedWorkCard(update)
        assertEquals("Checkout crew", card.title)
        assertEquals("Needs you", card.text)
        assertEquals("VibeDev crew · Shop", card.subText)
        assertTrue(card.ongoing)
        assertTrue(card.approve)
        val ended = followedWorkCard(FollowedWorkUpdate.fromPush(data + ("state" to "done") + ("done" to "true"))!!)
        assertEquals("Done", ended.text)
        assertFalse(ended.ongoing)
        assertFalse(ended.approve)
        assertEquals(10 * 60_000L, ended.timeoutMs)
        assertNull(FollowedWorkUpdate.fromPush(data - "item_id"))
        assertNull(FollowedWorkUpdate.fromPush(data + ("kind" to "coding_runs")))
    }

    @Test
    fun `older summaries are ignored and finished only replaces a live card`() {
        val live = CodingRunsSummary(1, 0, 0, 0, "h", "d", true, 100)
        val ended = live.copy(working = 0, active = false, eventTimestamp = 200)
        assertEquals(CodingRunsAction.Post, codingRunsAction(live, enabled = true, lastSameSource = 50, showingActive = false))
        assertEquals(CodingRunsAction.Ignore, codingRunsAction(live, enabled = true, lastSameSource = 100, showingActive = true))
        assertEquals(CodingRunsAction.Post, codingRunsAction(ended, enabled = true, lastSameSource = 100, showingActive = true))
        // A quiet refresh with nothing going posts nothing.
        assertEquals(CodingRunsAction.Ignore, codingRunsAction(ended, enabled = true, lastSameSource = 100, showingActive = false))
        assertEquals(CodingRunsAction.Cancel, codingRunsAction(live, enabled = false, lastSameSource = 0, showingActive = true))
    }

    @Test
    fun `the in-app summary leads with what needs the owner`() {
        val runs = listOf(
            VibeDevLiveRun("a", "a", "p", "Shop", "Search", stateWire = "working", step = "Editing search.ts"),
            VibeDevLiveRun("b", "b", "p", "Shop", "Form", stateWire = "needs_you", lane = "Login form"),
            VibeDevLiveRun("c", "c", "p", "Shop", "Old", stateWire = "failed"),
        )
        val summary = CodingRunsSummary.fromLive(runs, 9)
        assertEquals("Login form lane · Shop", summary.headline)
        assertEquals("Needs you", summary.detail)
        assertEquals(1, summary.failed)
        val working = CodingRunsSummary.fromLive(runs.take(1), 9)
        assertEquals("Search · Shop", working.headline)
        assertEquals("Editing search.ts", working.detail)
        assertFalse(CodingRunsSummary.fromLive(runs.drop(2), 9).active)
    }

    /** The notifier's watermarks, replayed without Android: one per source. */
    private class Shade {
        val last = mutableMapOf(CodingRunsSource.Push to 0L, CodingRunsSource.App to 0L)
        var showingActive = false
        var shown: CodingRunsSummary? = null
        fun receive(summary: CodingRunsSummary, source: CodingRunsSource): CodingRunsAction {
            val action = codingRunsAction(summary, enabled = true, lastSameSource = last.getValue(source), showingActive = showingActive)
            if (action == CodingRunsAction.Post) {
                last[source] = summary.eventTimestamp
                showingActive = summary.active
                shown = summary
            }
            return action
        }
    }

    @Test
    fun `a push older than the last in-app read by the clocks still applies when it arrives later`() {
        val shade = Shade()
        // The phone's clock runs ahead of the server's.
        val inApp = CodingRunsSummary(1, 0, 0, 0, "Search · Shop", "Editing", true, eventTimestamp = 2_000_000)
        val push = CodingRunsSummary(0, 1, 0, 0, "Form · Shop", "Needs you", true, eventTimestamp = 1_000_000)
        assertEquals(CodingRunsAction.Post, shade.receive(inApp, CodingRunsSource.App))
        assertEquals(CodingRunsAction.Post, shade.receive(push, CodingRunsSource.Push))
        assertEquals("Form · Shop", shade.shown!!.headline)
        // Within a source the clock still orders: a stale push is dropped,
        // and a later in-app read wins again by arriving last.
        assertEquals(CodingRunsAction.Ignore, shade.receive(push.copy(headline = "stale", eventTimestamp = 999_999), CodingRunsSource.Push))
        assertEquals(CodingRunsAction.Ignore, shade.receive(inApp.copy(eventTimestamp = 1_999_999), CodingRunsSource.App))
        assertEquals(CodingRunsAction.Post, shade.receive(inApp.copy(eventTimestamp = 2_000_500), CodingRunsSource.App))
        assertEquals("Search · Shop", shade.shown!!.headline)
    }
}
