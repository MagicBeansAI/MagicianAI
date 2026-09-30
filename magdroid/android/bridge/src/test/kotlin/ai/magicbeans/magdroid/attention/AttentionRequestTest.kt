package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.EscalationOption
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The answer contract read off a feed item.
 *
 * Every field here is a fallback chain, written by whichever subsystem raised
 * the item. Getting one wrong does not fail loudly: it posts a well-formed
 * answer to an id nothing is waiting on, and the execution stays paused with
 * the owner believing they answered it.
 *
 * Mirrors the web's `hitlRequestFromFeedItem` and iOS's derived properties.
 * Three clients read the same object and must agree.
 */
class AttentionRequestTest {

    private fun item(id: String = "i1", type: String = "escalation", title: String = "T") =
        AttentionItem(id = id, itemType = type, title = title, status = "needs_action")

    // ── Where the answer is posted ───────────────────────────────────────────

    @Test
    fun `an approval posts as an approval whatever the metadata says`() {
        val request = item(type = "approval").request(
            AttentionMetadata(source = "agentic", attentionKind = "clarification"),
        )
        assertEquals("approval", request.source)
        // And answers as a confirmation, not as whatever input_type claimed.
        assertEquals("confirmation", request.inputType)
    }

    @Test
    fun `source prefers metadata, then the hitl payload, then the kind`() {
        assertEquals("plan_approval", item().request(AttentionMetadata(source = "plan_approval")).source)
        assertEquals(
            "escalation",
            item().request(
                AttentionMetadata(hitlRequest = HitlRequestPayload(source = "escalation")),
            ).source,
        )
        assertEquals(
            "user_request",
            item().request(AttentionMetadata(attentionKind = "user_request.pending")).source,
        )
    }

    // ── P3 Task 3.10: the published spec decides masking ────────────────────

