package ai.magicbeans.magdroid.tasks

import ai.magicbeans.magdroid.vibedev.VibeDevRunState
import ai.magicbeans.magdroid.vibedev.VibeDevRunsPage
import ai.magicbeans.magdroid.vibedev.VibeDevRunTurn
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** `completion_kind` / `open_items` on V3 rows: a completed + partial task reads "Partly done", never "Completed". */
class TaskCompletionTest {
    private fun row(extra: String, status: String = "completed"): TaskV3 =
        taskJson.decodeFromString(TaskV3.serializer(), """{"id":"t1","title":"Ship","status":"$status"$extra}""")

    @Test fun decodes_a_partial_row_with_its_open_items() {
        val task = row(""","completion_kind":"partial","open_items":["Changelog entry not written","Release tag"]""")
        assertEquals("partial", task.completionKind)
        assertEquals(listOf("Changelog entry not written", "Release tag"), task.openItems)
        assertTrue(task.isPartlyDone)
        assertEquals("Partly done", task.statusLabel)
        assertEquals(TaskStatusTone.Warning, task.statusTone)
        assertEquals("Open: Changelog entry not written · +1 more", task.openItemsLine)
    }

    @Test fun full_or_absent_kind_stays_completed() {
        for (task in listOf(row(""","completion_kind":"full","open_items":[]"""), row(""))) {
            assertFalse(task.isPartlyDone)
            assertEquals("Completed", task.statusLabel)
            assertEquals(TaskStatusTone.Success, task.statusTone)
            assertNull(task.openItemsLine)
        }
    }

    @Test fun unknown_or_malformed_values_are_ignored() {
        val odd = row(""","completion_kind":"mostly","open_items":"not a list"""")
        assertNull(odd.completionKind)
        assertEquals(emptyList<String>(), odd.openItems)
        assertEquals("Completed", odd.statusLabel)
        val shapes = row(""","completion_kind":7,"open_items":[1,null,"  Real item  ",{"x":1},""]""")
        assertNull(shapes.completionKind)
        assertEquals(listOf("Real item"), shapes.openItems)
        assertEquals("Completed", row(""","completion_kind":null,"open_items":null""").statusLabel)
    }

    @Test fun partial_kind_on_a_task_that_is_not_completed_changes_nothing() {
        val running = row(""","completion_kind":"partial","open_items":["x"]""", status = "running")
        assertFalse(running.isPartlyDone)
        assertEquals("Running", running.statusLabel)
        assertEquals(TaskStatusTone.Active, running.statusTone)
        assertNull(running.openItemsLine)
    }

    @Test fun open_line_is_one_truncated_line() {
        val long = "word ".repeat(60) + "\nend"
        val line = TaskCompletion.openLine(listOf(long))!!
        assertTrue(line.startsWith("Open: word word"))
        assertTrue(line.endsWith("…"))
        assertFalse(line.contains('\n'))
        assertTrue(line.length <= "Open: ".length + 120)
        assertNull(TaskCompletion.openLine(emptyList()))
    }

    @Test fun detail_verdict_reads_partly_done_in_the_attention_severity() {
        val partial = TaskVerdict.deriveForTaskStatus(TaskVerdictInput(
            status = "completed", completionKind = "partial", openItems = listOf("Deploy blocked by missing key"),
        ))
        assertEquals(TaskVerdictState.PartlyDone, partial.state)
        assertEquals("Partly done", partial.headline)
        assertEquals("Open: Deploy blocked by missing key", partial.detail)
        assertEquals(TaskVerdictSeverity.Attention, partial.severity)
        assertEquals(TaskVerdictState.Finished, TaskVerdict.deriveForTaskStatus(TaskVerdictInput(status = "completed", completionKind = "full")).state)
        assertEquals(TaskVerdictState.Finished, TaskVerdict.deriveForTaskStatus(TaskVerdictInput(status = "completed")).state)
        assertEquals(TaskVerdictState.Running, TaskVerdict.deriveForTaskStatus(TaskVerdictInput(status = "running", completionKind = "partial")).state)
    }

    @Test fun vibedev_run_rows_carry_partly_done_from_the_embedded_task_rows() {
        val json = Json { ignoreUnknownKeys = true; isLenient = true; explicitNulls = false }
        val page = json.decodeFromString(VibeDevRunsPage.serializer(), """
            {"tasks":[
              {"id":"a","title":"VibeDev · Add login","status":"completed","completion_kind":"partial","open_items":["Tests still failing"]},
              {"id":"b","title":"VibeDev · Fix typo","status":"completed","completion_kind":"full"},
              {"id":"c","title":"VibeDev · Odd","status":"completed","completion_kind":["partial"],"open_items":7}
            ]}
        """.trimIndent())
        val (partly, full, odd) = page.runRows()
        assertEquals(VibeDevRunState.Done, partly.state)
        assertTrue(partly.isPartlyDone)
        assertEquals("Partly done", partly.statusLabel)
        assertEquals("Open: Tests still failing", partly.openItemsLine)
        assertEquals("Done", full.statusLabel)
        assertNull(full.openItemsLine)
        assertEquals("Done", odd.statusLabel)

        val turn = json.decodeFromString(VibeDevRunTurn.serializer(),
            """{"task_id":"t","status":"completed","completion_kind":"partial","open_items":["Push rejected"]}""")
        assertEquals("Partly done", turn.statusLabel)
        assertEquals("Done", json.decodeFromString(VibeDevRunTurn.serializer(), """{"task_id":"t","status":"completed"}""").statusLabel)

        val live = json.decodeFromString(ai.magicbeans.magdroid.vibedev.VibeDevLiveRun.serializer(),
            """{"run_id":"r","live_task_id":"t","project_id":"p","state":"done","status":"completed","completion_kind":"partial","open_items":["Merge conflict"]}""")
        assertTrue(live.isPartlyDone)
        assertEquals("Partly done", live.statusLabel)
        assertEquals("Open: Merge conflict", live.openItemsLine)
        val runData = ai.magicbeans.magdroid.vibedev.VibeDevRunData(
            turns = ai.magicbeans.magdroid.vibedev.VibeDevRunTurns("r", turns = listOf(VibeDevRunTurn("t", status = "completed"))),
            live = live,
        )
        assertEquals("Partly done", runData.statusLabel)
        assertEquals("Open: Merge conflict", runData.openItemsLine)
        val working = json.decodeFromString(ai.magicbeans.magdroid.vibedev.VibeDevLiveRun.serializer(),
            """{"run_id":"r","live_task_id":"t","project_id":"p","state":"working","completion_kind":"partial"}""")
        assertFalse(working.isPartlyDone)
    }
}
