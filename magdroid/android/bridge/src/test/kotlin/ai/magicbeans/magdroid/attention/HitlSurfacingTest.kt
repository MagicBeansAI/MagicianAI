package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.FormAnswer
import ai.magicbeans.magdroid.chat.HitlResponseComposer
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The 2026-09-30 HITL surfacing contract (C2–C4 in
 * `docs/archive/plans/2026-09-30-hitl-surfacing-fixes.md`), read off wire JSON.
 *
 * Decoded from JSON rather than built from Kotlin objects so a renamed field
 * on either side fails here instead of silently hiding an ask.
 */
class HitlSurfacingTest {

    private fun feed(json: String) =
        attentionJson.decodeFromString(AttentionFeedResponse.serializer(), json)

    // ── C2: a delegated child's ask on the parent task ─────────────────────

    private val childAsk = """
        {"requests":[{
          "id":"v3:attention:a1","item_type":"task","task_id":"parent-task",
          "title":"Which landlord address?","status":"needs_action",
          "execution_id":"child-exec",
          "metadata":{
            "delegation_child":true,
            "delegated_agent_id":"researcher",
            "execution_id":"child-exec",
            "hitl_request":{
              "correlation_id":"pause-child-1","source":"agentic",
              "execution_id":"child-exec","prompt":"Which landlord address?",
              "input_type":"form",
              "input_schema":{"type":"form","questions":[
                {"id":"addr","prompt":"Address","input_type":"text"},
                {"id":"when","prompt":"When","input_type":"choice",
                 "options":[{"id":"now","label":"Now"},{"id":"later","label":"Later"}]},
                {"id":"tags","prompt":"Tags","input_type":"multi_choice",
                 "options":[{"id":"a","label":"A"},{"id":"b","label":"B"}]}
              ]}
            }
          }
        }]}
    """.trimIndent()

    @Test
    fun `a delegated child ask is answerable against the child`() {
        val item = feed(childAsk).requests.single()
        val request = item.request(item.metadata)
        assertTrue(item.isActionable)
        assertEquals("agentic", request.source)
        assertEquals("pause-child-1", request.correlationId)
        assertEquals("child-exec", request.executionId)
        assertEquals("researcher", request.delegatedAgentId)
        assertEquals("from researcher", request.fromLabel)
        assertEquals("form", request.inputType)
        assertEquals("Which landlord address?", request.prompt)
        assertEquals(listOf("addr", "when", "tags"), request.formQuestions.map { it.id })
        assertFalse(request.formQuestions[0].isChoice)
        assertTrue(request.formQuestions[1].isChoice)
        assertFalse(request.formQuestions[1].isMultiChoice)
        assertTrue(request.formQuestions[2].isMultiChoice)
        assertEquals("parent-task", item.openTaskId)
    }

    @Test
    fun `the execution id falls back to the open target scope`() {
        val item = AttentionItem(
            id = "x", itemType = "task", title = "Q", status = "needs_action",
            metadata = AttentionMetadata(
                hitlRequest = HitlRequestPayload(
                    id = "pause-9",
                    scope = HitlRequestScope(executionId = "exec-9"),
                ),
            ),
        )
        val request = item.request(item.metadata)
        assertEquals("pause-9", request.correlationId)
        assertEquals("exec-9", request.executionId)
        // Not a child: no "from" label.
        assertNull(request.fromLabel)
    }

    @Test
    fun `a form answers with values, selected ids and skips`() {
        val json = HitlResponseComposer.composeForm(
            listOf(
                FormAnswer(id = "addr", value = "12 High St"),
                FormAnswer(id = "when", selectedIds = listOf("now")),
                FormAnswer(id = "tags", skipped = true, selectedIds = listOf("a")),
            ),
        )!!.toJson()
        assertEquals("form", json["type"]!!.jsonPrimitive.content)
        val answers = json["answers"]!!.jsonArray.map { it.jsonObject }
        assertEquals("12 High St", answers[0]["value"]!!.jsonPrimitive.content)
        assertNull(answers[0]["selected_ids"])
        assertEquals(listOf("now"), answers[1]["selected_ids"]!!.jsonArray.map { it.jsonPrimitive.content })
        assertNull(answers[1]["value"])
        assertEquals("true", answers[2]["skipped"]!!.jsonPrimitive.content)
        // A skipped answer carries nothing else.
        assertNull(answers[2]["selected_ids"])
    }

