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
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

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
    /** The server's respond handle, raw and read leniently ([HitlRespondHandle.parse]). */
    @SerialName("respond") val respondRaw: JsonElement? = null,
) {
    /** The server's respond handle (`metadata.respond`), when it sent a usable one. */
    val respond: HitlRespondHandle? get() = HitlRespondHandle.parse(respondRaw)

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
    /** `mcp_oauth` — the decline control's label ("Not now"). */
    @SerialName("dismiss_label") val dismissLabel: String? = null,
    /** `mcp_oauth` — the service being connected ("Swiggy (food)"). */
    @SerialName("service_label") val serviceLabel: String? = null,
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
    /**
     * `preference_alignment` for a hold against the owner's preferences,
     * `outward_review` for a captured send waiting for review.
     */
    @SerialName("escalation_trigger") val escalationTrigger: String? = null,
    /** A hold's public projection, kept raw and read leniently ([parseAlignmentProjection]). */
    @SerialName("alignment") val alignmentRaw: JsonElement? = null,
    /** A confirmation's structured approval card, raw and read leniently ([ApprovalPreview.parse]). */
    @SerialName("approval_preview") val approvalPreviewRaw: JsonElement? = null,
    /** The raw call a confirmation gates; raw, since producers disagree on its shape. */
    @SerialName("action_summary") val actionSummaryRaw: JsonElement? = null,
    @SerialName("reason") val reasonRaw: JsonElement? = null,
    /** A captured send's review record, raw and read leniently ([parseOutwardReview]). */
    @SerialName("outward_review") val outwardReviewRaw: JsonElement? = null,
) {
    fun externalUrl(): String? = listOf(url, authorizeUrl, authorizationUrl)
        .firstNotNullOfOrNull { it?.trim()?.takeIf(String::isNotEmpty) }

    /** One string field of [context], when it is an object that carries it. */
    fun contextString(key: String): String? =
        ((context as? JsonObject)?.get(key) as? JsonPrimitive)
            ?.takeIf { it.isString }?.content?.trim()?.takeIf(String::isNotEmpty)
}

@Serializable
data class AttentionFileDiff(
    val path: String,
    @SerialName("unified_diff") val unifiedDiff: String = "",
)

/** One grant on offer. Separate from [EscalationOption] so a fallback pair can be built. */
data class AttentionGrantOption(
    val id: String,
    val label: String,
)

/** A pair of exact server option ids; grouping changes presentation only. */
data class GodGrantScope(val id: String, val label: String, val detail: String, val sessionId: String, val alwaysId: String)

fun godGrantScopes(options: List<AttentionGrantOption>): List<GodGrantScope> {
    val lasting = options.filter { it.id != "allow_once" && it.id != "deny" }
    if (lasting.isEmpty() || lasting.any { !it.id.matches(Regex("^(session|always):[^:]+$")) }) return emptyList()
    val labels = mapOf("file" to "These files", "folder" to "Containing folders", "top" to "Top directories", "repository" to "This repository",
        "exact" to "Exact command", "mcp_tool" to "This action", "capability" to "Whole tool", "prefix" to "Command prefix", "program" to "Program", "host" to "This host",
        "any_host" to "Any host", "engine_repo" to "Engine and repository", "engine" to "This engine", "any_harness" to "Any harness")
    val scopes = lasting.filter { it.id.startsWith("session:") }.map { option ->
        val id = option.id.removePrefix("session:")
        val always = lasting.firstOrNull { it.id == "always:$id" } ?: return emptyList()
        GodGrantScope(id, labels[id] ?: id, option.label.removePrefix("This session: "), option.id, always.id)
    }
    return scopes.takeIf { it.size * 2 == lasting.size } ?: emptyList()
}

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
    /** The server's respond handle for this target, raw ([HitlRespondHandle.parse]). */
    @SerialName("respond") val respondRaw: JsonElement? = null,
) {
    val respond: HitlRespondHandle? get() = HitlRespondHandle.parse(respondRaw)
}

/**
 * How to answer one ask, decided by the server (Rust `HitlRespondHandle`).
 *
 * Rides as `metadata.respond` (and `metadata.hitl_request.respond`) on every
 * Attention row that offers an answer. When present it is the whole route:
 * `POST {url}` with its `source`, `input_type`, `execution_id` and `task_id`;
 * [request]'s per-source derivation answers only for servers that predate it.
 */
