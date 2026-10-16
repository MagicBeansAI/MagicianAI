package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.EscalationOption
import ai.magicbeans.magdroid.chat.SensitiveSpec
import ai.magicbeans.magdroid.chat.hitlRenderKind
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * The HITL contract a feed item carries in its `metadata`.
 *
 * Nothing about answering an attention item is at the top level: which endpoint
 * source to post as, which id to correlate on, what kind of input to draw and
 * what the options are all live in this free-form object, stamped by whichever
 * subsystem raised the item.
 *
 * Mirrors the web's `hitlRequestFromFeedItem` and iOS's derived properties. All
 * three read the same object, and disagreeing about a fallback would mean an
 * answer posted to an id the server is not waiting on.
 */
@Serializable
data class AttentionMetadata(
    val source: String? = null,
    @SerialName("attention_kind") val attentionKind: String? = null,
    @SerialName("input_type") val inputType: String? = null,
    @SerialName("input_schema") val inputSchema: AttentionInputSchema? = null,
    val question: String? = null,
    val hint: String? = null,
    val options: List<EscalationOption>? = null,
    @SerialName("pause_state_id") val pauseStateId: String? = null,
    @SerialName("hitl_request") val hitlRequest: HitlRequestPayload? = null,
    @SerialName("review_href") val reviewHref: String? = null,
    @SerialName("review_label") val reviewLabel: String? = null,
    /** The execution that is paused — for a delegated child, the child. */
    @SerialName("execution_id") val executionId: String? = null,
    @SerialName("task_id") val taskId: String? = null,
    /**
     * Set on a delegated child's ask surfaced on its parent task (C2). Kept
     * as raw JSON and read leniently: a type the decoder did not expect
     * must never fail the whole feed and hide every other ask with it.
     */
    @SerialName("delegation_child") val delegationChildRaw: JsonElement? = null,
    @SerialName("delegated_agent_id") val delegatedAgentId: String? = null,
    /** Why an item is here, when it is not an ask (C3 `waiting_without_question`). */
    @SerialName("attention_reason") val attentionReason: String? = null,
    /** The verbs an item offers, e.g. `["cancel", "open_task"]`. Raw for the same reason. */
    @SerialName("actions") val actionsRaw: JsonElement? = null,
) {
    val delegationChild: Boolean
        get() = when (val raw = delegationChildRaw) {
            is JsonPrimitive -> raw.content.equals("true", ignoreCase = true)
            else -> false
        }

    val actionIds: List<String>
        get() = (actionsRaw as? JsonArray).orEmpty().mapNotNull { element ->
            (element as? JsonPrimitive)?.takeIf { it.isString }?.content?.trim()?.takeIf { it.isNotEmpty() }
        }
}

@Serializable
data class AttentionInputSchema(
    val type: String? = null,
    val options: List<EscalationOption>? = null,
    val placeholder: String? = null,
    @SerialName("confirm_label") val confirmLabel: String? = null,
    @SerialName("deny_label") val denyLabel: String? = null,
    /** Several questions in one pause (`input_type=form`). */
    val questions: List<AttentionFormQuestion>? = null,
    @SerialName("allow_other") val allowOther: Boolean? = null,
    @SerialName("chain_id") val chainId: String? = null,
    @SerialName("chain_position") val chainPosition: Int? = null,
    @SerialName("chain_total") val chainTotal: Int? = null,
    val destructive: Boolean? = null,
    val instructions: String? = null,
    @SerialName("done_label") val doneLabel: String? = null,
    val multiple: Boolean? = null,
    val filter: String? = null,
    @SerialName("tool_name") val toolName: String? = null,
    @SerialName("params_summary") val paramsSummary: String? = null,
    val command: String? = null,
    val violation: String? = null,
    @SerialName("allowed_roots") val allowedRoots: List<String>? = null,
    /** The backend's classification when the ask collects a secret. */
    val sensitive: SensitiveSpec? = null,
    /** The service request type; the built-in secure browser asks predate the spec. */
    @SerialName("request_type") val requestType: String? = null,
    /** Where an external action happens — an `mcp_oauth` ask's authorize page (C4). */
    val url: String? = null,
    @SerialName("authorize_url") val authorizeUrl: String? = null,
    @SerialName("authorization_url") val authorizationUrl: String? = null,
    /**
     * Free-form context the asking subsystem attached. An object for a
     * Relentless review (`review_href`, `review_label`, `task_id`), but a
     * plain sentence on a tool authorization — so kept raw and read leniently.
     */
    val context: JsonElement? = null,
    /** `preference_alignment` for a hold against the owner's preferences. */
    @SerialName("escalation_trigger") val escalationTrigger: String? = null,
    /** A hold's public projection, kept raw and read leniently ([parseAlignmentProjection]). */
    @SerialName("alignment") val alignmentRaw: JsonElement? = null,
) {
    fun externalUrl(): String? = listOf(url, authorizeUrl, authorizationUrl)
        .firstNotNullOfOrNull { it?.trim()?.takeIf(String::isNotEmpty) }

    /** One string field of [context], when it is an object that carries it. */
    fun contextString(key: String): String? =
        ((context as? JsonObject)?.get(key) as? JsonPrimitive)
            ?.takeIf { it.isString }?.content?.trim()?.takeIf(String::isNotEmpty)
}