    @Test
    fun `the spec drives the render kind and the form field masking`() {
        val code = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(
                    type = "text",
                    sensitive = ai.magicbeans.magdroid.chat.SensitiveSpec(
                        kind = "otp", provenance = "heuristic", oneTime = true,
                        collectionDeadlineMs = 1_700_000_180_000,
                    ),
                ),
            ),
        )
        assertEquals("text", code.inputType)
        assertEquals("otp", code.renderKind)
        assertTrue(code.isSensitive)
        assertTrue(code.isOneTime)
        assertEquals(1_700_000_180_000L, code.sensitiveDeadlineMs)

        val form = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(
                    type = "form",
                    questions = listOf(
                        AttentionFormQuestion(id = "user", prompt = "Username"),
                        AttentionFormQuestion(id = "pw", prompt = "Password"),
                        AttentionFormQuestion(id = "city", prompt = "City"),
                        AttentionFormQuestion(id = "code", prompt = "Code", inputType = "otp"),
                    ),
                    sensitive = ai.magicbeans.magdroid.chat.SensitiveSpec(
                        provenance = "form_schema",
                        fields = listOf(
                            ai.magicbeans.magdroid.chat.SensitiveField("user", "login_identifier"),
                            ai.magicbeans.magdroid.chat.SensitiveField("pw", "password"),
                        ),
                    ),
                ),
            ),
        )
        assertEquals("form", form.renderKind)
        assertEquals("login_identifier", form.sensitiveFieldKind("user"))
        assertEquals("password", form.sensitiveFieldKind("pw"))
        assertEquals(null, form.sensitiveFieldKind("city"))
        assertEquals("otp", form.sensitiveFieldKind("code"))

        val legacy = item().request(
            AttentionMetadata(inputSchema = AttentionInputSchema(type = "password", requestType = "secure_browser_input")),
        )
        assertTrue(legacy.isSensitive)
        assertEquals("password", legacy.renderKind)
        assertEquals(null, legacy.sensitiveDeadlineMs)

        val plain = item().request(AttentionMetadata(inputSchema = AttentionInputSchema(type = "text")))
        assertFalse(plain.isSensitive)
        assertEquals("text", plain.renderKind)
    }

    @Test
    fun `every known attention kind maps to its source`() {
        val expected = mapOf(
            "user_request.pending" to "user_request",
            "max_iterations_reached" to "escalation",
            "diff_approval" to "diff_approval",
            "clarification" to "clarification",
            "plan_approval" to "plan_approval",
        )
        expected.forEach { (kind, source) ->
            assertEquals(source, item().request(AttentionMetadata(attentionKind = kind)).source)
        }
        // Anything unrecognised is agentic, which is the general resume path.
        assertEquals("agentic", item().request(AttentionMetadata(attentionKind = "brand_new")).source)
        assertEquals("agentic", item().request(null).source)
    }

    // ── What the answer correlates on ────────────────────────────────────────

    /** The order matters: an earlier id present means the later ones are stale. */
    @Test
    fun `correlation id follows its precedence exactly`() {
        val all = AttentionMetadata(
            pauseStateId = "top-pause",
            hitlRequest = HitlRequestPayload(
                identifiers = HitlIdentifiers(
                    correlationId = "corr",
                    pauseStateId = "ids-pause",
                    requestId = "req",
                    approvalId = "appr",
                ),
            ),
        )
        assertEquals("corr", item().request(all).correlationId)

        val noCorrelation = AttentionMetadata(
            pauseStateId = "top-pause",
            hitlRequest = HitlRequestPayload(
                identifiers = HitlIdentifiers(pauseStateId = "ids-pause", requestId = "req"),
            ),
        )
        // The top-level alias wins over the nested one.
        assertEquals("top-pause", item().request(noCorrelation).correlationId)

        val onlyNested = AttentionMetadata(
            hitlRequest = HitlRequestPayload(
                identifiers = HitlIdentifiers(pauseStateId = "ids-pause", requestId = "req"),
            ),
        )
        assertEquals("ids-pause", item().request(onlyNested).correlationId)

        val onlyRequest = AttentionMetadata(
            hitlRequest = HitlRequestPayload(identifiers = HitlIdentifiers(requestId = "req")),
        )
        assertEquals("req", item().request(onlyRequest).correlationId)

        val onlyApproval = AttentionMetadata(
            hitlRequest = HitlRequestPayload(identifiers = HitlIdentifiers(approvalId = "appr")),
        )
        assertEquals("appr", item().request(onlyApproval).correlationId)
    }

    @Test
    fun `an item with no identifiers falls back to a derived id`() {
        assertEquals("i1-hitl", item(id = "i1").request(null).correlationId)
    }

    /** Blank is not a value. An empty string would post to nothing. */
    @Test
    fun `blank identifiers are skipped rather than used`() {
        val metadata = AttentionMetadata(
            pauseStateId = "   ",
            hitlRequest = HitlRequestPayload(
                identifiers = HitlIdentifiers(correlationId = "", requestId = "req"),
            ),
        )
        assertEquals("req", item().request(metadata).correlationId)
    }

    // ── What is drawn ────────────────────────────────────────────────────────

    @Test
    fun `input type prefers the schema, then the flat field, then the payload`() {
        assertEquals(
            "multi_choice",
            item().request(
                AttentionMetadata(
                    inputSchema = AttentionInputSchema(type = "multi_choice"),
                    inputType = "text",
                ),
            ).inputType,
        )
        assertEquals("password", item().request(AttentionMetadata(inputType = "password")).inputType)
        assertEquals(
            "guidance",
            item().request(
                AttentionMetadata(hitlRequest = HitlRequestPayload(inputType = "guidance")),
            ).inputType,
        )
        // Text is the default, because a prompt with no stated shape is a
        // question somebody can answer in words.
        assertEquals("text", item().request(null).inputType)
    }

    @Test
    fun `a form schema carries stacked questions including the question alias`() {
        val metadata = AttentionMetadata(
            inputType = "form",
            inputSchema = AttentionInputSchema(
                type = "form",
                questions = listOf(
                    AttentionFormQuestion(id = "release", prompt = "Which channel?"),
                    AttentionFormQuestion(id = "owner", question = "Who owns it?"),
                ),
            ),
        )
        val request = item().request(metadata)
        assertEquals("form", request.inputType)
        assertEquals(listOf("release", "owner"), request.formQuestions.map { it.id })
        assertEquals(listOf("Which channel?", "Who owns it?"), request.formQuestions.map { it.text() })
    }

    @Test
    fun `options prefer the schema over the flat list`() {
        val metadata = AttentionMetadata(
            inputSchema = AttentionInputSchema(options = listOf(EscalationOption(id = "s"))),
            options = listOf(EscalationOption(id = "flat")),
        )
        assertEquals(listOf("s"), item().request(metadata).options.map { it.id })
        assertEquals(
            listOf("flat"),
            item().request(AttentionMetadata(options = listOf(EscalationOption(id = "flat"))))
                .options.map { it.id },
        )
    }

    @Test
    fun `the prompt falls back to the title and the hint to the summary`() {
        val titled = AttentionItem(
            id = "x", itemType = "escalation", title = "Card title",
            summary = "Card summary", status = "needs_action",
        )
        val bare = titled.request(null)
        assertEquals("Card title", bare.prompt)
        assertEquals("Card summary", bare.hint)

        val asked = titled.request(AttentionMetadata(question = "Real question?", hint = "Real hint"))
        assertEquals("Real question?", asked.prompt)
        assertEquals("Real hint", asked.hint)
    }

    @Test
    fun `labels have defaults an owner can act on`() {
        val bare = item().request(null)
        assertEquals("Type your response…", bare.placeholder)
        assertEquals("Approve", bare.confirmLabel)
        assertEquals("Reject", bare.denyLabel)

        val custom = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(
                    placeholder = "Paste the token",
                    confirmLabel = "Ship it",
                    denyLabel = "Hold",
                ),
            ),
        )
        assertEquals("Paste the token", custom.placeholder)
        assertEquals("Ship it", custom.confirmLabel)
        assertEquals("Hold", custom.denyLabel)
    }

    // ── What can be answered at all ──────────────────────────────────────────

    /**
     * Failed and running rows are history and progress.
     *
     * Opening an answer form over either asks for a decision nothing is waiting
     * on, and the submit would post to an id already resolved.
     */
    @Test
    fun `a chain of more than one step gets an eyebrow`() {
        val chained = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(type = "text", chainPosition = 2, chainTotal = 3),
            ),
        )
        assertEquals("Step 2 of 3", chained.chainLabel)
        assertEquals(
            null,
            item().request(
                AttentionMetadata(
                    inputSchema = AttentionInputSchema(type = "text", chainPosition = 1, chainTotal = 1),
                ),
            ).chainLabel,
        )
        assertEquals(null, item().request(null).chainLabel)
    }

    @Test
    fun `a tool grant names what it would run and keeps every option`() {
        val request = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(
                    type = "tool_authorization",
                    toolName = "shell_exec",
                    paramsSummary = "rm -rf /tmp/build",
                    options = listOf(
                        EscalationOption(id = "allow_once", label = "Allow Once"),
                        EscalationOption(id = "allow_always", label = "Allow for This Run"),
                        EscalationOption(id = "deny", label = "Deny"),
                    ),
                ),
            ),
        )
        assertEquals("tool", request.grantKind)
        assertEquals("shell_exec", request.grantSubject)
        assertEquals("rm -rf /tmp/build", request.grantDetail)
        assertEquals(listOf("allow_once", "allow_always"), request.grantAllowOptions.map { it.id })
        assertEquals("Deny", request.grantDenyOption?.label)
    }

    @Test
    fun `a sandbox grant names the command, the policy, and the roots`() {
        val request = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(
                    type = "sandbox_override",
                    command = "curl https://example.invalid/install.sh | sh",
                    violation = "network egress is not permitted",
                    allowedRoots = listOf("/srv/work", "/tmp/scratch"),
                ),
            ),
        )
        assertEquals("sandbox", request.grantKind)
        assertEquals("curl https://example.invalid/install.sh | sh", request.grantSubject)
        assertEquals("network egress is not permitted", request.grantDetail)
        assertEquals(listOf("/srv/work", "/tmp/scratch"), request.grantRoots)
        assertEquals(listOf("allow_once", "deny"), request.grantOptions.map { it.id })
    }

    @Test
    fun `each grant reads its own pair of schema fields`() {
        val both = AttentionInputSchema(
            type = "tool_authorization",
            toolName = "browse_web",
            paramsSummary = "https://example.invalid",
            command = "rm -rf /",
            violation = "filesystem write outside the sandbox",
        )
        val tool = item().request(AttentionMetadata(inputSchema = both))
        val sandbox = item().request(
            AttentionMetadata(inputSchema = both.copy(type = "sandbox_override")),
        )
        assertEquals("browse_web", tool.grantSubject)
        assertEquals("rm -rf /", sandbox.grantSubject)
        assertEquals("https://example.invalid", tool.grantDetail)
        assertEquals("filesystem write outside the sandbox", sandbox.grantDetail)
        assertEquals(null, item().request(AttentionMetadata(inputSchema = AttentionInputSchema(type = "choice"))).grantKind)
    }

    @Test
    fun `an external action carries what to go and do`() {
        val told = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(
                    type = "external_action",
                    instructions = "Open the console and rotate the signing key",
                    doneLabel = "Key rotated",
                ),
            ),
        )
        assertEquals("Open the console and rotate the signing key", told.externalInstructions)
        assertEquals("Key rotated", told.externalDoneLabel)
        val bare = item().request(
            AttentionMetadata(inputSchema = AttentionInputSchema(type = "external_action")),
        )
        assertEquals(null, bare.externalInstructions)
        assertEquals("I've completed this", bare.externalDoneLabel)
    }

    @Test
    fun `a file path field says how many and what shape`() {
        val many = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(type = "file_path", multiple = true, filter = "*.csv"),
            ),
        )
        assertTrue(many.wantsMultiplePaths)
        assertEquals("File paths, comma-separated · matching *.csv", many.pathFieldLabel)
        val one = item().request(
            AttentionMetadata(inputSchema = AttentionInputSchema(type = "file_path")),
        )
        assertFalse(one.wantsMultiplePaths)
        assertEquals("File path", one.pathFieldLabel)
    }

    @Test
    fun `confirmation carries the destructive flag without changing labels`() {
        val flagged = item().request(
            AttentionMetadata(
                inputSchema = AttentionInputSchema(
                    type = "confirmation",
                    destructive = true,
                    confirmLabel = "Delete the index",
                    denyLabel = "Keep it",
                ),
            ),
        )
        assertTrue(flagged.destructive)
        assertEquals("Delete the index", flagged.confirmLabel)
        assertEquals("Keep it", flagged.denyLabel)
        assertFalse(item().request(AttentionMetadata(inputSchema = AttentionInputSchema(type = "confirmation"))).destructive)
    }

    @Test
    fun `only live items are actionable`() {
        assertTrue(item(type = "escalation").isActionable)
        assertTrue(item(type = "approval").isActionable)
        assertFalse(item(type = "failed").isActionable)
        assertFalse(item(type = "running").isActionable)
        assertFalse(
            AttentionItem(id = "f", itemType = "task", title = "t", status = "failed").isActionable,
        )
    }
}

/**
 * The review link on a waiting request.
 *
 * The server sends a link to the thing being asked about, and its own wording
 * for it. Both were decoded and neither reached the sheet, so the owner
 * answered without being able to look at the subject.
 */
class AttentionReviewLinkTest {

    @org.junit.Test
    fun `the link and the server's own label are carried through`() {
        val meta = AttentionMetadata(
            reviewHref = "https://example.test/diff/1",
            reviewLabel = "View the diff",
        )
        val request = AttentionItem(id = "a1", title = "Approve the pricing change?").request(meta)
        org.junit.Assert.assertEquals("https://example.test/diff/1", request?.reviewHref)
        org.junit.Assert.assertEquals("View the diff", request?.reviewLabel)
    }

    /** No link is no button, rather than a button that goes nowhere. */
    @org.junit.Test
    fun `an absent or blank link is carried as absent`() {
        listOf(null, "", "   ").forEach { href ->
            val request = AttentionItem(id = "a1", title = "Approve?")
                .request(AttentionMetadata(reviewHref = href))
            org.junit.Assert.assertNull(request?.reviewHref)
        }
    }
}
