package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TaskDeepLinksTest {
    @org.junit.Test fun customTaskScheduleKeepsExactCronTimezoneAndRetention() {
        val schedule = taskSchedule("custom", "12", "30", "0 6 * * 1", "Asia/Kolkata")!!
        org.junit.Assert.assertEquals("Asia/Kolkata", (schedule["timezone"] as kotlinx.serialization.json.JsonPrimitive).content)
        org.junit.Assert.assertTrue(schedule.toString().contains("0 6 * * 1"))
        org.junit.Assert.assertTrue(schedule.toString().contains("max_records"))
    }

    @Test fun canonicalWorkRootAndNeedsYouRemainNativeTargets() {
        assertEquals(WorkLinkTarget.Library(ai.magicbeans.magdroid.work.WorkSection.All), WorkDeepLinks.parse("/work"))
        assertEquals(WorkLinkTarget.Library(ai.magicbeans.magdroid.work.WorkSection.NeedsYou), WorkDeepLinks.parse("/work?view=needs-you"))
    }
    @Test fun work_library_targets_preserve_encoded_identity_and_goal() {
        assertEquals(WorkLinkTarget.Library(ai.magicbeans.magdroid.work.WorkSection.Quality,
            WorkRoute("quality-campaign", "capture/one", "Capture policy")), WorkDeepLinks.parse("/work?view=quality&campaign=capture%2Fone"))
        assertEquals(WorkLinkTarget.Library(ai.magicbeans.magdroid.work.WorkSection.Crew,
            WorkRoute("memory-tier", "member/one", "Memory tier", goalId = "goal/one", tier = "goal/tier")),
            WorkDeepLinks.parse("/work/crew/member%2Fone/memory/goal%2Ftier?goal_id=goal%2Fone"))
        assertEquals(WorkLinkTarget.Library(ai.magicbeans.magdroid.work.WorkSection.Crew,
            WorkRoute("member-edit", "writer", "Edit member")), WorkDeepLinks.parse("/crew/new?edit=writer"))
        assertEquals(WorkLinkTarget.Thread("launch"), WorkDeepLinks.parse("/t/launch/tasks"))
        assertEquals(WorkLinkTarget.Library(ai.magicbeans.magdroid.work.WorkSection.Quality,
            WorkRoute("quality-run", "run/one", "Quality run")), WorkDeepLinks.parse("magican://work?view=quality&run=run%2Fone"))
        assertNull(WorkDeepLinks.parse("javascript:/work?view=crew"))
    }

    @Test fun work_task_links_keep_execution_and_internal_thread_context() {
        assertEquals(TaskDeepLinkTarget.Task("task/one", ai.magicbeans.magdroid.tasks.TaskDetailAct.Plan,
            executionId = "run/two", internalWork = true, threadId = "launch"),
            TaskDeepLinks.parse("/work?view=running&type=internal&selected=task%2Fone&execution_id=run%2Ftwo&plan=open&thread=launch"))
        assertNull(TaskDeepLinks.parse("/work?view=quality&selected=task-one"))
    }

    @Test fun task_link_targets_shared_task_detail() {
        assertEquals(TaskDeepLinkTarget.Task("task-1"), TaskDeepLinks.parse("magican://task/task-1"))
    }

    @Test fun monitor_link_preserves_update_highlight() {
        assertEquals(
            TaskDeepLinkTarget.Monitor("monitor-1", "update-2"),
            TaskDeepLinks.parse("magican://monitor/monitor-1?update=update-2"),
        )
    }

    @Test fun canonical_tasks_route_maps_monitor_mode() {
        assertEquals(
            TaskDeepLinkTarget.Monitor("monitor-1", "update-2"),
            TaskDeepLinks.parse("https://magican.example/tasks?type=monitors&selected=monitor-1&update=update-2"),
        )
    }

    @Test fun work_links_preserve_monitor_update_and_plan_focus() {
        assertEquals(TaskDeepLinkTarget.Monitor("monitor-1", "update/2"),
            TaskDeepLinks.parse("https://magican.example/work?type=monitors&selected=monitor-1&update=update%2F2"))
        assertEquals(TaskDeepLinkTarget.Task("task-1", ai.magicbeans.magdroid.tasks.TaskDetailAct.Plan),
            TaskDeepLinks.parse("/work?selected=task-1&plan=open"))
        assertNull(TaskDeepLinks.parse("/other/work?selected=task-1"))
    }

    @Test fun encoded_identifiers_are_decoded_once() {
        assertEquals(
            TaskDeepLinkTarget.Task("task/with space"),
            TaskDeepLinks.parse("magican://task/task%2Fwith%20space"),
        )
        assertEquals(
            TaskDeepLinkTarget.Monitor("monitor id", "update/2"),
            TaskDeepLinks.parse("magican://monitor/monitor%20id?update=update%2F2"),
        )
    }

    @Test fun unrelated_and_incomplete_links_are_ignored() {
        assertNull(TaskDeepLinks.parse("magican://chat/session-1"))
        assertNull(TaskDeepLinks.parse("magican://task/"))
        assertNull(TaskDeepLinks.parse("https://magican.example/tasks"))
    }

    @Test fun today_preserves_the_exact_attention_selection_for_the_target_surface() {
        AttentionDeepLinks.request("approval/one")
        assertEquals("approval/one", AttentionDeepLinks.itemId.value)
        AttentionDeepLinks.consume("another")
        assertEquals("approval/one", AttentionDeepLinks.itemId.value)
        AttentionDeepLinks.consume("approval/one")
        assertNull(AttentionDeepLinks.itemId.value)
    }

    @Test fun task_artifact_links_are_credential_free_encoded_and_traversal_safe() {
        assertEquals(
            "https://magican.example/api/magician/v3/tasks/task%201/outputs/draft/report%20final.pdf",
            TaskArtifactLinks.url("https://magican.example/", "task 1", "/outputs/./draft/../report final.pdf"),
        )
        assertNull(TaskArtifactLinks.url("", "task-1", "report.pdf"))
        assertNull(TaskArtifactLinks.url("https://magican.example", "task-1", "../"))
    }
}