/** One grant on offer. Separate from [EscalationOption] so a fallback pair can be built. */
data class AttentionGrantOption(
    val id: String,
    val label: String,
)

@Serializable
data class AttentionFormQuestion(
    val id: String,
    val prompt: String? = null,
    val question: String? = null,
    @SerialName("input_type") val inputType: String? = null,
    val options: List<EscalationOption>? = null,
) {
    fun text(): String = prompt?.trim()?.takeIf { it.isNotEmpty() } ?: question.orEmpty().trim()

    /** Answered by picking ids rather than typing: `choice` / `multi_choice` with options. */
    val isChoice: Boolean
        get() = !options.isNullOrEmpty() && (inputType == "choice" || inputType == "multi_choice")
    val isMultiChoice: Boolean get() = isChoice && inputType == "multi_choice"
}

/**
 * The complete prompt a projection carries (`HitlOpenTarget` on the server).
 *
 * C2 rows — a delegated child's ask on the parent task — may carry the ask only
 * here, so every field is read as a fallback for the item's own metadata.
 */
@Serializable
data class HitlRequestPayload(
    val identifiers: HitlIdentifiers? = null,
    val source: String? = null,
    @SerialName("input_type") val inputType: String? = null,
    /** The canonical `/hitl/{id}/respond` key in `HitlOpenTarget`. */
    val id: String? = null,
    @SerialName("correlation_id") val correlationId: String? = null,
    @SerialName("execution_id") val executionId: String? = null,
    val prompt: String? = null,
    val hint: String? = null,
    @SerialName("input_schema") val inputSchema: AttentionInputSchema? = null,
    val scope: HitlRequestScope? = null,
)

@Serializable
data class HitlRequestScope(
    @SerialName("execution_id") val executionId: String? = null,
    @SerialName("task_id") val taskId: String? = null,
    @SerialName("agent_id") val agentId: String? = null,
)

@Serializable
data class HitlIdentifiers(
    @SerialName("correlation_id") val correlationId: String? = null,
    @SerialName("pause_state_id") val pauseStateId: String? = null,
    @SerialName("request_id") val requestId: String? = null,
    @SerialName("approval_id") val approvalId: String? = null,
)

