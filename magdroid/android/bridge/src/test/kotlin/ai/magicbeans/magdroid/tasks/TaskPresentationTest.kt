package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant
import java.time.ZoneOffset

class TaskPresentationTest {
    private fun task(status: String, summary: String? = null, outcome: String? = null, names: List<String>? = null) =
        TaskV3(id = "t", status = status, completionSummary = summary, completionOutcome = outcome, completionArtifactNames = names)

    private fun cron(expression: String) = buildJsonObject {
        putJsonObject("kind") { putJsonObject("Cron") { put("expression", expression) } }
    }

    @Test fun decode_reads_completion_fields_and_tolerates_their_absence() {
        val decoded = taskJson.decodeFromString<TaskV3>(
            """{"id":"a","status":"completed","completion_outcome":"success",
               "completion_artifact_names":["report.md"],"recurring_schedule":{"interval_seconds":3600}}""",
        )
        assertEquals("success", decoded.completionOutcome)
        assertEquals(listOf("report.md"), decoded.completionArtifactNames)
        assertTrue(decoded.isRecurring)
        assertEquals("Every hour", decoded.recurringDescription)

        val bare = taskJson.decodeFromString<TaskV3>("""{"id":"b","completion_artifact_names":null}""")
        assertNull(bare.completionOutcome)
        assertNull(bare.completionArtifactNames)
        assertFalse(bare.isRecurring)
    }

    @Test fun has_result_rule_matches_web() {
        assertTrue(task("running", names = listOf("a.md")).hasFinalResult)
        assertTrue(task("failed", summary = "Did half").hasFinalResult)
        assertFalse(task("completed", summary = "   ").hasFinalResult)
        assertTrue(task("completed", outcome = "Delivered").hasFinalResult)
        for (bad in listOf("Failed", "CANCELLED", "canceled by user", "stopped", "error: boom")) {
            assertFalse(bad, task("completed", outcome = bad).hasFinalResult)
        }
        // A non-failure outcome only counts once the task completed.
        assertFalse(task("failed", outcome = "Delivered").hasFinalResult)
        assertFalse(task("completed", names = emptyList()).hasFinalResult)
    }

    @Test fun completed_actions_are_result_then_publish_or_publish_alone() {
        assertEquals(
            listOf(TaskCardAction.Result, TaskCardAction.PublishToNotes),
            task("completed", summary = "Done").visibleActions,
        )
        assertEquals(TaskCardAction.Result, task("completed", summary = "Done").primaryCardAction)
        assertEquals(listOf(TaskCardAction.PublishToNotes), task("completed").visibleActions)
        assertEquals(TaskCardAction.PublishToNotes, task("completed").primaryCardAction)
    }

    @Test fun failed_actions_depend_on_an_execution_and_never_repeat_result() {
        val noRun = task("cancelled", summary = "partial")
        assertEquals(listOf(TaskCardAction.PublishToNotes, TaskCardAction.Result), noRun.visibleActions)
        assertFalse(noRun.canResetToReady)
        val ran = noRun.copy(latestRootExecutionId = "exec")
        assertEquals(
            listOf(TaskCardAction.Reset, TaskCardAction.PublishToNotes, TaskCardAction.Result),
            ran.visibleActions,
        )
        assertTrue(ran.canResetToReady)
        assertTrue(task("canceled").copy(activeRootExecutionId = "x").canResetToReady)
        assertTrue(task("paused").canResetToReady)
        assertEquals(
            listOf(TaskCardAction.ReviewPlan, TaskCardAction.PublishToNotes),
            task("failed").copy(planStatus = "approved").visibleActions,
        )
        assertEquals(1, task("completed", summary = "x").visibleActions.count { it == TaskCardAction.Result })
    }

    @Test fun recurring_detection_uses_cron_tags_or_app_schedule() {
        assertTrue(TaskV3(id = "c", schedule = cron("0 9 * * *")).isRecurring)
        assertTrue(TaskV3(id = "t", tags = listOf(TaskTag("x", "Recurring"))).isRecurring)
        assertTrue(TaskV3(id = "a", tags = listOf(TaskTag("app_recurring", "App_Recurring"))).isRecurring)
        assertFalse(TaskV3(id = "n", tags = listOf(TaskTag("r", "recurring-ish"))).isRecurring)
        assertFalse(TaskV3(id = "none").isRecurring)

        val tagged = TaskV3(id = "t", schedule = cron("0 9 * * 1-5"), tags = listOf(TaskTag("r", "recurring"), TaskTag("w", "work")))
        assertEquals(listOf("work"), tagged.displayTags.map(TaskTag::name))
        assertEquals("Weekdays at 09:00", tagged.recurringDescription)
        assertNull(TaskV3(id = "t", tags = listOf(TaskTag("r", "recurring"))).recurringDescription)
    }