data class HitlRespondHandle(
    val url: String,
    val correlationId: String,
    val source: String,
    val inputType: String,
    val executionId: String? = null,
    val taskId: String? = null,
    val optionIds: List<String> = emptyList(),
    val softSuccessStatuses: List<Int> = listOf(409),
) {
    companion object {
        const val PATH_PREFIX = "/api/magician/v2/hitl/"

        /** A usable handle, or null: unknown shapes and off-route URLs fall back. */
        fun parse(raw: JsonElement?): HitlRespondHandle? {
            val obj = raw as? JsonObject ?: return null
            fun text(key: String): String? =
                (obj[key] as? JsonPrimitive)?.takeIf { it.isString }?.content?.trim()?.takeIf { it.isNotEmpty() }
            val url = text("url") ?: return null
            if (!url.startsWith(PATH_PREFIX) || !url.endsWith("/respond")) return null
            if (url.any { it == '?' || it == '#' || it == '\\' || it.isWhitespace() }) return null
            val statuses = (obj["soft_success_statuses"] as? JsonArray).orEmpty()
                .mapNotNull { (it as? JsonPrimitive)?.content?.toIntOrNull() }
            return HitlRespondHandle(
                url = url,
                correlationId = text("correlation_id") ?: return null,
                source = text("source") ?: return null,
                inputType = text("input_type") ?: return null,
                executionId = text("execution_id"),
                taskId = text("task_id"),
                optionIds = (obj["options"] as? JsonArray).orEmpty().mapNotNull { option ->
                    ((option as? JsonObject)?.get("id") as? JsonPrimitive)?.content?.trim()?.takeIf { it.isNotEmpty() }
                },
                softSuccessStatuses = statuses.ifEmpty { listOf(409) },
            )
        }
    }
}

/** One answer, ready to post: the path below the host and the JSON body. */
data class HitlRespondCall(
    val path: String,
    val body: JsonObject,
    val softSuccessStatuses: List<Int>,
)

/** `encodeURIComponent`, so a derived path matches the one the server names. */
fun encodeHitlPathSegment(segment: String): String = buildString {
    for (byte in segment.toByteArray(Charsets.UTF_8)) {
        val c = byte.toInt() and 0xff
        val ch = c.toChar()
        if (ch in 'A'..'Z' || ch in 'a'..'z' || ch in '0'..'9' || ch in "-_.!~*'()") {
            append(ch)
        } else {
            append('%').append("%02X".format(c))
        }
    }
}

/**
 * The call that posts [value]: the server's handle when it sent one, else the
 * per-source derivation (web `resolveHitlCall` parity). [taskId] is the
 * caller's planning task, posted only for `clarification` / `plan_approval`
 * on a derived call; a handle names its own.
 */
fun AttentionRequest.respondCall(
    value: JsonElement,
    channel: String = "android",
    taskId: String? = null,
): HitlRespondCall {
    val handle = respond
    val body = buildJsonObject {
        put("source", handle?.source ?: source)
        put("value", value)
        put("input_type", handle?.inputType ?: inputType)
        put("channel", channel)
        // The paused run. For a delegated child's ask (C2) this is the child,
        // which is the only run that can take the answer.
        (if (handle != null) handle.executionId else executionId)?.let { put("execution_id", it) }
        // Only the planning responders take a task; a handle names its own.
        val postedTaskId = if (handle != null) {
            handle.taskId
        } else {
            taskId.takeIf { source == "clarification" || source == "plan_approval" }
        }
        postedTaskId?.takeIf { it.isNotBlank() }?.let { put("task_id", it) }
    }
    if (handle != null) return HitlRespondCall(handle.url, body, handle.softSuccessStatuses)
    return HitlRespondCall(
        "${HitlRespondHandle.PATH_PREFIX}${encodeHitlPathSegment(correlationId)}/respond",
        body,
        if (source == "agentic" || source == "escalation") listOf(404, 409, 410) else listOf(409),
    )
}