/** Everything needed to draw an answer form and post what it collects. */
data class AttentionRequest(
    val source: String,
    val correlationId: String,
    val inputType: String,
    val options: List<EscalationOption>,
    val prompt: String,
    val hint: String?,
    /**
     * A link to the thing being asked about, with the server's own wording.
     *
     * Both were decoded and neither was carried through to the sheet, so
     * answering meant deciding without being able to look at the subject.
     */
    val reviewHref: String?,
    val reviewLabel: String?,
    val placeholder: String,
    val confirmLabel: String,
    val denyLabel: String,
    /** Present when `inputType` is `form`. Empty for every other shape. */
    val formQuestions: List<AttentionFormQuestion> = emptyList(),
    /** Batched-clarification eyebrow, e.g. "Step 2 of 3". Null when not chained. */
    val chainLabel: String? = null,
    /** Bands the confirmation control. Does not change the posted value. */
    val destructive: Boolean = false,
    /** `"tool"` / `"sandbox"`, or null when this is not a grant. */
    val grantKind: String? = null,
    val grantSubject: String = "",
    val grantDetail: String? = null,
    val grantRoots: List<String> = emptyList(),
    val grantOptions: List<AttentionGrantOption> = emptyList(),
    val externalInstructions: String? = null,
    val externalDoneLabel: String = "I've completed this",
    val wantsMultiplePaths: Boolean = false,
    val pathFieldLabel: String = "File path",
    /** The backend's value-free spec, when this ask collects a secret. */
    val sensitive: SensitiveSpec? = null,
    /** A built-in secure browser ask announced before the spec existed. */
    val legacySecure: Boolean = false,
    /**
     * The paused execution, posted back as `execution_id`. For a delegated
     * child's ask (C2) this is the child, not the parent task's run.
     */
    val executionId: String? = null,
    /** The delegated agent that asked, when the ask came from a child run. */
    val delegatedAgentId: String? = null,
    /** A page to open for an external action — an `mcp_oauth` authorize URL (C4). */
    val externalUrl: String? = null,
    /** The service request type, e.g. `relentless.accepted_review`. */
    val requestType: String? = null,
    /** A preference alignment hold's public projection (P6.3), on a confirmation only. */
    val alignment: AlignmentProjection? = null,
) {
    /**
     * A two-way decision offered as a choice: `reject` and `approve` (the
     * Relentless review's "Reject draft" / "Approve draft"). Drawn as a
     * Reject / Approve pair rather than a list, and posted as the chosen id.
     * Null for any other option set.
     */
    val decisionPair: Pair<EscalationOption, EscalationOption>?
        get() {
            if (inputType != "choice" || options.size != 2) return null
            val reject = options.firstOrNull { it.id.equals("reject", ignoreCase = true) } ?: return null
            val approve = options.firstOrNull { it.id.equals("approve", ignoreCase = true) } ?: return null
            return reject to approve
        }

    /** "from <agent>", for an ask raised by a delegated child. */
    val fromLabel: String? get() = delegatedAgentId?.let { "from $it" }

    val grantDenyOption: AttentionGrantOption?
        get() = grantOptions.firstOrNull { it.id == "deny" }
    val grantAllowOptions: List<AttentionGrantOption>
        get() = grantOptions.filter { it.id != "deny" }

    /** Whether the ask collects a secret: the spec, or the legacy secure types. */
    val isSensitive: Boolean get() = sensitive != null || legacySecure

    /** How the single-value field renders: `otp`, `password`, or the widget type. */
    val renderKind: String get() = hitlRenderKind(inputType, sensitive?.kind) ?: inputType

    /**
     * The flagged kind of one form field — the spec first, then a question
     * typed `password`/`otp` for an entry announced before the spec existed.
     */
    fun sensitiveFieldKind(questionId: String): String? {
        sensitive?.fieldKind(questionId)?.let { return it }
        val typed = formQuestions.firstOrNull { it.id == questionId }?.inputType
        return typed?.takeIf { it == "password" || it == "otp" }
    }

    val isOneTime: Boolean get() = sensitive?.oneTime == true
    val sensitiveDeadlineMs: Long? get() = sensitive?.collectionDeadlineMs?.takeIf { it > 0 }
}

private fun String?.orNull(): String? = this?.trim()?.takeIf { it.isNotEmpty() }

/** A source whose answer is an external action rather than typed input. */
const val MCP_OAUTH_SOURCE = "mcp_oauth"

/**
 * Input types the answer sheet has a renderer for. An unknown type is drawn
 * as a pick among its options, and with no options that would be a sheet with
 * nothing to press — so an unknown, option-less type is answered in words.
 */
private val KNOWN_INPUT_TYPES = setOf(
    "text", "guidance", "password", "otp", "file_path",
    "tool_authorization", "sandbox_override", "external_action", "confirmation",
    "choice", "multi_choice", "diff_approval", "form",
)

