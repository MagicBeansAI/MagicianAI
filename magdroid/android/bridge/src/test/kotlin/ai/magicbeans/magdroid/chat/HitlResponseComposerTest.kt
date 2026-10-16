package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The answer shapes the resume API validates.
 *
 * These assert the JSON that goes on the wire, not a round trip through our own
 * types — the server is the other party to this contract and it rejects shapes
 * it does not recognise. A wrong one here leaves an execution paused with no way
 * to answer it, which is the failure worth a test each.
 *
 * Mirrors `ChatHitlResponseComposer` in `magios/Shared/ExecutionPanelTypes.swift`;
 * the two must agree, and a difference is a bug in whichever moved last.
 */
class HitlResponseComposerTest {

    private fun compose(
        inputType: String?,
        option: EscalationOption? = null,
        text: String = "",
        selectedIds: List<String> = emptyList(),
    ) = HitlResponseComposer.compose(inputType, option, text, selectedIds)

    // ── The typed kinds ──────────────────────────────────────────────────────

    @Test
    fun `text is trimmed and carried as value`() {
        val json = compose("text", text = "  do the thing  ")!!.toJson()
        assertEquals("text", json["type"]!!.jsonPrimitive.content)
        assertEquals("do the thing", json["value"]!!.jsonPrimitive.content)
    }

    /**
     * Whitespace can be part of a secret, so a password is never trimmed.
     *
     * Trimming here would produce an authentication failure the owner cannot
     * see the cause of — the value they typed is not the value that was sent.
     */
    @Test
    fun `password keeps surrounding whitespace`() {
        val json = compose("password", text = "  hunter2  ")!!.toJson()
        assertEquals("password", json["type"]!!.jsonPrimitive.content)
        assertEquals("  hunter2  ", json["value"]!!.jsonPrimitive.content)
    }

    // ── P3 Task 3.10: codes and spec-classified asks are exact ───────────────

    @Test
    fun `a one-time code rides the password value shape, leading zero intact`() {
        val json = compose("otp", text = "007123")!!.toJson()
        assertEquals("password", json["type"]!!.jsonPrimitive.content)
        assertEquals("007123", json["value"]!!.jsonPrimitive.content)
        assertNull(compose("otp", text = ""))
    }

    @Test
    fun `a text ask the backend classified as a secret keeps its type and its exact bytes`() {
        val json = HitlResponseComposer.compose("text", text = " 007123 ", sensitive = true)!!.toJson()
        assertEquals("text", json["type"]!!.jsonPrimitive.content)
        assertEquals(" 007123 ", json["value"]!!.jsonPrimitive.content)
        val plain = HitlResponseComposer.compose("text", text = " city ", sensitive = false)!!.toJson()
        assertEquals("city", plain["value"]!!.jsonPrimitive.content)
        val aborted = HitlResponseValue.Aborted("fresh_code_requested").toJson()
        assertEquals("aborted", aborted["type"]!!.jsonPrimitive.content)
        assertEquals("fresh_code_requested", aborted["reason"]!!.jsonPrimitive.content)
    }

    @Test
    fun `the render kind follows the spec, never a decision`() {
        assertEquals("otp", hitlRenderKind("text", "otp"))
        assertEquals("password", hitlRenderKind("text", "password"))
        assertEquals("password", hitlRenderKind("guidance", "other"))
        assertEquals("text", hitlRenderKind("text", "login_identifier"))
        assertEquals("choice", hitlRenderKind("choice", "password"))
        assertEquals("otp", hitlRenderKind("otp", null))
        assertEquals("text", hitlRenderKind("text", null))
        assertTrue(hitlFieldIsMasked("password"))
        assertTrue(hitlFieldIsMasked("otp"))
        assertTrue(!hitlFieldIsMasked("login_identifier"))
        assertTrue(!hitlFieldIsMasked(null))
        val spec = SensitiveSpec(
            fields = listOf(SensitiveField("pw", "password"), SensitiveField("user", "login_identifier")),
            collectionDeadlineMs = 1_700_000_180_000,
        )
        assertEquals("password", spec.fieldKind("pw"))
        assertNull(spec.fieldKind("city"))
        assertTrue(spec.isExpired(nowMs = 1_700_000_180_000))
        assertTrue(!spec.isExpired(nowMs = 1_700_000_179_999))
    }

    @Test
    fun `guidance is advice, not value`() {
        val json = compose("guidance", text = " try the other one ")!!.toJson()
        assertEquals("guidance", json["type"]!!.jsonPrimitive.content)
        assertEquals("try the other one", json["advice"]!!.jsonPrimitive.content)
        assertNull(json["value"])
    }

    @Test
    fun `file paths split on commas and newlines`() {
        val json = compose("file_path", text = " /a/one.txt , /b/two.txt \n/c/three.txt ")!!.toJson()
        assertEquals("file_path", json["type"]!!.jsonPrimitive.content)
        assertEquals(
            listOf("/a/one.txt", "/b/two.txt", "/c/three.txt"),
            json["paths"]!!.jsonArray.map { it.jsonPrimitive.content },
        )
    }

