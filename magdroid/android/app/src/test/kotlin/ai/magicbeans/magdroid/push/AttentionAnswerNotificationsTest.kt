package ai.magicbeans.magdroid.push

import ai.magicbeans.magdroid.attention.AttentionItem
import ai.magicbeans.magdroid.chat.HitlResponseValue
import android.app.Notification
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AttentionAnswerNotificationsTest {

    private fun item(inputType: String, schema: String = "{}", extra: String = ""): AttentionItem {
        val body = """
            {"id":"feed-1","item_type":"request","status":"needs_action","task_id":"task-9",
             "title":"Ship the release?","summary":"Build 42 is green.",
             "metadata":{"source":"agentic","input_type":"$inputType","execution_id":"exec-7",
                         "input_schema":$schema $extra}}
        """.trimIndent()
        return requireNotNull(decodeAttentionItem(body))
    }

    // ---- eligibility and actions per mode ----

    @Test
    fun `a plain confirmation becomes two labelled buttons`() {
        val plan = notificationAnswerPlan(
            "confirm", "agentic",
            item("confirmation", """{"type":"confirmation","confirm_label":"Ship it","deny_label":"Hold"}"""),
        )!!
        assertEquals("Ship the release?", plan.question)
        assertEquals("Build 42 is green.", plan.hint)
        assertEquals(listOf("Ship it", "Hold"), plan.buttons.map { it.label })
        assertEquals(
            listOf(HitlResponseValue.Confirmation(true), HitlResponseValue.Confirmation(false)),
            plan.buttons.map { it.value },
        )
        assertEquals("exec-7", plan.executionId)
        assertEquals("task-9", plan.taskId)
        assertEquals("confirmation", plan.inputType)
    }

    @Test
    fun `a previewed approval reads by its title and change size`() {
        val plan = notificationAnswerPlan(
            "confirm", "agentic",
            item("confirmation", """{"type":"confirmation","approval_preview":{"title":"Mira wants to change `facts.md`",
                "diff":{"path":"facts.md","added":3,"removed":1},
                "warnings":[{"code":"shared_document","message":"Other agents read this document."}]}}"""),
        )!!
        assertEquals("Mira wants to change `facts.md`", plan.question)
        assertEquals("+3 lines, −1 line · Other agents read this document.", plan.hint)
        assertEquals(listOf("Approve", "Reject"), plan.buttons.map { it.label })
    }

    @Test
    fun `a confirmation without labels falls back to Approve and Reject`() {
        val plan = notificationAnswerPlan("confirm", "agentic", item("confirmation"))!!
        assertEquals(listOf("Approve", "Reject"), plan.buttons.map { it.label })
    }

    @Test
    fun `a destructive or sensitive confirmation opens the app instead`() {
        assertNull(notificationAnswerPlan("confirm", "agentic", item("confirmation", """{"destructive":true}""")))
        assertNull(
            notificationAnswerPlan("confirm", "agentic", item("confirmation", """{"sensitive":{"kind":"password"}}""")),
        )
    }

    @Test
    fun `an outward-send review opens the app in every mode`() {
        val review = """{"type":"confirmation","escalation_trigger":"outward_review",
            "approval_preview":{"title":"ambassador wants to email a@b.com"},
            "options":[{"id":"send","label":"Send"},{"id":"discard","label":"Discard"},
                       {"id":"always_allow_recipient","label":"Always allow ambassador to send to a@b.com"}],
            "outward_review":{"capability":"gmail","action":"send","class":"message",
                              "recipients":["a@b.com"],"agent_id":"ambassador","deadline_at_ms":0}}"""
        for (mode in listOf("confirm", "choice", "text")) {
            assertNull(mode, notificationAnswerPlan(mode, "agentic", item("confirmation", review)))
        }
        // The trigger alone is enough, before the record or options arrive.
        assertNull(
            notificationAnswerPlan("confirm", "agentic", item("confirmation", """{"escalation_trigger":"outward_review"}""")),
        )
        // Even if a server named it a choice.
        assertNull(notificationAnswerPlan("choice", "agentic", item("choice", review.replace("\"type\":\"confirmation\"", "\"type\":\"choice\""))))
    }

    @Test
    fun `a mode the fetched ask disagrees with opens the app`() {
        assertNull(notificationAnswerPlan("confirm", "agentic", item("text")))
        assertNull(notificationAnswerPlan("text", "agentic", item("confirmation")))
        assertNull(notificationAnswerPlan(null, "agentic", item("confirmation")))
        assertNull(notificationAnswerPlan("form", "agentic", item("confirmation")))
    }

    @Test
    fun `a single choice of up to three options becomes one button each`() {
        val plan = notificationAnswerPlan(
            "choice", "agentic",
            item(
                "choice",
                """{"type":"choice","options":[{"id":"a","label":"Alpha"},{"id":"b","label":""},{"id":"c","label":"Gamma"}]}""",
            ),
        )!!
        assertEquals(listOf("Alpha", "b", "Gamma"), plan.buttons.map { it.label })
        assertEquals(listOf("opt:a", "opt:b", "opt:c"), plan.buttons.map { it.key })
        assertEquals(HitlResponseValue.Choice("b"), plan.buttons[1].value)
        assertTrue(plan.buttons.size <= MAX_NOTIFICATION_ACTIONS)
    }

    @Test
    fun `four options, multi-select, or an option needing detail opens the app`() {
        val four = """{"options":[{"id":"a"},{"id":"b"},{"id":"c"},{"id":"d"}]}"""
        assertNull(notificationAnswerPlan("choice", "agentic", item("choice", four)))
        val multiple = """{"multiple":true,"options":[{"id":"a"},{"id":"b"}]}"""
        assertNull(notificationAnswerPlan("choice", "agentic", item("choice", multiple)))
        val detail = """{"options":[{"id":"a"},{"id":"other","requires_input":true}]}"""
        assertNull(notificationAnswerPlan("choice", "agentic", item("choice", detail)))
        val sensitive = """{"options":[{"id":"a"}],"sensitive":{"kind":"other"}}"""
        assertNull(notificationAnswerPlan("choice", "agentic", item("choice", sensitive)))
    }

    @Test
    fun `a nested hitl_request schema is read when the item has none of its own`() {
        val body = """
            {"id":"feed-2","item_type":"request","status":"needs_action","title":"Pick one",
             "metadata":{"hitl_request":{"source":"clarification","input_type":"choice",
               "input_schema":{"type":"choice","options":[{"id":"x","label":"X"},{"id":"y","label":"Y"}]}}}}
        """.trimIndent()
        val plan = notificationAnswerPlan("choice", null, decodeAttentionItem(body)!!)!!
        assertEquals(listOf("X", "Y"), plan.buttons.map { it.label })
        assertEquals("clarification", plan.source)
    }

    @Test
    fun `a plain text ask becomes one reply action`() {
        val plan = notificationAnswerPlan("text", "agentic", item("text", """{"placeholder":"Which branch?"}"""))!!
        assertTrue(plan.buttons.isEmpty())
        assertEquals("Which branch?", plan.replyLabel)
    }

    @Test
    fun `secret, otp, and multi-question text asks open the app`() {
        assertNull(notificationAnswerPlan("text", "agentic", item("text", """{"sensitive":{"kind":"otp"}}""")))
        assertNull(notificationAnswerPlan("text", "agentic", item("password")))
        assertNull(
            notificationAnswerPlan("text", "agentic", item("text", """{"questions":[{"id":"q1","prompt":"One"}]}""")),
        )
    }

    @Test
    fun `history rows are never answerable`() {
        val body = """{"id":"f","item_type":"failed","status":"failed","title":"Q","metadata":{"input_type":"confirmation"}}"""
        assertNull(notificationAnswerPlan("confirm", "agentic", decodeAttentionItem(body)!!))
    }

    @Test
    fun `the push source wins over the derived one`() {
        val plan = notificationAnswerPlan("confirm", "plan_approval", item("confirmation"))!!
        assertEquals("plan_approval", plan.source)
    }

    @Test
    fun `a wrapped single-ask body decodes too`() {
        val item = decodeAttentionItem("""{"item":{"id":"w","title":"Wrapped"}}""")
        assertEquals("Wrapped", item?.title)
        assertNull(decodeAttentionItem("not json"))
    }

    // ---- value encoding ----

    @Test
    fun `answers from actions encode to the resume value shapes`() {
        assertEquals(
            HitlResponseValue.Confirmation(false),
            notificationAnswerValue(AttentionActionReceiver.ANSWER_CONFIRM, false, null, null),
        )
        assertEquals(
            HitlResponseValue.Choice("b"),
            notificationAnswerValue(AttentionActionReceiver.ANSWER_CHOICE, false, "b", null),
        )
        assertEquals(
            HitlResponseValue.Text("main"),
            notificationAnswerValue(AttentionActionReceiver.ANSWER_TEXT, false, null, "  main \n"),
        )
        assertNull(notificationAnswerValue(AttentionActionReceiver.ANSWER_TEXT, false, null, "   "))
        assertNull(notificationAnswerValue(AttentionActionReceiver.ANSWER_CHOICE, false, "", null))
        assertNull(notificationAnswerValue("bogus", true, null, null))
    }

    @Test
    fun `the respond body carries the push channel and the run`() {
        val body = Json.parseToJsonElement(
            notificationAnswerBody("corr-1", "agentic", "text", HitlResponseValue.Text("main"), "exec-7", "task-9"),
        ).jsonObject
        assertEquals("agentic", body["source"]!!.jsonPrimitive.content)
        assertEquals("text", body["input_type"]!!.jsonPrimitive.content)
        assertEquals("android_push", body["channel"]!!.jsonPrimitive.content)
        assertEquals("exec-7", body["execution_id"]!!.jsonPrimitive.content)
        assertEquals("task-9", body["task_id"]!!.jsonPrimitive.content)
        val value = body["value"] as JsonObject
        assertEquals("text", value["type"]!!.jsonPrimitive.content)
        assertEquals("main", value["value"]!!.jsonPrimitive.content)

        val confirm = Json.parseToJsonElement(
            notificationAnswerBody("corr-1", "agentic", "confirmation", HitlResponseValue.Confirmation(true), null, " "),
        ).jsonObject
        assertTrue((confirm["value"] as JsonObject)["confirmed"]!!.jsonPrimitive.boolean)
        assertFalse(confirm.containsKey("execution_id"))
        assertFalse(confirm.containsKey("task_id"))

        val choice = Json.parseToJsonElement(
            notificationAnswerBody("c", "agentic", "choice", HitlResponseValue.Choice("opt-2"), null, null),
        ).jsonObject
        assertEquals("opt-2", (choice["value"] as JsonObject)["selected_id"]!!.jsonPrimitive.content)
    }

    // ---- result mapping ----

    @Test
    fun `respond results map to sent, already answered, or failed`() {
        assertEquals(NotificationAnswerOutcome.Sent, notificationAnswerOutcome(200, """{"accepted":true}"""))
        assertEquals(NotificationAnswerOutcome.Sent, notificationAnswerOutcome(204, null))
        assertEquals(NotificationAnswerOutcome.Sent, notificationAnswerOutcome(200, "{}"))
        assertEquals(
            NotificationAnswerOutcome.AlreadyAnswered,
            notificationAnswerOutcome(200, """{"accepted":false,"reason":"already_resolved"}"""),
        )
        assertEquals(NotificationAnswerOutcome.AlreadyAnswered, notificationAnswerOutcome(409, """{"error":"already_resolved"}"""))
        assertEquals(NotificationAnswerOutcome.AlreadyAnswered, notificationAnswerOutcome(404, ""))
        assertEquals(
            NotificationAnswerOutcome.Failed,
            notificationAnswerOutcome(200, """{"accepted":false,"reason":"scope_mismatch"}"""),
        )
        assertEquals(NotificationAnswerOutcome.Failed, notificationAnswerOutcome(403, ""))
        assertEquals(NotificationAnswerOutcome.Failed, notificationAnswerOutcome(500, ""))
    }

    // ---- privacy, links, request codes ----

    @Test
    fun `questions stay off the lock screen unless the owner opts in`() {
        assertEquals(Notification.VISIBILITY_SECRET, answerableNotificationVisibility(false))
        assertEquals(Notification.VISIBILITY_PUBLIC, answerableNotificationVisibility(true))
    }

    @Test
    fun `a failed answer opens this ask`() {
        assertEquals("magican://attention?correlation_id=a%20b", attentionDeepLink(null, "a b"))
        assertEquals(
            "magican://attention?correlation_id=x",
            attentionDeepLink("magican://attention?correlation_id=x", "x"),
        )
        assertEquals("magican://attention?correlation_id=y", attentionDeepLink("https://evil.example", "y"))
    }

    @Test
    fun `every action of every ask gets its own request code`() {
        val codes = setOf(
            answerRequestCode("ask-1", "confirm", 0),
            answerRequestCode("ask-1", "deny", 1),
            answerRequestCode("ask-2", "confirm", 0),
            answerRequestCode("ask-1", "reply", 0),
        )
        assertEquals(4, codes.size)
        assertNotEquals(answerRequestCode("ask-1", "opt:a", 0), answerRequestCode("ask-1", "opt:b", 1))
    }
}