    // ── C3: a run waiting with no question ───────────────────────────────────

    private val stalled = """
        {"escalations":[{
          "id":"stalled:exec-7","item_type":"escalation","task_id":"task-7",
          "title":"Draft the landlord email","summary":"Waiting on you, but no question was delivered",
          "status":"needs_action",
          "metadata":{"attention_reason":"waiting_without_question","execution_id":"exec-7",
                      "task_id":"task-7","actions":["cancel","open_task"]}
        }]}
    """.trimIndent()

    @Test
    fun `a stalled wait is a cancel card, not an ask`() {
        val item = feed(stalled).escalations.single()
        assertTrue(item.isStalledWait)
        assertFalse(item.isActionable)
        assertEquals("exec-7", item.stalledExecutionId)
        assertEquals("task-7", item.openTaskId)
        assertTrue(item.offers("cancel"))
        assertTrue(item.offers("open_task"))
        assertFalse(item.offers("retry"))
        // Still counted and listed: it must not vanish from All.
        assertEquals(listOf(item.id), feed(stalled).items(AttentionLane.All).map { it.id })
    }

    @Test
    fun `a mistyped actions field does not break the feed`() {
        val odd = stalled.replace("\"actions\":[\"cancel\",\"open_task\"]", "\"actions\":{\"cancel\":true}")
        val item = feed(odd).escalations.single()
        assertTrue(item.isStalledWait)
        // Unreadable actions offer both verbs rather than none.
        assertTrue(item.offers("cancel"))
    }

    // ── C4: mcp_oauth ────────────────────────────────────────────────────────

    @Test
    fun `mcp oauth is an external action that opens the authorize url`() {
        val json = """
            {"requests":[{
              "id":"mcp:1","item_type":"request","title":"Connect GitHub","status":"needs_action",
              "metadata":{"source":"mcp_oauth",
                "hitl_request":{"correlation_id":"oauth-1","source":"mcp_oauth","input_type":"text",
                  "input_schema":{"url":"https://example.test/authorize?state=1"}}}
            }]}
        """.trimIndent()
        val item = feed(json).requests.single()
        val request = item.request(item.metadata)
        assertEquals(MCP_OAUTH_SOURCE, request.source)
        assertEquals("oauth-1", request.correlationId)
        assertEquals("external_action", request.inputType)
        assertEquals("https://example.test/authorize?state=1", request.externalUrl)
        assertEquals("I've finished", request.externalDoneLabel)
        assertEquals(
            "external_action_completed",
            ai.magicbeans.magdroid.chat.HitlResponseValue.ExternalActionCompleted(null)
                .toJson()["type"]!!.jsonPrimitive.content,
        )
    }

    // ── Nothing is silently dropped ──────────────────────────────────────────

    @Test
    fun `an unknown source and input type still answer in words`() {
        val json = """
            {"requests":[{
              "id":"n:1","item_type":"request","title":"Something new","status":"needs_action",
              "metadata":{"source":"brand_new_source","input_type":"hologram",
                          "pause_state_id":"p-new"}
            }]}
        """.trimIndent()
        val item = feed(json).requests.single()
        val request = item.request(item.metadata)
        assertTrue(item.isActionable)
        assertEquals("brand_new_source", request.source)
        assertEquals("p-new", request.correlationId)
        assertEquals("text", request.inputType)
    }

    @Test
    fun `an unknown input type with options keeps its options`() {
        val item = AttentionItem(
            id = "o", itemType = "request", title = "Pick", status = "needs_action",
            metadata = AttentionMetadata(
                inputType = "hologram",
                options = listOf(ai.magicbeans.magdroid.chat.EscalationOption("a", "A")),
            ),
        )
        assertEquals("hologram", item.request(item.metadata).inputType)
    }
}