    @Test
    fun `a single file path keeps a comma in the path`() {
        val json = HitlResponseComposer.compose(
            inputType = "file_path",
            text = " /tmp/report, final.txt ",
            allowsMultipleFiles = false,
        )!!.toJson()
        assertEquals(listOf("/tmp/report, final.txt"), json["paths"]!!.jsonArray.map { it.jsonPrimitive.content })
    }

    @Test
    fun `multi choice carries every selected id`() {
        val json = compose("multi_choice", selectedIds = listOf("a", "c"))!!.toJson()
        assertEquals("multi_choice", json["type"]!!.jsonPrimitive.content)
        assertEquals(listOf("a", "c"), json["selected_ids"]!!.jsonArray.map { it.jsonPrimitive.content })
    }

    // ── Nothing incomplete reaches the network ───────────────────────────────

    /**
     * Null is the whole point of the composer.
     *
     * An empty form that composed to an empty value would resume a paused
     * execution with no answer in it, which is worse than the button appearing
     * to do nothing.
     */
    @Test
    fun `an unanswered form composes to nothing`() {
        assertNull(compose("text", text = "   "))
        assertNull(compose("password", text = ""))
        assertNull(compose("guidance", text = "\n"))
        assertNull(compose("file_path", text = " , , "))
        assertNull(compose("multi_choice", selectedIds = emptyList()))
        assertNull(compose("confirmation"))
        assertNull(compose(null))
    }

    @Test
    fun `an option needing detail is refused until it has some`() {
        val option = EscalationOption(id = "other", label = "Other", requiresInput = true)
        assertNull(compose("choice", option = option, text = "  "))
        assertNull(compose("external_action", option = option, text = ""))

        val answered = compose("choice", option = option, text = "because")!!.toJson()
        assertEquals("because", answered["other_value"]!!.jsonPrimitive.content)
    }

    // ── Confirmation maps words to a boolean ─────────────────────────────────

    @Test
    fun `affirmative and negative ids become a confirmation`() {
        listOf("approve", "allow", "yes", "confirm", "continue", "done").forEach { id ->
            val json = compose("confirmation", option = EscalationOption(id = id))!!.toJson()
            assertEquals("confirmation", json["type"]!!.jsonPrimitive.content)
            assertTrue("$id should be affirmative", json["confirmed"]!!.jsonPrimitive.content.toBoolean())
        }
        listOf("reject", "deny", "no", "cancel", "stop", "dismiss").forEach { id ->
            val json = compose("confirmation", option = EscalationOption(id = id))!!.toJson()
            assertEquals(false, json["confirmed"]!!.jsonPrimitive.content.toBoolean())
        }
    }

    /**
     * An id this build has never seen answers as itself.
     *
     * Guessing it into a yes or a no would be answering a confirmation the
     * owner did not give, on a pause that exists precisely to ask them.
     */
    @Test
    fun `an unknown confirmation id stays a choice`() {
        val json = compose("confirmation", option = EscalationOption(id = "defer"))!!.toJson()
        assertEquals("choice", json["type"]!!.jsonPrimitive.content)
        assertEquals("defer", json["selected_id"]!!.jsonPrimitive.content)
    }

    // ── Choice and external action ───────────────────────────────────────────

    @Test
    fun `a plain choice carries only its id`() {
        val json = compose("choice", option = EscalationOption(id = "allow_once"))!!.toJson()
        assertEquals("choice", json["type"]!!.jsonPrimitive.content)
        assertEquals("allow_once", json["selected_id"]!!.jsonPrimitive.content)
        assertNull(json["other_value"])
    }

    @Test
    fun `an unknown input type still answers as a choice`() {
        // Forward compatibility: the server may add kinds. Falling back to the
        // option the owner actually picked beats refusing to answer at all.
        val json = compose("some_future_kind", option = EscalationOption(id = "ok"))!!.toJson()
        assertEquals("choice", json["type"]!!.jsonPrimitive.content)
        assertEquals("ok", json["selected_id"]!!.jsonPrimitive.content)
    }

    @Test
    fun `external action completes with optional guidance`() {
        val bare = compose("external_action", option = EscalationOption(id = "done"))!!.toJson()
        assertEquals("external_action_completed", bare["type"]!!.jsonPrimitive.content)
        assertNull(bare["guidance"])

        val told = compose("external_action", option = EscalationOption(id = "done"), text = " signed in ")!!
            .toJson()
        assertEquals("signed in", told["guidance"]!!.jsonPrimitive.content)
    }

    @Test
    fun `a form carries skipped flags without aborting`() {
        val json = HitlResponseComposer.composeForm(
            listOf(
                FormAnswer(id = "release", value = "stable"),
                FormAnswer(id = "owner", skipped = true),
            ),
        )!!.toJson()
        assertEquals("form", json["type"]!!.jsonPrimitive.content)
        val answers = json["answers"]!!.jsonArray
        assertEquals(2, answers.size)
        assertEquals("release", answers[0].jsonObject["id"]!!.jsonPrimitive.content)
        assertEquals("stable", answers[0].jsonObject["value"]!!.jsonPrimitive.content)
        assertEquals(true, answers[1].jsonObject["skipped"]!!.jsonPrimitive.content.toBoolean())
    }

    @Test
    fun `an empty form composes to nothing`() {
        assertNull(HitlResponseComposer.composeForm(emptyList()))
        assertNull(compose("form", text = "ignored"))
    }
}
