package ai.magicbeans.magdroid.attention

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PendingUserRequestsTest {
    @Test fun `exact shell scope keeps both approval durations`() {
        val scopes = godGrantScopes(listOf(AttentionGrantOption("allow_once", "Allow once"),
            AttentionGrantOption("session:exact", "This session: this exact shell invocation"),
            AttentionGrantOption("always:exact", "Always: this exact shell invocation"),
            AttentionGrantOption("deny", "Deny")))
        assertEquals(1, scopes.size)
        assertEquals("Exact command", scopes.first().label)
        assertEquals("session:exact", scopes.first().sessionId)
        assertEquals("always:exact", scopes.first().alwaysId)
    }
    @Test fun `scope picker preserves exact server ids and full long paths`() {
        val path = "/notes/" + "long-folder/".repeat(40) + "plan.md"
        val options = listOf(AttentionGrantOption("allow_once", "Allow once"),
            AttentionGrantOption("session:file", "This session: $path"), AttentionGrantOption("always:file", "Always: $path"),
            AttentionGrantOption("session:folder", "This session: anything under /notes"), AttentionGrantOption("always:folder", "Always: anything under /notes"),
            AttentionGrantOption("deny", "Deny"))
        val scopes = godGrantScopes(options)
        assertEquals(listOf("file", "folder"), scopes.map { it.id })
        assertEquals(path, scopes.first().detail)
        assertEquals("session:file", scopes.first().sessionId)
        assertEquals("always:folder", scopes.last().alwaysId)
        assertTrue(godGrantScopes(options.filter { it.id != "always:file" }).isEmpty())
        assertTrue(godGrantScopes(options + AttentionGrantOption("new_option", "New choice")).isEmpty())
    }
    private val choice = """{"requests":[{"id":"choice-1","question":"Choose the test color","options":[{"id":"blue","label":"Blue"},{"id":"red","label":"Red"}],"context":{"input_type":"choice","input_schema":{"input_type":"choice","options":[{"id":"blue","label":"Blue"},{"id":"red","label":"Red"}]}},"created_at":42}]}"""

    @Test fun `a taskless chat question reaches its origin from the ledger and canonical feed`() {
        val raw = """{"requests":[{"id":"question-1","request_type":"need_user_input","question":"Approve the work plan?","context":{"chat_session_id":"chat-1","input_type":"confirmation","input_schema":{"confirm_label":"Approve","deny_label":"Deny"}}}]}"""
        val pending = attentionJson.decodeFromString(PendingUserRequests.serializer(), raw)
        val feed = AttentionFeedResponse().withPendingUserRequests(pending.requests)
        assertEquals(1, AskSurfaces.forThread(feed, setOf("chat-1"), emptySet()).size)
        assertTrue(AskSurfaces.forThread(feed, setOf("chat-2"), emptySet()).isEmpty())
        assertTrue(AskSurfaces.forThread(feed, emptySet(), emptySet()).isEmpty())
        val projected = attentionJson.decodeFromString(AttentionItem.serializer(), """{"id":"feed-question","item_type":"request","status":"needs_action","title":"Approve the work plan?","metadata":{"source":"user_request","input_type":"confirmation","pause_state_id":"question-1","input_schema":{"context":{"chat_session_id":"chat-1"}}}}""")
        assertEquals(1, AskSurfaces.forThread(AttentionFeedResponse(requests = listOf(projected)), setOf("chat-1"), emptySet()).size)
    }

    @Test fun `God file grants retain diff and appear in every composer with their original session`() {
        val raw = """{"requests":[{"id":"god-file","request_type":"sandbox_override","question":"Apply edits?","options":[{"id":"allow_once","label":"Allow once"},{"id":"session:file","label":"This session: plan.md"},{"id":"always:file","label":"Always: plan.md"},{"id":"deny","label":"Deny"}],"context":{"input_type":"choice","god_mode":true,"chat_session_id":"chat-1","command":"/notes/programs/plan.md","files":[{"path":"/notes/programs/plan.md","unified_diff":"+new plan"}]}}]}"""
        val ledger = attentionJson.decodeFromString(PendingUserRequests.serializer(), raw)
        val item = ledger.requests.single().attentionItem()
        val request = item.request(item.metadata)
        assertTrue(request.isGodMode)
        assertEquals("choice", request.inputType)
        assertEquals("sandbox_override", request.renderKind)
        assertEquals("/notes/programs/plan.md", request.grantSubject)
        assertEquals("+new plan", request.godFiles.single().unifiedDiff)
        assertEquals(listOf("allow_once", "session:file", "always:file", "deny"), request.grantOptions.map { it.id })
        org.junit.Assert.assertFalse(AskSurfaces.answersInSheet(request, compact = true))
        val feed = AttentionFeedResponse(requests = listOf(item.copy(uiThreadId = "general", taskId = "task-1")))
        assertEquals(1, AskSurfaces.forThread(feed, emptySet(), emptySet()).size)
        assertEquals(1, AskSurfaces.forThread(feed, setOf("another-chat"), emptySet()).size)
        assertEquals("chat-1", request.godChatSessionId)
        // The feed projection uses the same schema, not a second native grant store.
        val projected = item.copy(metadata = item.metadata!!.copy(hitlRequest = HitlRequestPayload(inputSchema = item.metadata.inputSchema), inputSchema = null))
        assertTrue(projected.request(projected.metadata).isGodMode)
    }

    @Test fun `a canonical chat question is answerable when the feed is empty`() {
        val pending = attentionJson.decodeFromString(PendingUserRequests.serializer(), choice)
        val feed = AttentionFeedResponse().withPendingUserRequests(pending.requests)
        val item = feed.requests.single()
        val request = item.request(item.metadata)
        assertEquals("user_request", request.source)
        assertEquals("choice-1", request.correlationId)
        assertEquals("choice", request.inputType)
        assertEquals(listOf("blue", "red"), request.options.map { it.id })
        assertTrue(item.isActionable)
        assertEquals(1L, feed.laneCount(AttentionLane.All))
        assertEquals(1L, feed.counts.needsAction)
    }

    @Test fun `an existing feed projection is not duplicated under another item id`() {
        val pending = attentionJson.decodeFromString(PendingUserRequests.serializer(), choice).requests
        val projected = pending.single().attentionItem().copy(id = "feed-row-94")
        val feed = AttentionFeedResponse(
            requests = listOf(projected),
            counts = AttentionLaneCounts(requests = 1, needsAction = 1),
            totals = AttentionLaneTotals(requests = 1),
        ).withPendingUserRequests(pending + pending)
        assertEquals(listOf("feed-row-94"), feed.requests.map { it.id })
        assertEquals(1L, feed.laneCount(AttentionLane.All))
    }

    @Test fun `a form keeps every field for the existing attention form renderer`() {
        val payload = """{"requests":[{"id":"form-1","question":"Connection details","context":{"input_type":"form","input_schema":{"questions":[{"id":"color","question":"Color","input_type":"choice","options":[{"id":"blue","label":"Blue"}]},{"id":"label","question":"Label","input_type":"text"}]}}}]}"""
        val item = attentionJson.decodeFromString(PendingUserRequests.serializer(), payload)
            .requests.single().attentionItem()
        val request = item.request(item.metadata)
        assertEquals("form", request.inputType)
        assertEquals(listOf("color", "label"), request.formQuestions.map { it.id })
    }
}
