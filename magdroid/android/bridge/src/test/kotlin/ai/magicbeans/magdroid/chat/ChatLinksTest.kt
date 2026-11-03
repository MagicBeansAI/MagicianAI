package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The chat link rule set (web `classifyRef`) for every link a reply can carry. */
class ChatLinksTest {
    private val task = "task_0123456789abcdef0123456789abcdef"

    @Test fun `file urls and absolute paths are host files`() {
        assertEquals(ChatLinkTarget.HostFile("/Volumes/SSD/My Report.pdf"), ChatLinks.classify("file:///Volumes/SSD/My%20Report.pdf"))
        assertEquals(ChatLinkTarget.HostFile("/Volumes/SSD/notes/plan.md"), ChatLinks.classify("/Volumes/SSD/notes/plan.md"))
        assertEquals(ChatLinkTarget.HostFile("Pages/Chapter-Map.md"), ChatLinks.classify("Pages/Chapter-Map.md"))
        // A task workdir file that is not an output opens on the host too.
        val workdir = "/Users/me/magician_data_v3/tasks/$task/workdir/main.py"
        assertEquals(ChatLinkTarget.HostFile(workdir), ChatLinks.classify(workdir))
    }

    @Test fun `a path inside a task's outputs is that task's output however it is spelled`() {
        val output = ChatLinkTarget.TaskOutput(task, "reports/summary.pdf")
        assertEquals(output, ChatLinks.classify("/Users/me/magician_data_v3/tasks/$task/workdir/outputs/reports/summary.pdf"))
        assertEquals(output, ChatLinks.classify("file:///Users/me/magician_data_v3/tasks/$task/outputs/reports/summary.pdf"))
        assertEquals(output, ChatLinks.classify("/api/magician/v3/tasks/$task/outputs/reports/summary.pdf"))
        assertEquals("/api/magician/v3/tasks/$task/outputs/reports/summary.pdf", output.apiPath)

        val spaced = ChatLinks.classify("/api/magician/v3/tasks/$task/outputs/My%20File.pdf") as ChatLinkTarget.TaskOutput
        assertEquals("My File.pdf", spaced.relativePath)
        assertEquals("/api/magician/v3/tasks/$task/outputs/My%20File.pdf", spaced.apiPath)

        assertEquals(
            ChatLinkTarget.TaskOutput(task, "executions/exec_1/outputs/a.txt"),
            ChatLinks.classify("/x/tasks/$task/executions/exec_1/outputs/a.txt"),
        )
    }

    @Test fun `relative api references are server resources`() {
        val path = "/api/magician/v2/chat/sessions/s1/outputs/a.md"
        assertEquals(ChatLinkTarget.ServerResource(path), ChatLinks.classify(path))
    }

    @Test fun `app routes stay app routes`() {
        assertEquals(ChatLinkTarget.AppRoute("/t/general"), ChatLinks.classify("/t/general"))
        assertEquals(ChatLinkTarget.AppRoute("/work?filter=all&selected=$task"), ChatLinks.classify("/work?filter=all&selected=$task"))
        assertEquals(ChatLinkTarget.AppRoute("/work?filter=all&selected=$task"), ChatLinks.classify(task))
        assertEquals(ChatLinkTarget.AppRoute("/chat"), ChatLinks.classify("/chat"))
        assertEquals(ChatLinkTarget.AppRoute("/tasks/$task"), ChatLinks.classify("/tasks/$task"))
    }

    @Test fun `every web chat url form opens the conversation in the app`() {
        assertEquals(ChatAppDestination.Chat(null, null), ChatLinks.appDestination("/chat"))
        assertEquals(ChatAppDestination.Chat("chat-1", null), ChatLinks.appDestination("/chat?session=chat-1"))
        assertEquals(ChatAppDestination.Chat("chat-1", "work"), ChatLinks.appDestination("/chat?thread=work&session=chat-1"))
        assertEquals(ChatAppDestination.Chat(null, "work"), ChatLinks.appDestination("/t/work"))
        assertEquals(ChatAppDestination.Chat(null, "work"), ChatLinks.appDestination("/t/work/chat"))
        assertEquals(ChatAppDestination.Chat("chat-1", "work"), ChatLinks.appDestination("/t/work/chat?session=chat-1"))
        assertEquals(ChatAppDestination.Chat("chat-1", "work"), ChatLinks.appDestination("/t/work/files?session=chat-1"))
        assertEquals(ChatAppDestination.Chat(null, "my thread"), ChatLinks.appDestination("/t/my%20thread"))
        assertEquals(ChatAppDestination.History, ChatLinks.appDestination("/history"))
    }

    @Test fun `a thread route's slug key is swapped for the canonical id`() {
        assertEquals("launch plan", ChatLinks.threadRouteKey("/t/launch%20plan/chat?session=s1"))
        assertNull(ChatLinks.threadRouteKey("/work?selected=$task"))
        assertEquals("/t/thr_1/tasks/$task", ChatLinks.withThreadId("/t/launch-plan/tasks/$task", "thr_1"))
        assertEquals("/t/thr_1?selected=$task", ChatLinks.withThreadId("/t/launch-plan?selected=$task", "thr_1"))
        assertEquals("/t/thr_1", ChatLinks.withThreadId("/t/launch-plan", "thr_1"))
    }

