package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Deliverables and the other task files: the iOS `TaskParityRulesTests`
 * cases added with `TaskOutputClassification` (web `taskPanelModel.test.ts`
 * `isDeliverableRef` / `outputFilesFrom` / `sidecarFilesFrom`).
 */
class TaskOutputClassificationTest {
    private fun json(text: String): JsonObject = Json.parseToJsonElement(text).jsonObject

    private fun groups(files: List<TaskOutputFile>) =
        TaskOutputGrouping.group(files, TaskOutputFile::scope, TaskOutputFile::deliverable)

    @Test fun output_grouping_prefers_the_endpoints_deliverable_mark_and_keeps_the_absolute_path() {
        val files = TaskOutputClassification.taskOutputFiles(json("""
            {"outputs": {
              "outputs": [
                {"relative_path": "outputs/report.md", "media_type": "text/markdown",
                 "audience": "agent", "role": "continuation_context",
                 "deliverable": true, "absolute_path": "/srv/t/outputs/report.md"},
                {"relative_path": "outputs/state.json", "audience": "user", "role": "primary_task_user",
                 "deliverable": false, "absolute_path": "relative/not/absolute.json"}
              ],
              "internal_files": [
                {"kind": "task_summary", "file_name": "task_summary.json",
                 "absolute_path": "/srv/t/outputs/task_summary.json"},
                {"kind": "voice_speech", "file_name": "voice_speech.json", "absolute_path": null},
                {"kind": "bad", "file_name": "../escape.json"},
                {"kind": "blank", "file_name": ""}
              ]
            }}
        """))
        val groups = groups(files)
        // The endpoint's mark wins over the role rule.
        assertEquals(listOf("outputs/report.md"), groups.deliverables.map { it.relativePath })
        assertEquals("/srv/t/outputs/report.md", groups.deliverables.first().absolutePath)
        assertEquals(listOf(TaskOutputScope.Internal), groups.intermediates.map { it.scope })
        val technical = groups.intermediates[0].files
        assertEquals(
            listOf("outputs/state.json", "outputs/task_summary.json", "outputs/voice_speech.json"),
            technical.map { it.relativePath },
        )
        assertEquals(listOf(false, false, false), technical.map { it.deliverable })
        // Only an absolute path is one: anything else could not be resolved by another tool.
        assertEquals(listOf(null, "/srv/t/outputs/task_summary.json", null), technical.map { it.absolutePath })
        assertEquals(listOf("Primary task user", "Task summary", "Voice speech"), technical.map { it.detail })
        assertEquals(3, groups.intermediateCount)
        assertEquals("Other task files", TaskOutputScope.Internal.title)
    }

    @Test fun keeps_the_endpoints_own_deliverable_mark_when_it_sends_one() {
        assertTrue(TaskOutputClassification.isDeliverable(true, "agent", "continuation_context"))
        assertFalse(TaskOutputClassification.isDeliverable(false, "user", "primary_task_user"))
    }

    @Test fun derives_the_mark_by_the_export_rule_when_an_older_server_omits_it() {
        val rule = TaskOutputClassification::isDeliverable
        assertTrue(rule(null, "user", "primary_task_user"))
        assertTrue(rule(null, "user", "user_media"))
        assertFalse(rule(null, "agent", "continuation_context"))
        // The legacy agent-facing duplicate of the user output.
        assertFalse(rule(null, "agent", "primary_task_agent"))
        assertFalse(rule(null, "user", "execution_output"))
        // No role says too little to hide a file, unless it is agent-facing.
        assertTrue(rule(null, "user", null))
        assertTrue(rule(null, null, null))
        assertFalse(rule(null, "agent", null))
    }

    @Test fun marks_a_non_deliverable_task_row_and_says_what_it_is_in_the_old_shape() {
        val files = TaskOutputClassification.taskOutputFiles(json("""
            {"outputs": {"outputs": [
              {"audience": "user", "role": "primary_task_user", "relative_path": "outputs/out_task_user_1.html",
               "media_type": "text/html"},
              {"audience": "agent", "role": "primary_task_agent", "relative_path": "outputs/out_task_agent_1.md",
               "media_type": "text/markdown"},
              {"audience": "agent", "role": "continuation_context",
               "relative_path": "outputs/out_task_continuation_1.json", "media_type": "application/json"}
            ]}}
        """))
        val groups = groups(files)
        assertEquals(listOf("out_task_user_1.html"), groups.deliverables.map { it.displayName })
        assertNull(groups.deliverables.first().detail)
        assertEquals(listOf("out_task_agent_1.md", "out_task_continuation_1.json"),
            groups.intermediates.first().files.map { it.displayName })
        assertEquals(listOf("Agent copy (legacy)", "Continuation context"),
            groups.intermediates.first().files.map { it.detail })
    }

    @Test fun never_splits_run_rows_by_the_task_rule() {
        val run = TaskOutputClassification.files(
            json("""{"rows": [{"relative_path": "executions/e1/outputs/x.md", "audience": "agent", "role": "execution_output"}]}""")
                .get("rows").jObjects().orEmpty(),
            TaskOutputScope.Execution,
        )
        val groups = groups(run)
        assertEquals(listOf(true), groups.intermediates.first { it.scope == TaskOutputScope.Execution }.files.map { it.deliverable })
        assertNull(groups.intermediates.firstOrNull { it.scope == TaskOutputScope.Internal })
    }

    @Test fun absolute_path_accepts_only_host_paths_and_sidecar_names_only_basenames() {
        assertEquals("/a/b", TaskOutputClassification.absolutePath("  /a/b "))
        assertEquals("C:\\x", TaskOutputClassification.absolutePath("C:\\x"))
        assertNull(TaskOutputClassification.absolutePath("a/b"))
        assertNull(TaskOutputClassification.absolutePath(null))
        assertEquals("a.json", TaskOutputClassification.sidecarName("a.json"))
        for (bad in listOf("", "..", ".", "a/b", "a\\b", null)) assertNull(TaskOutputClassification.sidecarName(bad))
    }
}
