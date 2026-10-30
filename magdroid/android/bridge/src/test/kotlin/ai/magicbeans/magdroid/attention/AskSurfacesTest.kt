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
 * Asks answered where they are shown: the composer dock (every open ask, this
 * conversation's first) and a task's deep panel (that task's asks), both drawn
 * from the same Attention feed the Attention tab reads.
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
    fun `nested canonical scope routes a taskless question into its composer`() {
        for (key in listOf("chat_session_id", "session_id", "thread_id", "ui_thread_id")) {
            val data = attentionJson.decodeFromString(AttentionFeedResponse.serializer(), """
                {"requests":[{"id":"scoped","item_type":"request","title":"Which region?","status":"needs_action",
                  "metadata":{"hitl_request":{"id":"q-scoped","input_type":"text","scope":{"$key":"chat-1"}}}}]}
            """.trimIndent())
            assertEquals(key, listOf("scoped"), AskSurfaces.forThread(data, setOf("chat-1"), emptySet()).map { it.id })
            assertTrue(AskSurfaces.forThread(data, setOf("chat-2"), emptySet()).isEmpty())
        }
    }

    @Test
    fun `a scoped child question brings its task-only sibling into the composer`() {
        val child = row("child", task = "child-task").let { item -> item.copy(metadata = item.metadata!!.copy(
            hitlRequest = item.metadata.hitlRequest!!.copy(scope = HitlRequestScope(threadId = "chat-1")),
        )) }
        val data = feed(child, row("sibling", task = "child-task"), row("elsewhere", task = "other-task"))
        assertEquals(setOf("child", "sibling"), AskSurfaces.forThread(data, setOf("chat-1"), emptySet()).map { it.id }.toSet())
    }

    @Test
    fun `VibeDev lists every question with its turns, project chat and God approvals first`() {
        val god = attentionJson.decodeFromString(AttentionFeedResponse.serializer(), """
            {"requests":[{"id":"god","item_type":"request","title":"Allow?","status":"needs_action",
              "metadata":{"source":"user_request","input_type":"choice","input_schema":{
                "request_type":"sandbox_override","context":{"god_mode":true,"chat_session_id":"other-chat"},
                "options":[{"id":"allow_once","label":"Allow once"},{"id":"deny","label":"Deny"}]}}}]}
        """.trimIndent()).requests.single()
        val data = feed(god, row("first", task = "turn-1"), row("latest", task = "turn-2"),
            row("project", thread = "project-chat"), row("unrelated", task = "another-run"))
        val scoped = AskSurfaces.forVibeDev(data, setOf("turn-1", "turn-2"), "project-chat")
        assertEquals(setOf("god", "first", "latest", "project"), scoped.items.take(4).map { it.id }.toSet())
        assertEquals("unrelated", scoped.items.last().id)
        assertEquals(scoped.items.take(4).map { it.askKey }.toSet(), scoped.here)
        val bare = AskSurfaces.forVibeDev(data, emptySet())
        assertEquals(5, bare.items.size)
        assertEquals(setOf(god.askKey), bare.here)
        assertFalse(AskSurfaces.forVibeDev(data, emptySet(), settled = god.askIds).items.any { it.id == "god" })
    }

    @Test
    fun `a task named by a transcript escalation remains in the composer task set`() {
        val message = transcriptAsk("m-task", "c-task")
        val stamped = message.copy(escalation = message.escalation!!.copy(taskId = "child-task"))
        assertEquals(setOf("child-task"), ComposerAskDock.taskIds(listOf(stamped)))
    }

    @Test
    fun `every canonical input kind remains discoverable above the composer`() {
        val kinds = listOf("text", "password", "otp", "choice", "multi_choice", "confirmation", "external_action",
            "file_path", "guidance", "tool_authorization", "sandbox_override", "diff_approval", "form")
        val rows = kinds.map { kind -> row(kind, thread = "chat-1").let { it.copy(metadata = it.metadata!!.copy(inputType = kind)) } }
        assertEquals(kinds.toSet(), AskSurfaces.forThread(feed(*rows.toTypedArray()), setOf("chat-1"), emptySet()).map { it.id }.toSet())
    }

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
    fun `the composer dock lists every open ask with this conversation's first`() {
        val otherOld = row("other-old", thread = "session-2").copy(updatedAt = 1)
        val hereThread = row("here-thread", thread = "session-1").copy(updatedAt = 2)
        val otherNew = row("other-new", task = "task-other").copy(updatedAt = 3)
        val hereTask = row("here-task", task = "task-9").copy(updatedAt = 4)
        val failed = row("failed", thread = "session-2", type = "failed", status = "failed").copy(updatedAt = 5)
        val asks = AskSurfaces.forComposer(
            feed(otherOld, hereThread, otherNew, hereTask, failed), setOf("session-1"), setOf("task-9"),
        )
        assertEquals(listOf("here-thread", "here-task", "other-old", "other-new"), asks.items.map { it.id })
        assertEquals(setOf(hereThread.askKey, hereTask.askKey), asks.here)
        // A conversation with no thread or task still lists every open ask, none of them here.
        val bare = AskSurfaces.forComposer(feed(otherOld, hereThread), emptySet(), emptySet())
        assertEquals(listOf("other-old", "here-thread"), bare.items.map { it.id })
        assertTrue(bare.here.isEmpty())
    }

    @Test
    fun `a plan approval the planner dock offers here is left to it, one from elsewhere is not`() {
        fun plan(id: String, task: String) = row(id, task = task).let {
            it.copy(metadata = it.metadata!!.copy(source = "plan_approval", inputType = "confirmation"))
        }
        val here = plan("plan-here", task = "task-9")
        val other = plan("plan-other", task = "task-other")
        val offered = AskSurfaces.forComposer(
            feed(here, other), setOf("session-1"), setOf("task-9"), planApprovalTaskIds = setOf("task-9", "task-other"),
        )
        assertEquals(listOf("plan-other"), offered.items.map { it.id })
        assertTrue(offered.here.isEmpty())
        // Not offered by the planner dock, it is a question here like any other.
        assertEquals(setOf(here.askKey), AskSurfaces.forComposer(feed(here, other), setOf("session-1"), setOf("task-9")).here)
    }

    @Test
    fun `a transcript card is here, a feed ask only when the selection names it`() {
        val messages = listOf(transcriptAsk("m1", correlation = "c-shared"), transcriptAsk("m2", correlation = "c-only"))
        val shared = row("shared", correlation = "c-shared")
        val other = row("other")
        val merged = ComposerAskDock.merged(messages, listOf(shared, other))
        assertTrue(ComposerAskDock.isHere(merged[0], emptySet())) // the feed row that replaced a card
        assertTrue(merged[1] is ComposerAsk.Transcript && ComposerAskDock.isHere(merged[1], emptySet()))
        assertFalse(ComposerAskDock.isHere(merged[2], emptySet()))
        assertTrue(ComposerAskDock.isHere(merged[2], setOf(other.askKey)))
        assertTrue(ComposerAskDock.isHere(merged[2], null))
    }

    @Test
    fun `a question from elsewhere names the chat or task it came from`() {
        val chat = row("chat", task = "task-x").copy(chatSessionId = "abcdef123456", agentId = "researcher")
        assertEquals("From Trip plans · researcher", ComposerAskDock.originLabel(chat, mapOf("abcdef123456" to "Trip plans")))
        assertEquals("From Chat abcdef12 · researcher", ComposerAskDock.originLabel(chat, emptyMap()))
        val task = row("task", task = "task-1234567890abc")
        assertEquals("From task “Weekly digest”",
            ComposerAskDock.originLabel(task, emptyMap(), mapOf("task-1234567890abc" to "Weekly digest")))
        assertEquals("From task task-12345678", ComposerAskDock.originLabel(task, emptyMap()))
        assertEquals("From background run", ComposerAskDock.originLabel(row("bare"), emptyMap()))
    }

    @Test
    fun `another chat's question in the same named thread is not here and is labelled with its chat`() {
        val other = row("other", thread = "trip").copy(chatSessionId = "chat-b")
        val asks = AskSurfaces.forComposer(feed(other), setOf("chat-a", "trip"), emptySet(), chatSessionIds = setOf("chat-a"))
        assertEquals(listOf("other"), asks.items.map { it.id })
        assertTrue(asks.here.isEmpty())
        assertEquals("From Packing", ComposerAskDock.originLabel(other, mapOf("chat-b" to "Packing")))
        // Stamped only in input_schema.context, the same rule holds — even for a task this chat started.
        val stamped = row("stamped", thread = "trip", task = "task-9").let { it.copy(metadata = it.metadata!!.copy(
            inputSchema = AttentionInputSchema(context = attentionJson.parseToJsonElement("""{"chat_session_id":"chat-b"}""")),
        )) }
        assertTrue(AskSurfaces.forThread(feed(stamped), setOf("chat-a", "trip"), setOf("task-9"), chatSessionIds = setOf("chat-a")).isEmpty())
    }

    @Test
    fun `this chat's question is here`() {
        val mine = row("mine", thread = "trip").copy(chatSessionId = "chat-a")
        val asks = AskSurfaces.forComposer(feed(mine), setOf("chat-a", "trip"), emptySet(), chatSessionIds = setOf("chat-a"))
        assertEquals(setOf(mine.askKey), asks.here)
    }

    @Test
    fun `a taskless question with no session in a named thread still matches by thread`() {
        val bare = row("bare", thread = "trip")
        val asks = AskSurfaces.forComposer(feed(bare), setOf("chat-a", "trip"), emptySet(), chatSessionIds = setOf("chat-a"))
        assertEquals(setOf(bare.askKey), asks.here)
        assertTrue(AskSurfaces.forComposer(feed(bare), setOf("chat-c", "work"), emptySet(), chatSessionIds = setOf("chat-c")).here.isEmpty())
    }

    @Test
    fun `the dock shows a pause once when the transcript and the feed both carry it`() {
        val messages = listOf(transcriptAsk("m1", correlation = "c-shared"))
        val feedAsks = listOf(row("shared", correlation = "c-shared", thread = "s"), row("only-feed", thread = "s"))
        val merged = ComposerAskDock.merged(messages, feedAsks)
        assertEquals(2, merged.size)
        assertEquals("shared", (merged[0] as ComposerAsk.Feed).item.id)
        assertEquals("only-feed", (merged[1] as ComposerAsk.Feed).item.id)
        assertEquals(listOf("m1", "feed:c-only-feed"), merged.map { it.key })
    }

    @Test
    fun `a duplicate transcript cannot hide canonical God mode scope choices`() {
        val canonical = row("god", correlation = "approval", thread = "s").copy(metadata = AttentionMetadata(
            source = "user_request", inputType = "choice", pauseStateId = "approval",
            inputSchema = AttentionInputSchema(requestType = "sandbox_override",
                context = attentionJson.parseToJsonElement("""{"god_mode":true,"chat_session_id":"origin"}"""),
                options = listOf(ai.magicbeans.magdroid.chat.EscalationOption("allow_once", "Allow once"),
                    ai.magicbeans.magdroid.chat.EscalationOption("session:file", "This session: file"),
                    ai.magicbeans.magdroid.chat.EscalationOption("always:file", "Always: file"),
                    ai.magicbeans.magdroid.chat.EscalationOption("deny", "Deny"))),
        ))
        val merged = ComposerAskDock.merged(listOf(transcriptAsk("m", "approval")), listOf(canonical))
        assertEquals(1, merged.size)
        val shown = (merged.single() as ComposerAsk.Feed).item.request(canonical.metadata)
        assertTrue(shown.isGodMode)
        assertEquals(listOf("allow_once", "session:file", "always:file", "deny"), shown.grantOptions.map { it.id })
        assertEquals("m", merged.single().key)
    }

    @Test
    fun `a dismiss-only notice stays in Attention, out of the chat and VibeDev composers`() {
        fun asking(id: String, vararg options: String) = row(id, thread = "s").copy(metadata = AttentionMetadata(
            source = "service_health", inputType = "choice",
            inputSchema = AttentionInputSchema(type = "choice",
                options = options.map { ai.magicbeans.magdroid.chat.EscalationOption(it, it) }),
        ))
        val notice = asking("notice", "dismiss")
        val question = asking("question", "dismiss", "retry")
        assertTrue(notice.isDismissOnly)
        assertFalse(question.isDismissOnly)
        val all = feed(notice, question)
        assertEquals(listOf(notice.askKey, question.askKey).toSet(), AskSurfaces.open(all).map { it.askKey }.toSet())
        assertEquals(listOf(question.askKey), AskSurfaces.forComposer(all, setOf("s"), emptySet()).items.map { it.askKey })
        assertEquals(listOf(question.askKey), AskSurfaces.forComposer(all, setOf("other"), emptySet()).items.map { it.askKey })
        assertEquals(listOf(question.askKey), AskSurfaces.forVibeDev(all, emptySet(), chatSessionId = "s").items.map { it.askKey })
    }

    @Test
    fun `another chat's God mode approval is listed with its origin in the composer`() {
        val god = row("god", correlation = "approval", thread = "s").copy(metadata = AttentionMetadata(
            source = "user_request", inputType = "choice", pauseStateId = "approval",
            inputSchema = AttentionInputSchema(requestType = "sandbox_override",
                context = attentionJson.parseToJsonElement("""{"god_mode":true,"chat_session_id":"origin"}""")),
        ))
        // The thread view lists a workspace-wide approval everywhere.
        assertEquals(listOf(god.askKey), AskSurfaces.forThread(feed(god), setOf("s"), emptySet(), chatSessionIds = setOf("mine")).map { it.askKey })
        // The composer keeps another chat's with its origin, as web and iOS do.
        val composer = AskSurfaces.forComposer(feed(god), setOf("s"), emptySet(), chatSessionIds = setOf("mine"))
        assertTrue(composer.here.isEmpty())
        assertEquals(listOf(god.askKey), composer.items.map { it.askKey })
        // Its own chat answers it as its own; VibeDev keeps God approvals first.
        assertEquals(setOf(god.askKey), AskSurfaces.forComposer(feed(god), setOf("s"), emptySet(), chatSessionIds = setOf("origin")).here)
        assertEquals(setOf(god.askKey), AskSurfaces.forVibeDev(feed(god), emptySet(), chatSessionId = "mine").here)
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