    @Test fun `app routes land on their native screens and only unknown ones on the web`() {
        assertEquals(ChatAppDestination.Work("/t/work/tasks/$task"), ChatLinks.appDestination("/t/work?selected=$task"))
        assertEquals(ChatAppDestination.Work("/t/work/tasks/$task"), ChatLinks.appDestination("/t/work/tasks/$task"))
        assertEquals(ChatAppDestination.Work("/work?selected=$task&type=monitors"), ChatLinks.appDestination("/work?selected=$task&type=monitors"))
        assertEquals(ChatAppDestination.Work("/tasks/$task"), ChatLinks.appDestination("/tasks/$task"))
        assertEquals(ChatAppDestination.Work("/crew/agent-1"), ChatLinks.appDestination("/crew/agent-1"))
        assertEquals(ChatAppDestination.Attention("a1"), ChatLinks.appDestination("/attention?item_id=a1"))
        assertEquals(ChatAppDestination.Today, ChatLinks.appDestination("/today?tab=followups"))
        assertEquals(ChatAppDestination.Observe("/observe?pane=now"), ChatLinks.appDestination("/observe?pane=now"))
        assertEquals(ChatAppDestination.Settings, ChatLinks.appDestination("/settings/model-routing"))
        assertEquals(ChatAppDestination.Notes, ChatLinks.appDestination("/notes"))
        assertEquals(ChatAppDestination.VibeDev, ChatLinks.appDestination("/vibe"))
        assertEquals(ChatAppDestination.VibeDev, ChatLinks.appDestination("/t/vibedev"))
        assertEquals(ChatAppDestination.Web("/briefing/today"), ChatLinks.appDestination("/briefing/today"))
        assertEquals(ChatAppDestination.Web("/debug?execution_id=e1"), ChatLinks.appDestination("/debug?execution_id=e1"))
    }

    @Test fun `an absolute link on the paired machine is read by its path`() {
        val origin = "https://mac.example.test"
        assertEquals(
            ChatLinkTarget.TaskOutput(task, "a.png"),
            ChatLinks.classify("https://mac.example.test/api/magician/v3/tasks/$task/outputs/a.png", origin),
        )
        assertEquals(ChatLinkTarget.AppRoute("/t/general"), ChatLinks.classify("https://mac.example.test/t/general", origin))
        assertEquals(ChatLinkTarget.External("https://example.com/a.pdf"), ChatLinks.classify("https://example.com/a.pdf", origin))
    }

    @Test fun `https mailto and tel are external`() {
        assertEquals(ChatLinkTarget.External("https://example.com/report"), ChatLinks.classify("https://example.com/report"))
        assertEquals(ChatLinkTarget.External("mailto:owner@example.com"), ChatLinks.classify("mailto:owner@example.com"))
        assertEquals(ChatLinkTarget.External("tel:+15551234"), ChatLinks.classify("tel:+15551234"))
    }

    @Test fun `unknown schemes escapes and prose are not opened`() {
        listOf("javascript:alert(1)", "//evil.test/a.md", "data:text/html,hi", "", "just words").forEach {
            assertTrue(it, ChatLinks.classify(it) is ChatLinkTarget.Unknown)
        }
    }

    @Test fun `structured artifact rows name what they open`() {
        assertEquals("/api/magician/v3/tasks/$task/outputs/a%20b.pdf", ChatLinks.artifactItemLink(mapOf("task_id" to task, "relative_path" to "outputs/a b.pdf")))
        assertEquals("/Volumes/x/a.pdf", ChatLinks.artifactItemLink(mapOf("absolute_path" to "/Volumes/x/a.pdf", "relative_path" to "a.pdf")))
        assertEquals("/api/magician/v3/tasks/$task/outputs/r.md", ChatLinks.artifactItemLink(mapOf("artifact_id" to "magician-artifact:task:$task:r.md")))
        assertNull(ChatLinks.artifactItemLink(mapOf("label" to "Report")))
    }

    @Test fun `mentioned paths become links while code and links stay as written`() {
        assertEquals(
            "Saved to [/Volumes/SSD/out/report.pdf](/Volumes/SSD/out/report.pdf).",
            ChatLinks.linkifyMarkdown("Saved to /Volumes/SSD/out/report.pdf."),
        )
        assertEquals(
            "See [`file:///tmp/My Report.pdf`](file:///tmp/My%20Report.pdf)",
            ChatLinks.linkifyMarkdown("See `file:///tmp/My Report.pdf`"),
        )
        assertEquals("Run `/usr/bin/env` now", ChatLinks.linkifyMarkdown("Run `/usr/bin/env` now"))
        listOf(
            "[report](/tmp/a/report.pdf)",
            "<https://example.com/a/b.pdf>",
            "https://example.com/a/b.pdf",
            "and/or/x.md",
            "```\n/tmp/a/b.png\n```",
        ).forEach { assertEquals(it, it, ChatLinks.linkifyMarkdown(it)) }
        assertFalse(ChatLinks.isLinkableCode("origin/main"))
        assertTrue(ChatLinks.isLinkableCode("Pages/Chapter-Map.md"))
    }
}
