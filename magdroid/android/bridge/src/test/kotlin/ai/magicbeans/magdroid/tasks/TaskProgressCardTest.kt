package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The panel-to-card digest behind the task-progress notification. These pin
 * the reading to iOS's `BackgroundEngine`, field for field — two phones
 * watching the same run must describe it with the same words.
 */
class TaskProgressCardTest {

    private fun panel(
        status: String = "running",
        title: String = "Summarise the thread",
        log: List<Pair<String?, String?>>? = null,
        recent: List<Pair<String?, String?>> = emptyList(),
    ) = buildJsonObject {
        putJsonObject("overview") {
            put("status", status)
            put("title", title)
        }
        putJsonObject("run") {
            log?.let { entries ->
                putJsonArray("activity_log") {
                    entries.forEach { (entryTitle, content) ->
                        add(
                            buildJsonObject {
                                // Not `?.let { put } ?: put(JsonNull)` — put()
                                // returns the PREVIOUS value (null), so the
                                // elvis branch would always run and null every
                                // title it had just set.
                                if (entryTitle != null) {
                                    put("title", entryTitle)
                                } else {
                                    put("title", JsonNull)
                                }
                                content?.let { put("content", it) }
                            },
                        )
                    }
                }
            }
            putJsonArray("recent_activity") {
                recent.forEach { (entryTitle, content) ->
                    add(
                        buildJsonObject {
                            entryTitle?.let { put("title", it) }
                            content?.let { put("content", it) }
                        },
                    )
                }
            }
        }
    }

    @Test
    fun `the newest activity line is the status and the feed length the count`() {
        val card = taskProgressCard(
            panel(log = listOf("Reading files" to null, "Drafting reply" to null)),
            fallbackTitle = "fallback",
        )
        assertEquals("Summarise the thread", card?.title)
        assertEquals("Drafting reply", card?.status)
        assertEquals(2, card?.stepCount)
        assertFalse(card!!.done)
    }

    @Test
    fun `the full log wins over the recent window even when empty`() {
        // iOS nil-coalesces activityLog ?? recentActivity: a present-but-empty
        // log is chosen, so both cards count the same feed.
        val card = taskProgressCard(
            panel(log = emptyList(), recent = listOf("Old step" to null)),
            fallbackTitle = "fallback",
        )
        assertEquals(0, card?.stepCount)
        assertEquals("Working…", card?.status)
    }

    @Test
    fun `the newest-first recent fallback uses its first entry`() {
        val card = taskProgressCard(
            panel(
                log = null,
                recent = listOf(
                    "Newest step" to null,
                    "Older step" to null,
                ),
            ),
            fallbackTitle = "fallback",
        )

        assertEquals("Newest step", card?.status)
        assertEquals(2, card?.stepCount)
    }

    @Test
    fun `a title-less entry falls back to its content, then to working`() {
        assertEquals(
            "wrote three paragraphs",
            taskProgressCard(
                panel(log = listOf(null to "wrote three paragraphs")),
                fallbackTitle = "x",
            )?.status,
        )
        assertEquals(
            "Working…",
            taskProgressCard(panel(log = listOf(null to null)), fallbackTitle = "x")?.status,
        )
    }

    @Test
    fun `terminal statuses settle the card with their own words`() {
        assertEquals("Done.", taskProgressCard(panel(status = "completed"), "x")?.status)
        assertTrue(taskProgressCard(panel(status = "completed"), "x")!!.done)
        assertEquals("Did not finish.", taskProgressCard(panel(status = "failed"), "x")?.status)
        assertEquals("Cancelled.", taskProgressCard(panel(status = "cancelled"), "x")?.status)
    }

    @Test
    fun `an unknown status keeps showing progress rather than declaring an end`() {
        val card = taskProgressCard(panel(status = "paused_for_review"), "x")
        assertFalse(card!!.done)
    }

    @Test
    fun `a blank overview title falls back to the dispatch's name`() {
        assertEquals(
            "what the keyboard sent",
            taskProgressCard(panel(title = ""), "what the keyboard sent")?.title,
        )
    }

    @Test
    fun `a frame without an overview changes nothing`() {
        assertNull(taskProgressCard(null, "x"))
        assertNull(taskProgressCard(buildJsonObject { put("run", buildJsonObject {}) }, "x"))
    }

    @Test
    fun `a delayed push cannot overwrite the task that superseded it`() {
        assertTrue(TaskProgressNotifier.shouldApplyRemote("task-new", "task-new"))
        assertFalse(TaskProgressNotifier.shouldApplyRemote("task-new", "task-old"))
        assertFalse(TaskProgressNotifier.shouldApplyRemote(null, "task-old"))
        assertFalse(TaskProgressNotifier.shouldApplyRemote("", "task-old"))
    }

    @Test
    fun `restored task tracking cannot outlive its watchdog`() {
        val now = 2_000_000L
        assertTrue(isTaskTrackingFresh(now - TaskProgressNotifier.WATCHDOG_MS, now))
        assertFalse(isTaskTrackingFresh(now - TaskProgressNotifier.WATCHDOG_MS - 1, now))
        assertFalse(isTaskTrackingFresh(0, now))
        assertFalse(isTaskTrackingFresh(now + 60_001, now))
    }

    @Test
    fun `socket and push task updates share one monotonic ordering contract`() {
        assertTrue(isNewerTaskEvent(previousTimestamp = 100, candidateTimestamp = 101))
        assertFalse(isNewerTaskEvent(previousTimestamp = 100, candidateTimestamp = 100))
        assertFalse(isNewerTaskEvent(previousTimestamp = 100, candidateTimestamp = 99))
        assertFalse(isNewerTaskEvent(previousTimestamp = 0, candidateTimestamp = 0))
    }
}