/**
 * Read the answer contract off an item.
 *
 * Every field is a fallback chain because the object is written by several
 * subsystems that each stamp what they know. Getting one wrong does not fail
 * loudly — it posts a well-formed answer to something that is not waiting for
 * it, and the execution stays paused. That is why this is pure and tested
 * rather than read inline by the sheet that draws it.
 */
fun AttentionItem.request(metadata: AttentionMetadata?): AttentionRequest {
    val approval = itemType == "approval"

    val source = when {
        // An approval posts as an approval whatever else the metadata says.
        approval -> "approval"
        metadata?.source.orNull() != null -> metadata!!.source!!.trim()
        metadata?.hitlRequest?.source.orNull() != null -> metadata!!.hitlRequest!!.source!!.trim()
        metadata?.attentionKind == MCP_OAUTH_SOURCE -> MCP_OAUTH_SOURCE
        else -> when (metadata?.attentionKind) {
            "user_request.pending" -> "user_request"
            "max_iterations_reached" -> "escalation"
            "diff_approval" -> "diff_approval"
            "clarification" -> "clarification"
            "plan_approval" -> "plan_approval"
            else -> "agentic"
        }
    }

    val hitl = metadata?.hitlRequest
    val ids = hitl?.identifiers
    val correlationId = ids?.correlationId.orNull()
        ?: hitl?.correlationId.orNull()
        ?: metadata?.pauseStateId.orNull()
        ?: ids?.pauseStateId.orNull()
        ?: hitl?.id.orNull()
        ?: ids?.requestId.orNull()
        ?: ids?.approvalId.orNull()
        // Last resort, and the same one the other clients use. An item with no
        // stamped identifiers is answered against a derived id rather than not
        // at all.
        ?: "$id-hitl"

    // A child's ask can arrive with its schema only inside `hitl_request`.
    val schema = metadata?.inputSchema ?: hitl?.inputSchema
    val options = schema?.options ?: metadata?.options ?: hitl?.inputSchema?.options ?: emptyList()
    val oauth = source == MCP_OAUTH_SOURCE
    val externalUrl = schema?.externalUrl() ?: hitl?.inputSchema?.externalUrl()

    val declaredType = when {
        approval -> "confirmation"
        // Connecting a service is done in the browser, whatever widget the
        // payload named: the answer is only ever "I've finished".
        oauth -> "external_action"
        else -> schema?.type.orNull()
            ?: metadata?.inputType.orNull()
            ?: hitl?.inputType.orNull()
            ?: "text"
    }
    // Nothing waits silently behind an empty sheet: a type this build cannot
    // draw, or an option list that arrived empty, falls back to a typed
    // answer the owner can always give.
    val inputType = if (declaredType !in KNOWN_INPUT_TYPES && options.isEmpty()) "text" else declaredType
    val grantKind = when (inputType) {
        "tool_authorization" -> "tool"
        "sandbox_override" -> "sandbox"
        else -> null
    }
    val grantSubject = when (grantKind) {
        "sandbox" -> schema?.command
        "tool" -> schema?.toolName
        else -> null
    }.orEmpty()
    val grantDetail = when (grantKind) {
        "sandbox" -> schema?.violation
        "tool" -> schema?.paramsSummary
        else -> null
    }.orNull()
    val chainTotal = schema?.chainTotal
    val chainPosition = schema?.chainPosition
    val wantsMultiple = schema?.multiple == true
    val pathFilter = schema?.filter.orNull()

    return AttentionRequest(
        source = source,
        correlationId = correlationId,
        inputType = inputType,
        options = options,
        prompt = metadata?.question.orNull() ?: hitl?.prompt.orNull() ?: title,
        hint = metadata?.hint.orNull() ?: hitl?.hint.orNull() ?: summary.orNull(),
        // The top-level stamp first, then the ask's own context: a Relentless
        // review carries its Work Quality link only in `input_schema.context`.
        reviewHref = metadata?.reviewHref.orNull()
            ?: schema?.contextString("review_href")
            ?: hitl?.inputSchema?.contextString("review_href"),
        reviewLabel = metadata?.reviewLabel.orNull()
            ?: schema?.contextString("review_label")
            ?: hitl?.inputSchema?.contextString("review_label"),
        placeholder = schema?.placeholder.orNull() ?: "Type your response…",
        confirmLabel = schema?.confirmLabel.orNull() ?: "Approve",
        denyLabel = schema?.denyLabel.orNull() ?: "Reject",
        formQuestions = schema?.questions.orEmpty(),
        chainLabel = if (chainPosition != null && chainTotal != null && chainTotal > 1) {
            "Step $chainPosition of $chainTotal"
        } else {
            null
        },
        destructive = schema?.destructive == true,
        grantKind = grantKind,
        grantSubject = grantSubject,
        grantDetail = grantDetail,
        grantRoots = schema?.allowedRoots.orEmpty(),
        grantOptions = when {
            grantKind == null -> emptyList()
            options.isEmpty() -> listOf(
                AttentionGrantOption("allow_once", "Allow once"),
                AttentionGrantOption("deny", "Deny"),
            )
            else -> options.map { AttentionGrantOption(it.id, it.label.ifBlank { it.id }) }
        },
        externalInstructions = schema?.instructions.orNull() ?: if (oauth) {
            "Sign in to connect the service in your browser, then come back and tap I've finished."
        } else {
            null
        },
        externalDoneLabel = schema?.doneLabel.orNull() ?: if (oauth) "I've finished" else "I've completed this",
        wantsMultiplePaths = wantsMultiple,
        pathFieldLabel = buildString {
            append(if (wantsMultiple) "File paths, comma-separated" else "File path")
            if (pathFilter != null) append(" · matching $pathFilter")
        },
        sensitive = schema?.sensitive,
        legacySecure = schema?.requestType == "secure_browser_input" ||
            schema?.requestType == "secure_browser_confirm",
        executionId = hitl?.executionId.orNull()
            ?: hitl?.scope?.executionId.orNull()
            ?: metadata?.executionId.orNull()
            ?: executionId.orNull(),
        delegatedAgentId = if (metadata?.delegationChild == true || metadata?.delegatedAgentId.orNull() != null) {
            metadata?.delegatedAgentId.orNull() ?: hitl?.scope?.agentId.orNull() ?: agentId.orNull() ?: "a delegated agent"
        } else {
            null
        },
        externalUrl = externalUrl,
        requestType = schema?.requestType.orNull() ?: hitl?.inputSchema?.requestType.orNull(),
        alignment = if (inputType == "confirmation") {
            parseAlignmentProjection(schema?.alignmentRaw ?: hitl?.inputSchema?.alignmentRaw)
        } else {
            null
        },
    )
}

