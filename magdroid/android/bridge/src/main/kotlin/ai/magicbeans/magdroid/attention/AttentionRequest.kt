package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.EscalationOption
import ai.magicbeans.magdroid.chat.SensitiveSpec
import ai.magicbeans.magdroid.chat.hitlRenderKind
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

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
)

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
)

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
}

@Serializable
data class HitlRequestPayload(
    val identifiers: HitlIdentifiers? = null,
    val source: String? = null,
    @SerialName("input_type") val inputType: String? = null,
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
) {
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
        else -> when (metadata?.attentionKind) {
            "user_request.pending" -> "user_request"
            "max_iterations_reached" -> "escalation"
            "diff_approval" -> "diff_approval"
            "clarification" -> "clarification"
            "plan_approval" -> "plan_approval"
            else -> "agentic"
        }
    }

    val ids = metadata?.hitlRequest?.identifiers
    val correlationId = ids?.correlationId.orNull()
        ?: metadata?.pauseStateId.orNull()
        ?: ids?.pauseStateId.orNull()
        ?: ids?.requestId.orNull()
        ?: ids?.approvalId.orNull()
        // Last resort, and the same one the other clients use. An item with no
        // stamped identifiers is answered against a derived id rather than not
        // at all.
        ?: "$id-hitl"

    val inputType = when {
        approval -> "confirmation"
        else -> metadata?.inputSchema?.type.orNull()
            ?: metadata?.inputType.orNull()
            ?: metadata?.hitlRequest?.inputType.orNull()
            ?: "text"
    }

    val schema = metadata?.inputSchema
    val options = schema?.options ?: metadata?.options ?: emptyList()
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
        prompt = metadata?.question.orNull() ?: title,
        hint = metadata?.hint.orNull() ?: summary.orNull(),
        reviewHref = metadata?.reviewHref.orNull(),
        reviewLabel = metadata?.reviewLabel.orNull(),
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
        externalInstructions = schema?.instructions.orNull(),
        externalDoneLabel = schema?.doneLabel.orNull() ?: "I've completed this",
        wantsMultiplePaths = wantsMultiple,
        pathFieldLabel = buildString {
            append(if (wantsMultiple) "File paths, comma-separated" else "File path")
            if (pathFilter != null) append(" · matching $pathFilter")
        },
        sensitive = schema?.sensitive,
        legacySecure = schema?.requestType == "secure_browser_input" ||
            schema?.requestType == "secure_browser_confirm",
    )
}

/**
 * Whether this item is something the owner can answer.
 *
 * Failed and running rows are history and progress respectively. Opening an
 * answer form over either would ask for a decision nothing is waiting on.
 */
val AttentionItem.isActionable: Boolean
    get() = itemType != "failed" && itemType != "running" && status != "failed"
