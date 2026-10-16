package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.ChatMessage
import ai.magicbeans.magdroid.chat.ComposerAsk
import ai.magicbeans.magdroid.chat.ComposerAskDock
import ai.magicbeans.magdroid.chat.EscalationCard
import ai.magicbeans.magdroid.chat.MessageKind
import ai.magicbeans.magdroid.chat.TaskStatusCard
import kotlinx.serialization.json.JsonArray
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Asks answered where they are shown: the composer dock (this conversation's
 * asks) and a task's deep panel (that task's asks), both drawn from the same
 * Attention feed the Attention tab reads.
 */
class AskSurfacesTest {

    private fun row(
        id: String,
        correlation: String = "c-$id",
        thread: String? = null,
        task: String? = null,
        type: String = "request",
        status: String = "needs_action",
    ) = AttentionItem(
        id = id,
        itemType = type,
        taskId = task,
        uiThreadId = thread,
        title = "Question $id",
        status = status,
        updatedAt = id.hashCode().toLong().and(0xffff),
        metadata = AttentionMetadata(
            source = "agentic",
            inputType = "choice",
            hitlRequest = HitlRequestPayload(identifiers = HitlIdentifiers(correlationId = correlation)),
        ),
    )

    private fun feed(vararg requests: AttentionItem, escalations: List<AttentionItem> = emptyList()) =
        AttentionFeedResponse(requests = requests.toList(), escalations = escalations)

    private fun transcriptAsk(id: String, correlation: String) = ChatMessage(
        id = id, fromUser = false, text = "", kind = MessageKind.Escalation,
        escalation = EscalationCard(
            question = "Which one?", options = emptyList(), correlationId = correlation,
            executionId = null, resolved = false, inputType = "choice",
        ),
    )

    private fun taskCard(taskId: String) = ChatMessage(
        id = "m-$taskId", fromUser = false, text = "", kind = MessageKind.TaskStatus,
        task = TaskStatusCard(taskId = taskId, title = "Task", status = "running"),
    )

    // ── Composer dock ───────────────────────────────────────────────────────

    @Test
    fun `the dock takes feed asks for this thread and for tasks the chat started`() {
        val inThread = row("a", thread = "session-1")
        val relentless = row("b", task = "task-9") // a task, no thread
        val elsewhere = row("c", thread = "session-2", task = "task-other")
        val failed = row("d", thread = "session-1", type = "failed", status = "failed")
        val asks = AskSurfaces.forThread(
            feed(inThread, relentless, elsewhere, failed),
            threadIds = setOf("session-1"),
            taskIds = ComposerAskDock.taskIds(listOf(taskCard("task-9"))),
        )
        assertEquals(setOf("a", "b"), asks.map { it.id }.toSet())
    }

    @Test
    fun `a conversation with no thread or task shows no feed asks`() {
        assertTrue(AskSurfaces.forThread(feed(row("a", thread = "x")), emptySet(), emptySet()).isEmpty())
    }

    @Test
    fun `the dock shows a pause once when the transcript and the feed both carry it`() {
        val messages = listOf(transcriptAsk("m1", correlation = "c-shared"))
        val feedAsks = listOf(row("shared", correlation = "c-shared", thread = "s"), row("only-feed", thread = "s"))
        val merged = ComposerAskDock.merged(messages, feedAsks)
        assertEquals(2, merged.size)
        assertTrue(merged[0] is ComposerAsk.Transcript)
        assertEquals("only-feed", (merged[1] as ComposerAsk.Feed).item.id)
        assertEquals(listOf("m1", "feed:c-only-feed"), merged.map { it.key })
    }

    @Test
    fun `an ask answered on another in-place surface leaves the dock at once`() {
        val messages = listOf(transcriptAsk("m1", correlation = "c-1"))
        val merged = ComposerAskDock.merged(messages, listOf(row("x", thread = "s")), settled = setOf("c-1", "c-x"))
        assertTrue(merged.isEmpty())
    }

    @Test
    fun `dock paging works on merged keys`() {
        val keys = listOf("m1", "feed:a", "feed:b")
        assertEquals("m1", ComposerAskDock.activeKey(keys, "gone"))
        assertEquals("feed:b", ComposerAskDock.stepKey(keys, "m1", -1))
        assertEquals("2 of 3", ComposerAskDock.positionOf(keys, "feed:a"))
        assertNull(ComposerAskDock.positionOf(keys.take(1), "m1"))
    }

    // ── Task deep panel ─────────────────────────────────────────────────────

    private val panelRows = attentionJson.parseToJsonElement(
        """
        [
          {"id":"panel-1","item_type":"request","task_id":"task-1","title":"Pick a region","status":"needs_action",
           "hitl_request":{"id":"pause-1","source":"clarification","input_type":"choice","prompt":"Pick a region",
             "input_schema":{"options":[{"id":"eu","label":"EU"},{"id":"us","label":"US"}]},
             "identifiers":{"correlation_id":"pause-1"},"scope":{"task_id":"task-1","execution_id":"exec-1"}}},
          {"id":"panel-status","item_type":"running","task_id":"task-1","title":"Working","status":"running"}
        ]
        """.trimIndent(),
    ) as JsonArray