/**
 * A run waiting on the owner with no question delivered (C3).
 *
 * Not an ask: there is nothing to answer, so opening the answer sheet over it
 * would post to a pause that does not exist. It offers cancel and open-task.
 */
val AttentionItem.isStalledWait: Boolean
    get() = metadata?.attentionReason == "waiting_without_question"

/** The run a stalled-wait card cancels. */
val AttentionItem.stalledExecutionId: String?
    get() = metadata?.executionId.orNull()
        ?: metadata?.hitlRequest?.scope?.executionId.orNull()
        ?: executionId.orNull()

/** The task a card opens — the item's own, else the one its metadata names. */
val AttentionItem.openTaskId: String?
    get() = taskId.orNull() ?: metadata?.taskId.orNull() ?: metadata?.hitlRequest?.scope?.taskId.orNull()

/** Whether a stalled-wait card offers a verb; absent `actions` means both. */
fun AttentionItem.offers(action: String): Boolean {
    val listed = metadata?.actionIds.orEmpty()
    return listed.isEmpty() || action in listed
}

/**
 * Whether this item is something the owner can answer.
 *
 * Failed and running rows are history and progress respectively. Opening an
 * answer form over either would ask for a decision nothing is waiting on.
 */
val AttentionItem.isActionable: Boolean
    get() = itemType != "failed" && itemType != "running" && status != "failed" && !isStalledWait