@Serializable
data class HitlRequestScope(
    @SerialName("execution_id") val executionId: String? = null,
    @SerialName("task_id") val taskId: String? = null,
    @SerialName("agent_id") val agentId: String? = null,
    @SerialName("chat_session_id") val chatSessionId: String? = null,
    @SerialName("session_id") val sessionId: String? = null,
    @SerialName("thread_id") val threadId: String? = null,
    @SerialName("ui_thread_id") val uiThreadId: String? = null,
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
    val isGodMode: Boolean = false,
    val godChatSessionId: String? = null,
    val godOriginSession: String? = null,
    val godOriginAgent: String? = null,
    val godFiles: List<AttentionFileDiff> = emptyList(),
    val grantSubject: String = "",
    val grantDetail: String? = null,
    val grantRoots: List<String> = emptyList(),
    val grantOptions: List<AttentionGrantOption> = emptyList(),
    val externalInstructions: String? = null,
    val externalDoneLabel: String = "I've completed this",
    /**
     * The decline on an MCP sign-in ("Not now"); null when the ask offers none.
     * It posts an abort, which cancels the flow on the computer and closes the
     * ask everywhere. Every sign-in can be declined, so older asks get one too.
     */
    val externalDismissLabel: String? = null,
    /** The service an MCP sign-in connects ("Swiggy (food)"), when named. */
    val externalServiceLabel: String? = null,
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
    /**
     * The gated tool's structured approval card, on a plain confirmation or an
     * outward review (never a preference hold). Display only: the answer
     * posted is unchanged.
     */
    val approvalPreview: ApprovalPreview? = null,
    /** The original call behind [approvalPreview], for its collapsed "Raw call" row. */
    val approvalRawCall: String? = null,
    /**
     * A captured send waiting for review: Send / Discard / Always allow,
     * posted as the chosen id. Null for every other confirmation.
     */
    val outwardReview: OutwardReview? = null,
    /** The server's respond handle; when present, [respondCall] posts exactly it. */
    val respond: HitlRespondHandle? = null,
    val isOriginalFolderEdit: Boolean = false,
    val originConnection: ai.magicbeans.magdroid.access.MagicianConnectionSnapshot? = null,
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

    /** The line an ask leads with: a previewed approval's title, else the prompt. */
    val headline: String get() = approvalPreview?.title ?: prompt

    /** "from <agent>", for an ask raised by a delegated child. */
    val fromLabel: String? get() = delegatedAgentId?.let { "from $it" }

    val grantDenyOption: AttentionGrantOption?
        get() = grantOptions.firstOrNull { it.id == "deny" }
    val grantAllowOptions: List<AttentionGrantOption>
        get() = grantOptions.filter { it.id != "deny" }

    /** Whether the ask collects a secret: the spec, or the legacy secure types. */
    val isSensitive: Boolean get() = sensitive != null || legacySecure

    /** How the single-value field renders: `otp`, `password`, or the widget type. */
    val renderKind: String get() = if (isGodMode && !isSensitive) "sandbox_override" else hitlRenderKind(inputType, sensitive?.kind) ?: inputType

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
    // The server's respond handle names the route; the chains below answer
    // only for servers that predate it.
    val handle = metadata?.respond ?: metadata?.hitlRequest?.respond

    val source = when {
        // An approval posts as an approval whatever else the metadata says.
        approval -> "approval"
        handle != null -> handle.source
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
    val correlationId = handle?.correlationId
        ?: ids?.correlationId.orNull()
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
    val godContext = schema?.context as? JsonObject
    val god = source == "user_request" && inputType == "choice" && schema?.requestType == "sandbox_override" &&
        godContext?.get("god_mode") == JsonPrimitive(true)
    val godFiles = if (god) (godContext?.get("files") as? JsonArray).orEmpty().mapNotNull {
        runCatching { attentionJson.decodeFromJsonElement(AttentionFileDiff.serializer(), it) }.getOrNull()
    } else emptyList()
    val grantKind = if (god) "sandbox" else when (inputType) {
        "tool_authorization" -> "tool"
        "sandbox_override" -> "sandbox"
        else -> null
    }
    val grantSubject = when (grantKind) {
        "sandbox" -> if (god) schema?.contextString("command") else schema?.command
        "tool" -> schema?.toolName
        else -> null
    }.orEmpty()
    val grantDetail = when (grantKind) {
        "sandbox" -> if (god) schema?.contextString("violation") else schema?.violation
        "tool" -> schema?.paramsSummary
        else -> null
    }.orNull()
    val chainTotal = schema?.chainTotal
    val chainPosition = schema?.chainPosition
    val wantsMultiple = schema?.multiple == true
    val pathFilter = schema?.filter.orNull()

    val alignment = if (inputType == "confirmation") {
        parseAlignmentProjection(schema?.alignmentRaw ?: hitl?.inputSchema?.alignmentRaw)
    } else {
        null
    }
    val outwardReview = if (alignment == null) {
        parseOutwardReview(
            inputType = inputType,
            escalationTrigger = schema?.escalationTrigger ?: hitl?.inputSchema?.escalationTrigger,
            options = options,
            raw = schema?.outwardReviewRaw ?: hitl?.inputSchema?.outwardReviewRaw,
        )
    } else {
        null
    }
    // A plain confirmation and an outward review both lead with the card.
    val approvalPreview = if (inputType == "confirmation" && alignment == null) {
        ApprovalPreview.parse(schema?.approvalPreviewRaw ?: hitl?.inputSchema?.approvalPreviewRaw)
    } else {
        null
    }
    val prompt = metadata?.question.orNull() ?: hitl?.prompt.orNull() ?: title

    return AttentionRequest(
        source = source,
        correlationId = correlationId,
        inputType = inputType,
        options = options,
        prompt = prompt,
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
        originConnection = originConnection,
        isGodMode = god,
        isOriginalFolderEdit = god && godContext?.get("vibedev_edit") == JsonPrimitive(true),
        godChatSessionId = if (god) schema?.contextString("chat_session_id") else null,
        godOriginSession = if (god) schema?.contextString("session") else null,
        godOriginAgent = if (god) schema?.contextString("agent_id") else null,
        godFiles = godFiles,
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
        externalDismissLabel = if (oauth) schema?.dismissLabel.orNull() ?: "Not now" else null,
        externalServiceLabel = if (oauth) schema?.serviceLabel.orNull() else null,
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
        alignment = alignment,
        approvalPreview = approvalPreview,
        outwardReview = outwardReview,
        approvalRawCall = approvalPreview?.let {
            approvalRawCall(schema?.actionSummaryRaw, schema?.reasonRaw)
                ?: approvalRawCall(hitl?.inputSchema?.actionSummaryRaw, hitl?.inputSchema?.reasonRaw)
                ?: prompt
        },
        respond = handle,
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
