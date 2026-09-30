package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TaskDeepLinksTest {
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
