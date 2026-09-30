package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Answering a paused execution, against `respond_hitl_handler` in `web_api.rs`.
 *
 * The body is `{source, value, input_type?, channel?, execution_id?}` where
 * `value` is an `AgenticResumeValue` tagged by `type`. Getting the tag or the
 * field name wrong is a 400 the user would see as "nothing happened".
 */
class HitlRespondTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `the answer matches the AgenticResumeValue choice shape`() {
        val body = chatRequestJson.encodeToString(
            HitlRespondRequest.serializer(),
            HitlRespondRequest(
                value = HitlResponseValue.Choice(selectedId = "allow_once").toJson(),
                inputType = "confirmation",
                executionId = "exec-1",
            ),
        )
        val obj = json.parseToJsonElement(body).jsonObject
        assertEquals("escalation", obj["source"]?.jsonPrimitive?.content)
        assertEquals("android", obj["channel"]?.jsonPrimitive?.content)
        assertEquals("confirmation", obj["input_type"]?.jsonPrimitive?.content)
        assertEquals("exec-1", obj["execution_id"]?.jsonPrimitive?.content)

        // `#[serde(tag = "type", rename_all = "snake_case")]` — the variant is
        // named by `type` and its field is `selected_id`.
        val value = obj["value"]!!.jsonObject
        assertEquals("choice", value["type"]?.jsonPrimitive?.content)
        assertEquals("allow_once", value["selected_id"]?.jsonPrimitive?.content)
    }

    /**
     * The pause's own `input_type` is sent back rather than derived from the
     * option, because choice-shaped values are also used by specialised pauses
     * whose contract would otherwise be lost.
     */
    @Test
    fun `the input type comes from the escalation, not the answer`() {
        val detail = json.decodeFromString(
            ChatSessionDetail.serializer(),
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "direction": "assistant",
               "content": {"type": "escalation", "execution_id": "e", "pause_state_id": "p",
                           "escalation_type": "tool_authorization", "input_type": "confirmation",
                           "question": "Allow the tool?",
                           "options": [{"id": "allow_once", "label": "Allow once"}]}}]}
            """,
        )
        val card = detail.messages.single().project(0).escalation!!
        assertEquals("confirmation", card.inputType)
        assertEquals("p", card.correlationId)
        assertEquals("e", card.executionId)
    }

    /** An accepted answer is a success with nothing to say. */
    @Test
    fun `an accepted response has no refusal to report`() {
        val result = json.decodeFromString(
            HitlRespondResponse.serializer(),
            """{"accepted": true, "source": "escalation"}""",
        )
        assertTrue(result.accepted)
    }

    /**
     * The failure that matters: HTTP 200, `accepted: false`. Trusting the
     * status code alone would mark the card answered while the execution stays
     * paused.
     */
    @Test
    fun `a soft refusal is not a success`() {
        val result = json.decodeFromString(
            HitlRespondResponse.serializer(),
            """{"accepted": false, "source": "escalation", "reason": "already_resolved"}""",
        )
        assertFalse(result.accepted)
        assertEquals("That was already answered.", result.refusal())
    }

    @Test
    fun `a scope mismatch says whose it is`() {
        val result = json.decodeFromString(
            HitlRespondResponse.serializer(),
            """{"accepted": false, "reason": "scope_mismatch"}""",
        )
        assertEquals("That escalation belongs to another workspace.", result.refusal())
    }

    /** An unfamiliar reason is passed through rather than swallowed. */
    @Test
    fun `an unknown reason is reported as given`() {
        val result = json.decodeFromString(
            HitlRespondResponse.serializer(),
            """{"accepted": false, "reason": "pause_expired"}""",
        )
        assertEquals("pause_expired", result.refusal())
    }

    /** A refusal with no reason still has to say something. */
    @Test
    fun `a bare refusal still explains itself`() {
        val result = json.decodeFromString(HitlRespondResponse.serializer(), """{"accepted": false}""")
        assertEquals("Magician did not accept the answer.", result.refusal())
    }

    /**
     * `correlation_id` is preferred, then `request_id`, then `pause_state_id` —
     * the canonical id differs by which subsystem raised the pause.
     */
    @Test
    fun `the answer is addressed to the canonical id`() {
        assertEquals(
            "req-1",
            MessageContent(requestId = "req-1", pauseStateId = "pause-1").hitlCorrelationId(),
        )
        assertEquals("pause-1", MessageContent(pauseStateId = "pause-1").hitlCorrelationId())
        assertEquals(null, MessageContent().hitlCorrelationId())
    }
}
