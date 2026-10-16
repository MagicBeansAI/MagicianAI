package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.TaskCardAction
import ai.magicbeans.magdroid.tasks.TaskVerdictState
import org.junit.Assert.assertEquals
import org.junit.Test

class TaskParityUiTest {
    @Test
    fun verdict_states_map_to_the_ios_dot_tones() {
        assertEquals(StatusTone.Running, verdictTone(TaskVerdictState.Running))
        assertEquals(StatusTone.NotStarted, verdictTone(TaskVerdictState.Queued))
        assertEquals(StatusTone.Done, verdictTone(TaskVerdictState.Finished))
        assertEquals(StatusTone.Failed, verdictTone(TaskVerdictState.Failed))
        assertEquals(StatusTone.Partial, verdictTone(TaskVerdictState.Stalled))
        assertEquals(StatusTone.Partial, verdictTone(TaskVerdictState.Paused))
        assertEquals(StatusTone.Cancelled, verdictTone(TaskVerdictState.Cancelled))
        assertEquals(StatusTone.Waiting, verdictTone(TaskVerdictState.Waiting))
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
    fun delegation_accents_follow_the_palette() {
        val day = Themes.palette("longhand")
        assertEquals(day.info, delegationAccent("running", day))
        assertEquals(day.success, delegationAccent("done", day))
        assertEquals(day.success, delegationAccent("completed", day))
        assertEquals(day.danger, delegationAccent("failed", day))
        assertEquals(day.warning, delegationAccent("waiting", day))
    }
}