    @Test fun cron_descriptions_cover_the_common_shapes_and_fall_back_to_raw() {
        assertEquals("Every 15 minutes", TaskPresentation.describeCron("*/15 * * * *"))
        assertEquals("Every minute", TaskPresentation.describeCron("* * * * *"))
        assertEquals("Every hour", TaskPresentation.describeCron("0 * * * *"))
        assertEquals("Every 2 hours", TaskPresentation.describeCron("0 */2 * * *"))
        assertEquals("Daily at 09:00", TaskPresentation.describeCron("0 9 * * *"))
        assertEquals("Daily at 18:30", TaskPresentation.describeCron(" 30  18 * * * "))
        assertEquals("Weekdays at 08:05", TaskPresentation.describeCron("5 8 * * 1-5"))
        assertEquals("Weekly on Monday at 09:00", TaskPresentation.describeCron("0 9 * * 1"))
        assertEquals("Weekly on Sunday at 07:00", TaskPresentation.describeCron("0 7 * * 0"))
        assertEquals("Monthly on day 1 at 06:00", TaskPresentation.describeCron("0 6 1 * *"))
        assertEquals("0 9 1 1 *", TaskPresentation.describeCron("0 9 1 1 *"))
        assertEquals("0 9 * * 1,3", TaskPresentation.describeCron("0 9 * * 1,3"))
        assertEquals("not cron", TaskPresentation.describeCron("not cron"))
        assertEquals("", TaskPresentation.describeCron("  "))
    }

    @Test fun output_scope_grouping_splits_deliverables_from_ordered_intermediates() {
        data class F(val name: String, val scope: TaskOutputScope)
        val files = listOf(
            F("artifact.json", TaskOutputScope.Artifact),
            F("report.md", TaskOutputScope.Task),
            F("child.txt", TaskOutputScope.Delegated),
            F("raw.csv", TaskOutputScope.Execution),
            F("notes.md", TaskOutputScope.fromWire(null)),
        )
        val groups = TaskOutputGrouping.group(files, F::scope)
        assertEquals(listOf("report.md", "notes.md"), groups.deliverables.map(F::name))
        assertEquals(
            listOf(TaskOutputScope.Execution, TaskOutputScope.Delegated, TaskOutputScope.Artifact),
            groups.intermediates.map { it.scope },
        )
        assertEquals(3, groups.intermediateCount)
        assertEquals(TaskOutputScope.Task, TaskOutputScope.fromWire("mystery"))
        assertEquals(TaskOutputScope.Delegated, TaskOutputScope.fromWire("Delegated"))
        assertEquals("Direct outputs", TaskOutputScope.Execution.title)
        assertEquals("Persisted artifacts", TaskOutputScope.Artifact.title)
        assertEquals(0, TaskOutputGrouping.group(listOf(F("a", TaskOutputScope.Task)), F::scope).intermediateCount)
    }

    @Test fun delegation_grouping_folds_each_child_at_its_first_row() {
        data class E(val id: String, val exec: String?)
        val child = TaskDelegationGroup("child-1", "researcher", "completed")
        val entries = listOf(
            E("1", "root"), E("2", "child-1"), E("3", "root"), E("4", "child-1"), E("5", "orphan"), E("6", null),
        )
        val segments = TaskTimeline.groupByDelegation(entries, listOf(child), E::exec)
        assertEquals(5, segments.size)
        assertEquals(TaskTimelineSegment.Row(E("1", "root")), segments[0])
        val block = segments[1] as TaskTimelineSegment.Delegation
        assertEquals(child, block.group)
        assertEquals(listOf("2", "4"), block.entries.map(E::id))
        assertEquals(listOf("3", "5", "6"), segments.drop(2).map { (it as TaskTimelineSegment.Row).entry.id })

        val flat = TaskTimeline.groupByDelegation(entries, emptyList(), E::exec)
        assertEquals(entries.size, flat.size)
        assertTrue(flat.all { it is TaskTimelineSegment.Row })
    }

    @Test fun delegation_span_summarises_clock_range_and_duration() {
        val utc = ZoneOffset.UTC
        fun at(clock: String) = Instant.parse("2026-09-28T${clock}Z").toEpochMilli()
        val done = TaskTimeline.delegationSpan(
            listOf(at("10:02:00"), at("10:07:00")),
            TaskDelegationGroup("c", "a", "completed"),
            utc,
        )
        assertEquals("10:02 – 10:07 (5m)", done.summary)
        assertEquals("5m", done.duration)

        val running = TaskTimeline.delegationSpan(
            listOf(at("10:02:00"), at("10:05:00")),
            TaskDelegationGroup("c", "a", "running"),
            utc,
        )
        assertEquals("started 10:02", running.summary)
        assertNull(running.endClock)

        val fromGroup = TaskTimeline.delegationSpan(
            emptyList(),
            TaskDelegationGroup("c", "a", "failed", startedAt = "2026-09-28T09:00:00Z", completedAt = "2026-09-28T09:30:30Z"),
            utc,
        )
        assertEquals("09:00 – 09:30 (30m 30s)", fromGroup.summary)
        assertNull(TaskTimeline.delegationSpan(listOf(null), TaskDelegationGroup("c", "a", "done"), utc).summary)
    }

    @Test fun execution_duration_needs_both_ends() {
        assertEquals("5m", TaskTimeline.executionDuration("2026-09-28T10:00:00Z", "2026-09-28T10:05:00Z"))
        assertNull(TaskTimeline.executionDuration("2026-09-28T10:00:00Z", null))
        assertNull(TaskTimeline.executionDuration("2026-09-28T10:05:00Z", "2026-09-28T10:00:00Z"))
        assertEquals(TaskTimelineMode.Grouped, TaskTimelineMode.fromWire(null))
        assertEquals(TaskTimelineMode.Chronological, TaskTimelineMode.fromWire("chronological"))
    }
}