    @Test
    fun `the deep panel lists and answers a panel-only ask with the Attention contract`() {
        val asks = AskSurfaces.forTask(AttentionFeedResponse(), "task-1", AskSurfaces.panelItems(panelRows))
        val ask = asks.single()
        val request = ask.request(ask.metadata)
        assertEquals("pause-1", request.correlationId)
        assertEquals("clarification", request.source)
        assertEquals("choice", request.inputType)
        assertEquals(listOf("eu", "us"), request.options.map { it.id })
        assertEquals("exec-1", request.executionId)
        assertEquals("task-1", ask.openTaskId)
    }

    @Test
    fun `the deep panel lists a feed-only ask for its task and merges duplicates`() {
        val feedOnly = row("relentless", task = "task-1")
        val duplicate = row("feed-copy", correlation = "pause-1", task = "task-1")
        val otherTask = row("other", task = "task-2")
        val asks = AskSurfaces.forTask(feed(feedOnly, duplicate, otherTask), "task-1", AskSurfaces.panelItems(panelRows))
        assertEquals(listOf("feed-copy", "relentless").sorted(), asks.map { it.id }.sorted())
    }

    @Test
    fun `an answered ask leaves the deep panel before the panel is re-read`() {
        val asks = AskSurfaces.forTask(AttentionFeedResponse(), "task-1", AskSurfaces.panelItems(panelRows), settled = setOf("pause-1"))
        assertTrue(asks.isEmpty())
    }

    // ── Relentless review ───────────────────────────────────────────────────

    private val relentless = """{"requests":[{"id":"exec-1:relentless-review:run-7:abc","question":"A draft passed its checks and needs your review.",
        "request_type":"relentless.accepted_review","task_id":"task-1",
        "options":[{"id":"approve","label":"Approve draft"},{"id":"reject","label":"Reject draft"}],
        "context":{"run_id":"run-7","task_id":"task-1","review_href":"/work?view=quality&run=run-7&task=task-1","review_label":"Review draft"}}]}"""

    @Test
    fun `a Relentless review carries its Work Quality link and an approve or reject decision`() {
        val pending = attentionJson.decodeFromString(PendingUserRequests.serializer(), relentless)
        val feed = AttentionFeedResponse().withPendingUserRequests(pending.requests)
        val item = AskSurfaces.forTask(feed, "task-1", emptyList()).single()
        val request = item.request(item.metadata)
        assertEquals("/work?view=quality&run=run-7&task=task-1", request.reviewHref)
        assertEquals("Review draft", request.reviewLabel)
        assertEquals("relentless.accepted_review", request.requestType)
        assertEquals("choice", request.inputType)
        val (reject, approve) = request.decisionPair!!
        assertEquals("reject" to "Reject draft", reject.id to reject.label)
        assertEquals("approve" to "Approve draft", approve.id to approve.label)
        // And it reaches the chat that started the task, though it names no thread.
        assertEquals(1, AskSurfaces.forThread(feed, setOf("s"), setOf("task-1")).size)
    }

    @Test
    fun `a feed row reads the review link from input_schema context`() {
        val json = """{"id":"f1","item_type":"request","task_id":"task-1","title":"Review","status":"needs_action",
            "metadata":{"source":"user_request","input_type":"choice",
              "input_schema":{"type":"choice","request_type":"relentless.accepted_review",
                "options":[{"id":"approve","label":"Approve draft"},{"id":"reject","label":"Reject draft"}],
                "context":{"review_href":"/work?view=quality&run=r&task=task-1","review_label":"Review draft"}}}}"""
        val item = attentionJson.decodeFromString(AttentionItem.serializer(), json)
        val request = item.request(item.metadata)
        assertEquals("/work?view=quality&run=r&task=task-1", request.reviewHref)
        assertEquals("Review draft", request.reviewLabel)
        assertTrue(request.decisionPair != null)
    }

    @Test
    fun `a sentence context does not break the decode and gives no link`() {
        val json = """{"id":"g1","item_type":"approval","title":"Allow?","status":"needs_action",
            "metadata":{"input_schema":{"type":"tool_authorization","context":"The tool wants network access"}}}"""
        val item = attentionJson.decodeFromString(AttentionItem.serializer(), json)
        assertNull(item.request(item.metadata).reviewHref)
    }

    @Test
    fun `only an approve and reject pair is drawn as a decision`() {
        val dismissOnly = AttentionItem(id = "x", title = "Q").request(
            AttentionMetadata(inputType = "choice", options = listOf(ai.magicbeans.magdroid.chat.EscalationOption("dismiss", "Dismiss"))),
        )
        assertNull(dismissOnly.decisionPair)
    }

    // ── Sheet hand-off ──────────────────────────────────────────────────────

    @Test
    fun `secrets and grants go to the secure sheet, forms only from the dock`() {
        fun req(type: String, schema: AttentionInputSchema? = null) =
            AttentionItem(id = "x", title = "Q").request(AttentionMetadata(inputType = type, inputSchema = schema))
        assertTrue(AskSurfaces.answersInSheet(req("password"), compact = false))
        assertTrue(AskSurfaces.answersInSheet(req("tool_authorization"), compact = false))
        assertTrue(AskSurfaces.answersInSheet(req("form"), compact = true))
        assertFalse(AskSurfaces.answersInSheet(req("form"), compact = false))
        assertFalse(AskSurfaces.answersInSheet(req("choice"), compact = true))
        assertFalse(AskSurfaces.answersInSheet(req("confirmation"), compact = true))
    }
}
