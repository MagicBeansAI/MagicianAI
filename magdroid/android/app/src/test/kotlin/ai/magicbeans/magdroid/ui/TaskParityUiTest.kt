package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.TaskCardAction
import ai.magicbeans.magdroid.tasks.TaskV3
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TaskParityUiTest {
    @Test
    fun detail_shows_exactly_one_reset() {
        val failed = TaskV3(id = "t", status = "failed", latestRootExecutionId = "e")
        assertTrue(detailHeaderShowsReset(failed))
        assertTrue(detailHeaderShowsReset(failed.copy(status = "canceled")))
        assertTrue(detailHeaderShowsReset(failed.copy(status = "paused")))
        // No execution: nothing to reset, so neither header nor overflow offers it.
        assertFalse(detailHeaderShowsReset(failed.copy(latestRootExecutionId = null)))
        assertFalse(failed.copy(latestRootExecutionId = null).canResetToReady)
        // Plan review owns the header of a paused task; the overflow keeps the Reset.
        val reviewing = failed.copy(status = "paused", hasPlan = true, latestPlanId = "p", planStatus = "draft")
        assertTrue(detailHeaderShowsPlanReview(reviewing))
        assertFalse(detailHeaderShowsReset(reviewing))
        assertTrue(reviewing.canResetToReady)
        // A finished task's stale draft plan is not offered for approval.
        val finished = TaskV3(id = "c", status = "completed", hasPlan = true, latestPlanId = "p")
        assertFalse(detailHeaderShowsPlanReview(finished))
        assertTrue(detailHeaderShowsReset(failed.copy(hasPlan = true, latestPlanId = "p")))
    }

    @Test
    fun card_labels_and_recurring_chip_copy() {
        assertEquals("Result", taskCardActionLabel(TaskCardAction.Result))
        assertEquals("Publish to Notes", taskCardActionLabel(TaskCardAction.PublishToNotes))
        assertEquals("Reset to Ready", taskCardActionLabel(TaskCardAction.Reset))
        assertEquals("↻ Recurring", TASK_RECURRING_LABEL)
    }

    @Test
    fun output_and_history_copy() {
        assertEquals("Intermediate artifacts & evidence (1 item)", intermediatesTitle(1))
        assertEquals("Intermediate artifacts & evidence (4 items)", intermediatesTitle(4))
        val utc = java.time.ZoneOffset.UTC
        assertEquals(
            "Sep 28, 10:00 → 10:05 · 5m",
            historySpanLine("2026-09-28T10:00:00Z", "2026-09-28T10:05:00Z", utc),
        )
        assertEquals(
            "Sep 28, 23:50 → Sep 29, 00:10 · 20m",
            historySpanLine("2026-09-28T23:50:00Z", "2026-09-29T00:10:00Z", utc),
        )
        assertEquals("Sep 28, 10:00", historySpanLine("2026-09-28T10:00:00Z", null, utc))
        assertEquals("soon", historySpanLine("soon", null, utc))
        assertEquals("05:43:40", activityClock(java.time.Instant.parse("2026-09-23T05:43:40Z").toEpochMilli(), utc))
    }

    @Test
    fun verdict_detail_wraps_and_delegation_accents_follow_the_palette() {
        assertEquals(3, TASK_VERDICT_DETAIL_MAX_LINES)
        val day = Themes.palette("longhand")
        assertEquals(day.info, delegationAccent("running", day))
        assertEquals(day.success, delegationAccent("done", day))
        assertEquals(day.success, delegationAccent("completed", day))
        assertEquals(day.danger, delegationAccent("failed", day))
        assertEquals(day.warning, delegationAccent("waiting", day))
    }
}
