package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.TaskDetailRules
import ai.magicbeans.magdroid.tasks.TaskOutputFile
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CopyFeedbackRulesTest {
    @Test fun id_target_copies_the_full_id_not_the_short_one() {
        val full = "task_01JABCDEFGHJKMNPQRSTVWXYZ"
        val target = CopyTarget.id(full)
        assertEquals(full, target.value)
        assertEquals("task id", target.what)
        assertEquals("Copy full id", target.clickLabel)
        assertTrue(TaskDetailRules.shortId(full).endsWith("…"))
    }

    @Test fun run_id_target_names_itself() {
        val target = CopyTarget.id(" exec_0123456789abcdef ", "run id")
        assertEquals("exec_0123456789abcdef", target.value)
        assertEquals("run id", target.what)
    }

    @Test fun path_target_prefers_the_absolute_path() {
        val target = CopyTarget.path("/Users/me/MagicianNotes/tasks/t1/report.md", "outputs/report.md")
        assertEquals("/Users/me/MagicianNotes/tasks/t1/report.md", target.value)
        assertEquals("path", target.what)
        assertEquals("Copy path", target.clickLabel)
    }

    @Test fun path_target_falls_back_to_the_relative_path() {
        assertEquals("outputs/report.md", CopyTarget.path(null, "outputs/report.md").value)
        assertEquals("outputs/report.md", CopyTarget.path("  ", "outputs/report.md").value)
        assertEquals("outputs/report.md", CopyTarget.path("relative/only.md", "outputs/report.md").value)
    }

    @Test fun path_target_agrees_with_the_detail_rules_for_a_file() {
        val file = TaskOutputFile(id = "f", relativePath = "a/b.md", absolutePath = "/root/a/b.md")
        assertEquals(TaskDetailRules.copyPath(file), CopyTarget.path(file.absolutePath, file.relativePath).value)
        val bare = TaskOutputFile(id = "g", relativePath = "a/c.md")
        assertEquals(TaskDetailRules.copyPath(bare), CopyTarget.path(bare.absolutePath, bare.relativePath).value)
    }

    @Test fun labels_and_toast_name_what_was_copied() {
        assertEquals("Magican task id", CopyFeedbackRules.clipLabel("task id"))
        assertEquals("Magican text", CopyFeedbackRules.clipLabel(" "))
        assertEquals("Copied path", CopyFeedbackRules.toastText("path"))
        assertEquals("Copied ✓", CopyFeedbackRules.label("task_01J…", copied = true))
        assertEquals("task_01J…", CopyFeedbackRules.label("task_01J…", copied = false))
        assertEquals("Copied", CopyFeedbackRules.label("task_01J…", copied = true, copiedLabel = "Copied"))
    }

    @Test fun toast_only_below_api_33_where_the_system_shows_nothing() {
        assertTrue(CopyFeedbackRules.showsToast(32))
        assertTrue(CopyFeedbackRules.showsToast(24))
        assertFalse(CopyFeedbackRules.showsToast(33))
        assertFalse(CopyFeedbackRules.showsToast(34))
    }

    @Test fun flash_is_static_when_animations_are_off() {
        assertFalse(CopyFeedbackRules.crossfades(0f))
        assertTrue(CopyFeedbackRules.crossfades(1f))
        assertEquals(1_200L, CopyFeedbackRules.FLASH_MILLIS)
    }
}
