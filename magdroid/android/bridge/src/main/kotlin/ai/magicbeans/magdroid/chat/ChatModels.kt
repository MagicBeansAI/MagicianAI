package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.voice.SpeechTags
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.jsonPrimitive

/**
 * The chat contract, as the backend actually speaks it.
 *
 * Fields are optional wherever the backend may omit them and `ignoreUnknownKeys`
 * is on, because this API is developed alongside a web and an iOS client and a
 * new field must not take the Android one down.
 */
@Serializable
data class ChatSession(
    @SerialName("internal_voice") val internalVoice: ConcurrentSessionOrigin? = null,
    @SerialName("history_lane") val historyLane: String? = null,
    val id: String = "",
    @SerialName("session_id") val sessionId: String? = null,
    val title: String? = null,
    @SerialName("ui_thread_id") val uiThreadId: String? = null,
    val status: String? = null,
    @SerialName("is_default_session") val isDefaultSession: Boolean = false,
) {
    /** The backend has used both names over time; either is the session. */
    fun identifier(): String = sessionId?.takeIf { it.isNotBlank() } ?: id
}

/**
 * `GET /chat/sessions/{id}` — the session beside its messages.
 *
 * The session is nested, not spread across the top level, so the id has to be
 * read through it.
 */
@Serializable
data class ChatSessionDetail(
    val session: ChatSession = ChatSession(),
    val messages: List<ChatMessageDto> = emptyList(),
    /**
     * Whether messages older than the first embedded one exist. Absent on an
     * older server; [resolveHistoryHasMore] then guesses from a full page.
     */
    @SerialName("has_more") val hasMore: Boolean? = null,
)

/** Session-creating endpoints answer with the session wrapped. */
@Serializable
data class ChatSessionEnvelope(val session: ChatSession = ChatSession())

@Serializable
data class ChatMessageOrigin(
    @SerialName("ui_thread_id") val uiThreadId: String,
    @SerialName("session_id") val sessionId: String,
    @SerialName("request_id") val requestId: String,
    @SerialName("message_id") val messageId: String? = null,
)

data class OriginalAnswerLink(val origin: ChatMessageOrigin, val turnId: String?, val createdAt: Long?) {
    fun matches(message: ChatMessageDto): Boolean = if (origin.messageId != null) {
        message.id == origin.messageId
    } else {
        turnId != null && createdAt != null && message.chatTurnId == turnId &&
            message.createdAt == createdAt && !message.fromUser()
    }
}

@Serializable
data class ChatMessageDto(
    @SerialName("context_origin") val contextOrigin: ChatMessageOrigin? = null,
    val id: String? = null,
    @SerialName("session_id") val sessionId: String? = null,
    /** `user`, `assistant` or `system` — not a `role` field. */
    val direction: String? = null,
    val content: MessageContent = MessageContent(),
    @SerialName("presentation") val presentation: StructuredResponse? = null,
    @SerialName("chat_turn_id") val chatTurnId: String? = null,
    @SerialName("voice_origin") val voiceOrigin: Boolean? = null,
    /** Millis since epoch. */
    @SerialName("created_at") val createdAt: Long? = null,
) {
    fun fromUser(): Boolean = direction.equals("user", ignoreCase = true)
    fun isSystem(): Boolean = direction.equals("system", ignoreCase = true)
}

/**
 * A message's payload, discriminated by `type`.
 *
 * Modelled as one flat record with every field optional rather than a sealed
 * hierarchy: the backend adds variants faster than a closed hierarchy can
 * follow, and an unrecognised `type` should degrade to its text rather than
 * fail the whole history.
 */
@Serializable
data class MessageContent(
    val type: String = "text",
    val text: String? = null,
    val summary: String? = null,
    // task_status_update
    @SerialName("task_id") val taskId: String? = null,
    val status: String? = null,
    @SerialName("display_label") val displayLabel: String? = null,
    @SerialName("execution_id") val executionId: String? = null,
    @SerialName("ui_thread_id") val uiThreadId: String? = null,
    @SerialName("output_files") val outputFiles: List<ContentBlockRecord> = emptyList(),
    @SerialName("synthesis_pending") val synthesisPending: Boolean = false,
    /** `full` / `partial` on a completed status update (a partial success). */
    @SerialName("completion_kind") val completionKind: String? = null,
    // escalation
    @SerialName("pause_state_id") val pauseStateId: String? = null,
    @SerialName("request_id") val requestId: String? = null,
    @SerialName("correlation_id") val correlationId: String? = null,
    @SerialName("escalation_type") val escalationType: String? = null,
    @SerialName("input_type") val inputType: String? = null,
    /** The typed schema; read for the backend's sensitivity spec and placeholder. */
    @SerialName("input_schema") val inputSchema: ChatInputSchema? = null,
    val question: String? = null,
    val options: List<EscalationOption> = emptyList(),
    val resolved: Boolean? = null,
    // attachment
    val filename: String? = null,
    @SerialName("mime_type") val mimeType: String? = null,
    val size: Long? = null,
    val label: String? = null,
    // tool_call_executed / rich_tool_result
    @SerialName("tool_name") val toolName: String? = null,
    @SerialName("content_blocks") val contentBlocks: List<ContentBlockRecord> = emptyList(),
) {
    /** Answer an escalation through whichever id the backend gave us. */
    fun hitlCorrelationId(): String? =
        correlationId ?: requestId ?: pauseStateId
}

@Serializable
data class EscalationOption(
    val id: String = "",
    val label: String = "",
    val description: String? = null,
    /** Choosing this one is not an answer on its own; it needs typed detail. */
    @SerialName("requires_input") val requiresInput: Boolean? = null,
)

/**
 * An answer to a paused execution, in the shape the resume API validates.
 *
 * A sealed type rather than one struct with optional fields, because the
 * variants disagree about more than which key is filled: a password must not be
 * trimmed, file paths are a list split out of one line of text, and a
 * confirmation carries a boolean rather than an id. Flattening them would make
 * "which fields does this shape actually need" a question every call site has
 * to answer again.
 */
sealed class HitlResponseValue {
    abstract fun toJson(): JsonObject

    data class Text(val value: String) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "text"); put("value", value)
        }
    }

    /** Never trimmed — leading and trailing space can be part of a secret. */
    data class Password(val value: String) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "password"); put("value", value)
        }
    }

    data class Guidance(val advice: String) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "guidance"); put("advice", advice)
        }
    }

    data class FilePath(val paths: List<String>) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "file_path")
            putJsonArray("paths") { paths.forEach { add(it) } }
        }
    }

    data class Choice(val selectedId: String, val otherValue: String? = null) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "choice")
            put("selected_id", selectedId)
            otherValue?.takeIf { it.isNotEmpty() }?.let { put("other_value", it) }
        }
    }

    data class MultiChoice(val selectedIds: List<String>) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "multi_choice")
            putJsonArray("selected_ids") { selectedIds.forEach { add(it) } }
        }
    }

    data class Confirmation(val confirmed: Boolean) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "confirmation"); put("confirmed", confirmed)
        }
    }

    data class ExternalActionCompleted(val guidance: String? = null) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "external_action_completed")
            guidance?.takeIf { it.isNotEmpty() }?.let { put("guidance", it) }
        }
    }

    /**
     * The owner gave up on a secret ask — dismissed it, or wants a fresh code.
     * Posted so the pending operation retires and a new challenge can be
     * raised, rather than the ask waiting out its window.
     */
    data class Aborted(val reason: String? = null) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "aborted")
            reason?.takeIf { it.isNotEmpty() }?.let { put("reason", it) }
        }
    }

    data class Form(val answers: List<FormAnswer>) : HitlResponseValue() {
        override fun toJson() = buildJsonObject {
            put("type", "form")
            putJsonArray("answers") {
                answers.forEach { answer ->
                    add(
                        buildJsonObject {
                            put("id", answer.id)
                            put("skipped", answer.skipped)
                            if (!answer.skipped) {
                                // A choice question answers with ids, never a
                                // label typed into `value`.
                                if (answer.selectedIds.isNotEmpty()) {
                                    putJsonArray("selected_ids") { answer.selectedIds.forEach { add(it) } }
                                } else {
                                    answer.value?.let { put("value", it) }
                                }
                            }
                        },
                    )
                }
            }
        }
    }
}

data class FormAnswer(
    val id: String,
    val skipped: Boolean = false,
    val value: String? = null,
    /** A `choice` / `multi_choice` question's picks; wins over [value]. */
    val selectedIds: List<String> = emptyList(),
)

/**
 * The backend's value-free classification of an ask that collects a secret
 * (Rust `SensitiveInputSpec`, published as `input_schema.sensitive` on every
 * `hitl.requested` and pending listing since P3). A client masks by this —
 * never by the request-type name or the wording. `kind` describes a
 * single-value ask; `fields` lists a form's flagged ids and a field absent
 * from it is ordinary; `oneTime` material has a short collection deadline.
 */
@Serializable
data class SensitiveSpec(
    val kind: String? = null,
    val fields: List<SensitiveField>? = null,
    val provenance: String? = null,
    @SerialName("one_time") val oneTime: Boolean? = null,
    @SerialName("collection_deadline_ms") val collectionDeadlineMs: Long? = null,
    @SerialName("challenge_id") val challengeId: String? = null,
) {
    /** The flagged kind of one form field, or null for an ordinary field. */
    fun fieldKind(id: String): String? = fields?.firstOrNull { it.id == id }?.kind

    /** Whether the window for this material has closed. */
    fun isExpired(nowMs: Long = System.currentTimeMillis()): Boolean {
        val deadline = collectionDeadlineMs ?: return false
        return deadline > 0 && nowMs >= deadline
    }
}

@Serializable
data class SensitiveField(val id: String, val kind: String)

/** The typed schema a chat escalation carries; only what the card reads. */
@Serializable
data class ChatInputSchema(
    val type: String? = null,
    val placeholder: String? = null,
    val sensitive: SensitiveSpec? = null,
    @SerialName("approval_preview") val approvalPreview: kotlinx.serialization.json.JsonElement? = null,
    val alignment: kotlinx.serialization.json.JsonElement? = null,
    @SerialName("action_type") val actionTypeRaw: kotlinx.serialization.json.JsonElement? = null,
    @SerialName("destructive") val destructiveRaw: kotlinx.serialization.json.JsonElement? = null,
    @SerialName("protected") val protectedRaw: kotlinx.serialization.json.JsonElement? = null,
    @SerialName("allow_other") val allowOtherRaw: kotlinx.serialization.json.JsonElement? = null,
) {
    private fun flagged(value: kotlinx.serialization.json.JsonElement?): Boolean =
        (value as? kotlinx.serialization.json.JsonPrimitive)?.content == "true"
    val allowOther: Boolean get() = flagged(allowOtherRaw)
    val requiresReview: Boolean get() = flagged(destructiveRaw) || flagged(protectedRaw) ||
        (actionTypeRaw != null && actionTypeRaw != kotlinx.serialization.json.JsonNull) ||
        (approvalPreview != null && approvalPreview != kotlinx.serialization.json.JsonNull) ||
        (alignment != null && alignment != kotlinx.serialization.json.JsonNull)
}

/**
 * How an ask with the given widget type and classification is rendered.
 *
 * The typed widget wins; a `text`/`guidance` ask the backend classified as a
 * secret is masked by its kind — a code as `otp`, anything else as `password`.
 * An identifier stays readable; only its handling changes. **The value posted
 * still follows the widget type** (a masked `text` ask posts `text`), which is
 * the type the pause expects; the backend routes it to custody by the spec.
 */
fun hitlRenderKind(inputType: String?, sensitiveKind: String?): String? {
    val kind = sensitiveKind ?: return inputType
    if (inputType != "text" && inputType != "guidance") return inputType
    return when (kind) {
        "otp" -> "otp"
        "password", "other" -> "password"
        else -> inputType
    }
}

/** Whether a field of this flagged kind renders masked. */
fun hitlFieldIsMasked(sensitiveKind: String?): Boolean =
    sensitiveKind == "password" || sensitiveKind == "otp" || sensitiveKind == "other"

/**
 * Turn what the owner did into what the resume API expects.
 *
 * Pure, and separate from the card that draws it, so the wire contract can be
 * tested without a screen — the shapes here are validated server-side and a
 * wrong one leaves an execution paused with no way to answer it.
 *
 * Null means the form is not answerable yet. Returning null rather than an
 * empty value is what stops a blank text box from resuming a task with nothing.
 */
object HitlResponseComposer {

    private val AFFIRMATIVE = setOf("approve", "allow", "yes", "confirm", "continue", "done")
    private val NEGATIVE = setOf("reject", "deny", "no", "cancel", "stop", "dismiss")

    fun compose(
        inputType: String?,
        option: EscalationOption? = null,
        text: String = "",
        selectedIds: List<String> = emptyList(),
        allowsMultipleFiles: Boolean = true,
        /**
         * The backend classified this ask as collecting a secret. The value
         * posted keeps the widget's type — the pause expects it — and is never
         * trimmed: a code's leading zero and a password's trailing space are
         * part of the value.
         */
        sensitive: Boolean = false,
    ): HitlResponseValue? {
        val trimmed = text.trim()
        return when (inputType) {
            "text" ->
                if (sensitive) text.ifEmpty { null }?.let { HitlResponseValue.Text(it) }
                else trimmed.ifEmpty { null }?.let { HitlResponseValue.Text(it) }
            // Deliberately `text`, not `trimmed`: whitespace can be significant
            // in a secret, and silently editing one produces a failure the owner
            // cannot see. A one-time code rides the password value shape —
            // masked everywhere, exact, never coerced through a number; the
            // ask's type is what says "code".
            "password", "otp" -> text.ifEmpty { null }?.let { HitlResponseValue.Password(it) }
            "guidance" ->
                if (sensitive) text.ifEmpty { null }?.let { HitlResponseValue.Guidance(it) }
                else trimmed.ifEmpty { null }?.let { HitlResponseValue.Guidance(it) }
            "file_path" -> {
                val paths = if (allowsMultipleFiles) {
                    text.split(',', '\n').map(String::trim).filter(String::isNotEmpty)
                } else {
                    listOf(trimmed).filter(String::isNotEmpty)
                }
                paths.takeIf { it.isNotEmpty() }?.let { HitlResponseValue.FilePath(it) }
            }
            "multi_choice" -> selectedIds
                .takeIf { it.isNotEmpty() }
                ?.let { HitlResponseValue.MultiChoice(it) }
            "confirmation" -> option?.let {
                when (it.id) {
                    in AFFIRMATIVE -> HitlResponseValue.Confirmation(true)
                    in NEGATIVE -> HitlResponseValue.Confirmation(false)
                    // An option this build does not recognise still answers as
                    // itself rather than being guessed into a yes or a no.
                    else -> HitlResponseValue.Choice(it.id)
                }
            }
            "external_action" -> option?.let {
                if (it.requiresInput == true && trimmed.isEmpty()) null
                else HitlResponseValue.ExternalActionCompleted(trimmed.ifEmpty { null })
            }
            "form" -> null
            else -> option?.let {
                if (it.requiresInput == true && trimmed.isEmpty()) null
                else HitlResponseValue.Choice(it.id, trimmed.ifEmpty { null })
            }
        }
    }

    /**
     * Several answers in one pause. Skip is not abort: a skipped field is
     * still an answer the loop can continue from.
     *
     * Null when there are no questions — posting `{type:form, answers:[]}`
     * would resume a pause with nothing in it.
     */
    fun composeForm(answers: List<FormAnswer>): HitlResponseValue? =
        answers.takeIf { it.isNotEmpty() }?.let { HitlResponseValue.Form(it) }
}

/**
 * Where a chat output block's file lives (`source`): `task_output` (the
 * task's own outputs) or `session_output` / `chat_output` (copied into the
 * chat session). [LiveTaskCardRules.outputHome] reads it.
 */
@Serializable
data class ContentBlockSource(
    val type: String? = null,
    @SerialName("task_id") val taskId: String? = null,
)

@Serializable
data class ContentBlockRecord(
    val type: String? = null,
    val text: String? = null,
    val label: String? = null,
    val filename: String? = null,
    val url: String? = null,
    @SerialName("absolute_path") val absolutePath: String? = null,
    @SerialName("mime_type") val mimeType: String? = null,
    /** The path under the owner's outputs the file is served from. */
    @SerialName("relative_path") val relativePath: String? = null,
    @SerialName("display_name") val displayName: String? = null,
    val source: ContentBlockSource? = null,
    /**
     * Byte size as the wire states it. Kept raw: a float or a string here
     * must not fail the whole message's decode — [size] reads it loosely.
     */
    @SerialName("size") val sizeRaw: JsonPrimitive? = null,
) {
    /** Bytes, when the block states a non-negative number. */
    val size: Long?
        get() {
            val raw = sizeRaw ?: return null
            val value = if (raw.isString) raw.content.trim().toDoubleOrNull() else raw.doubleOrNull
            return value?.takeIf { it.isFinite() && it >= 0 }?.toLong()
        }

    /** The name a reader sees: display name, label, file name, the path's last part. */
    fun name(): String? =
        displayName?.takeIf { it.isNotBlank() }
            ?: label?.takeIf { it.isNotBlank() }
            ?: filename?.takeIf { it.isNotBlank() }
            ?: relativePath?.trim()?.trimEnd('/')?.substringAfterLast('/')?.takeIf { it.isNotBlank() }

    fun display(): String =
        displayName?.takeIf { it.isNotBlank() }
            ?: label?.takeIf { it.isNotBlank() }
            ?: filename?.takeIf { it.isNotBlank() }
            ?: text?.takeIf { it.isNotBlank() }
            ?: url.orEmpty()
}

/**
 * What a message is, once its content type has been read.
 *
 * A task card and an answer are not the same object wearing different text, and
 * rendering both as a bubble is what made a paused execution look like a reply.
 */
enum class MessageKind { Text, TaskStatus, Escalation, Attachment }

/** Task-card fields, lifted out of the content so the view needs no parsing. */
data class TaskStatusCard(
    val taskId: String,
    val title: String,
    val status: String,
    val summary: String? = null,
    val executionId: String? = null,
    val outputs: List<ContentBlockRecord> = emptyList(),
    val synthesisPending: Boolean = false,
    /**
     * Shell output streamed while the run works, newest last — the card's
     * terminal block. Realtime-only: history reloads start it empty, the same
     * as iOS, because the transcript endpoint does not carry shell replays.
     */
    val terminalLines: List<String> = emptyList(),
    /** `full` / `partial` on a completed status update (a partial success). */
    val completionKind: String? = null,
    /** The chat session the task's outputs were copied into (`ui_thread_id`). */
    val uiThreadId: String? = null,
    /**
     * The task's status in the task record (`GET /v3/tasks/{id}` →
     * `task.state.status`), when fetched. The live card shows it over a
     * message stuck at planning/draft ([LiveTaskCardRules.reconciled]).
     */
    val storeStatus: String? = null,
    /** Epoch ms (or seconds) of the first status update this card has seen and of the latest one. */
    val startedAtMs: Long? = null,
    val updatedAtMs: Long? = null,
) {
    private val normalized: String get() = status.lowercase()

    val terminal: Boolean
        get() = normalized in setOf(
            "complete", "completed", "done", "failed", "error", "cancelled", "canceled",
        )

    val failed: Boolean get() = normalized in setOf("failed", "error")

    val running: Boolean
        get() = synthesisPending || normalized in setOf(
            "planning", "ready", "queued", "running", "in_progress", "executing",
        )

    /** The status as a person would say it, matching the other surfaces. */
    fun verb(): String = when {
        synthesisPending -> "Preparing final result"
        normalized == "created" -> "Created"
        normalized in setOf("planning", "ready") -> "Planning"
        normalized == "queued" -> "Queued"
        normalized in setOf("running", "in_progress", "executing") -> "Running"
        normalized == "paused" -> "Paused"
        normalized in setOf("complete", "completed", "done") -> "Completed"
        normalized in setOf("failed", "error") -> "Failed"
        normalized in setOf("cancelled", "canceled") -> "Cancelled"
        else -> status.replace('_', ' ').replaceFirstChar { it.uppercase() }
    }
}

/** An escalation waiting on an answer. */
data class EscalationCard(
    val question: String,
    val options: List<EscalationOption>,
    val correlationId: String?,
    val executionId: String?,
    val resolved: Boolean,
    /**
     * The pause's expected input shape.
     *
     * Sent back on the answer because the resume API validates against this
     * exact value; deriving it from the chosen option would lose the pause's
     * contract for the specialised kinds that are also choice-shaped.
     */
    val inputType: String? = null,
    /**
     * The backend's value-free classification when the ask collects a secret
     * (`input_schema.sensitive`). A card masks by this, never by wording.
     */
    val sensitive: SensitiveSpec? = null,
    val placeholder: String? = null,
    /** Set while the answer is in flight, so the card cannot be double-sent. */
    val answering: String? = null,
    /** The pause's category (`user_input`, `confirmation`, `tool_authorization`, …). */
    val escalationType: String? = null,
    /** Why the last answer did not land. */
    val error: String? = null,
    val taskId: String? = null,
    val requiresReview: Boolean = false,
    val allowOther: Boolean = false,
)

/** A message as the screen holds it, streaming or settled. */
data class ChatMessage(
    val id: String,
    val fromUser: Boolean,
    val text: String,
    val streaming: Boolean = false,
    /** Reasoning shown while it happens and collapsed after; never mixed into `text`. */
    val reasoning: String = "",
    /** Tools named as they run, so a long silence has a visible cause. */
    val activity: List<String> = emptyList(),
    val failed: Boolean = false,
    /**
     * The backend's own composition of this answer, when it sent one. Preferred
     * over `text`: rendering the plain field instead would flatten a table back
     * into a paragraph.
     */
    val structured: StructuredResponse? = null,
    /** Coalesced steps, shown collapsed under the bubble. */
    val activityRows: List<ActivityRow> = emptyList(),
    /**
     * A note from the system rather than a turn in the conversation.
     *
     * Shown as a small quiet pill, not a bubble: dressing "session resumed" as
     * an answer implies someone said it.
     */
    val system: Boolean = false,
    /** What this row is. Everything but [MessageKind.Text] carries a card. */
    val kind: MessageKind = MessageKind.Text,
    val task: TaskStatusCard? = null,
    val escalation: EscalationCard? = null,
    /** `filename` and a human-readable size, for an attachment row. */
    val attachment: Pair<String, String?>? = null,
    /** Files an answer produced, shown as their own rows under the bubble. */
    val outputs: List<ContentBlockRecord> = emptyList(),
    /** The turn came in by voice, which the transcript says out loud. */
    val voiceOrigin: Boolean = false,
    /**
     * The turn this message belongs to.
     *
     * Carried so live activity deltas can find their message: the realtime bus
     * addresses rows by turn, and without it a delta has no way to say which
     * bubble it belongs to.
     */
    val chatTurnId: String? = null,
    /**
     * When the message was written, epoch milliseconds (the wire's
     * `created_at`; a local echo's send time). The transcript's day dividers
     * and floating date pill read it; null never opens a day.
     */
    val createdAtMs: Long? = null,
    val originalAnswer: OriginalAnswerLink? = null,
    val linkedAnswerTarget: Boolean = false,
    /**
     * A local row awaiting its persisted copy: the user's echo, or the
     * assistant bubble a turn draws before its reply is stored. The persisted
     * reply replaces such a bubble whatever state the stream left it in — a
     * clean drop leaves it neither streaming nor failed, and matching only
     * those drew the late reply under an empty bubble.
     */
    val optimistic: Boolean = false,
    /**
     * Set on an `escalation_resolved` row: the ask it settles. The transcript
     * flips the matching open card instead of drawing this row as a second card.
     */
    val resolvesEscalation: EscalationRef? = null,
)

/** The ids that name one escalation, for matching a resolution to its card. */
data class EscalationRef(val correlationId: String?, val executionId: String?) {
    /**
     * Same rule as iOS `escalationMatches`: two canonical ids must agree, since
     * one planning execution can own several sibling questions; only when either
     * side lacks one does the execution id decide.
     */
    fun matches(card: EscalationCard): Boolean {
        val mine = correlationId?.trim()?.takeIf { it.isNotEmpty() }
        val theirs = card.correlationId?.trim()?.takeIf { it.isNotEmpty() }
        if (mine != null && theirs != null) return mine == theirs
        val execution = card.executionId?.trim()?.takeIf { it.isNotEmpty() } ?: return false
        return execution == executionId?.trim()
    }
}

/** The immediate local echo must carry the same origin as its later wire row. */
internal fun optimisticUserMessage(
    id: String, text: String, voiceOrigin: Boolean, chatTurnId: String? = null,
): ChatMessage = ChatMessage(
    id = id, fromUser = true, text = text, voiceOrigin = voiceOrigin,
    chatTurnId = chatTurnId, optimistic = true, createdAtMs = System.currentTimeMillis(),
)

/**
 * One decoded SSE frame.
 *
 * Modelled as a sealed type rather than a string switch so an unhandled frame is
 * a compile error here rather than silence at runtime — the backend emits more
 * kinds than this screen renders, and the ones it ignores should be ignored on
 * purpose.
 */
sealed interface ChatStreamEvent {
    data class Token(val text: String) : ChatStreamEvent
    data class ReasoningDelta(val text: String) : ChatStreamEvent
    data object ReasoningEnd : ChatStreamEvent
    data class ToolCall(val name: String) : ChatStreamEvent
    data class Failed(val message: String) : ChatStreamEvent

    /**
     * The turn settled, carrying everything the stream could not.
     *
     * `done` is not a bare signal: it holds the whole `ChatResponse`, and the
     * assistant's composed presentation and any cards this turn produced arrive
     * only here. Treating it as "the tokens are finished" is why a structured
     * answer showed as flat text until the session was reloaded.
     */
    data class Done(
        val messages: List<ChatMessage> = emptyList(),
        val sessionTitle: String? = null,
        val queuedPosition: Int? = null,
        val cancelled: Boolean = false,
        /**
         * The stream ended without a `done` frame. The turn itself may still
         * be running on the server (`continue_on_disconnect`); only its
         * `turn_state` can say.
         */
        val dropped: Boolean = false,
    ) : ChatStreamEvent
    /** A frame this screen does not render. Kept explicit so it is a decision. */
    data class Ignored(val event: String) : ChatStreamEvent
}

val chatJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    coerceInputValues = true
}

/**
 * The encoder for request bodies.
 *
 * `encodeDefaults` is off by default, which silently drops any field left at
 * its default — including constants like `source` and the `type` tag that
 * discriminates a value. The server requires those, so a body encoded with
 * [chatJson] is rejected for being incomplete rather than wrong, which is
 * indistinguishable from nothing happening.
 */
val chatRequestJson: Json = Json {
    encodeDefaults = true
    explicitNulls = false
}

/**
 * A backend-authored structured reply.
 *
 * Magician composes answers as blocks — a summary, key values, a list, an
 * artifact — rather than one wall of markdown. Rendering only `text` throws that
 * structure away and gives the owner a paragraph where they were handed a table.
 */
@Serializable
data class StructuredResponse(
    val title: String? = null,
    val summary: String? = null,
    val tone: String? = null,
    val blocks: List<StructuredBlock> = emptyList(),
)

@Serializable
data class StructuredBlock(
    val kind: String = "text",
    val title: String? = null,
    val text: String? = null,
    val tone: String? = null,
    /**
     * Rows for `key_values`, entries for `list`, files for `artifacts`. Values
     * are read loosely: an artifact's `size` arrives as a number, and a strict
     * String decode of it failed the whole reply.
     */
    val items: List<@Serializable(with = LooseStringMapSerializer::class) Map<String, String>> = emptyList(),
)

/** A JSON object read as text values: strings as-is, other primitives by their
 *  literal (`1024`, `true`), nested values as JSON, `null` dropped. */
object LooseStringMapSerializer : KSerializer<Map<String, String>> {
    private val delegate = MapSerializer(String.serializer(), JsonElement.serializer())
    override val descriptor: SerialDescriptor = delegate.descriptor

    override fun deserialize(decoder: Decoder): Map<String, String> =
        decoder.decodeSerializableValue(delegate).mapNotNull { (key, value) ->
            when {
                value is JsonNull -> null
                value is JsonPrimitive -> key to value.content
                else -> key to value.toString()
            }
        }.toMap()

    override fun serialize(encoder: Encoder, value: Map<String, String>) =
        encoder.encodeSerializableValue(delegate, value.mapValues { JsonPrimitive(it.value) })
}

/** One coalesced step of what the agent did, for the chat turn's Steps section. */
@Serializable
data class ActivityRow(
    val label: String = "",
    val detail: String? = null,
    val status: String? = null,
    /** Opaque handle for the lossless result reader; never a filesystem path. */
    val resultRef: String? = null,
    val resultHash: String? = null,
    val resultSizeBytes: Int? = null,
    /** Canonical lifecycle owner, separate from task navigation metadata. */
    val resultOwner: ActivityResultOwner? = null,
    val taskId: String? = null,
    val executionId: String? = null,
    /** `llm`, `tool`, `step`, `pause`, … — the accumulator's row kind. */
    val kind: String? = null,
    val durationMs: Double? = null,
    /** A finished model call's usage (LLM rows only). */
    val callUsage: ActivityCallUsage? = null,
    /** Tasks a finished tool call started (`create_task` / `run_task`). */
    val spawnedTaskIds: List<String> = emptyList(),
    /** Host files the step wrote or read (absolute paths), for Open file / Reveal folder. */
    val files: List<ActivityFileRef> = emptyList(),
    /** `info`, `tool`, `error`, `reasoning`, `pause` — the row mark's colour when not settled. */
    val tone: String? = null,
    /** The tool a tool row calls; keys the row's dot tint (web `StageRow.tool`). */
    val toolName: String? = null,
    /** The accumulator's coalescing key (`task::<id>` for a delegated task's row). */
    val key: String? = null,
    /** When a model or tool call began (epoch ms), for a running row's live elapsed clock. */
    val startedAtMs: Double? = null,
)

/**
 * What one finished model call reported, read off its event (web
 * `deriveCacheSummary` / `llmUsageSuffix`). Every figure is separately
 * optional: absence is not zero.
 */
@Serializable
data class ActivityCallUsage(
    /** Stable logical call id, when two transports deliver the same call. */
    val callId: String? = null,
    /** Null when not reported or when `usage_availability.tokens` is false. */
    val inputTokens: Int? = null,
    val outputTokens: Int? = null,
    /** Any of input, output, cache-read or cache-write tokens was reported. */
    val reportedTokens: Boolean = false,
    /** Input + output as the event states them, for the step's own suffix. */
    val stepTokens: Int? = null,
    /** `cost_usd` (chat calls), else `cost` (task-run calls). */
    val costUsd: Double? = null,
)

/** Canonical owner emitted in `tool.result.projected`. */
@Serializable
data class ActivityResultOwner(
    val kind: String = "",
    @SerialName("session_id") val sessionId: String? = null,
    @SerialName("task_id") val taskId: String? = null,
    @SerialName("execution_id") val executionId: String? = null,
    @SerialName("voice_session_id") val voiceSessionId: String? = null,
)

/** A session as the history panel lists it. */
@Serializable
data class SessionSummary(
    @SerialName("internal_voice") val internalVoice: ConcurrentSessionOrigin? = null,
    val id: String = "",
    @SerialName("session_id") val sessionId: String? = null,
    val principal: String? = null,
    val workspace: String? = null,
    @SerialName("agent_id") val agentId: String? = null,
    val title: String? = null,
    @SerialName("created_at") val createdAt: Long? = null,
    @SerialName("updated_at") val updatedAt: Long? = null,
    @SerialName("message_count") val messageCount: Int? = null,
    @SerialName("ui_thread_id") val uiThreadId: String? = null,
    val status: String? = null,
    @SerialName("history_lane") val historyLane: String? = null,
    @SerialName("is_default_session") val isDefaultSession: Boolean = false,
) {
    fun identifier(): String = sessionId?.takeIf { it.isNotBlank() } ?: id
    /** An untitled session is normal — it is named by its first exchange. */
    fun label(): String = title?.takeIf { it.isNotBlank() } ?: "Untitled chat"

    /**
     * The thread this session belongs to, as a person would say it.
     *
     * The default thread's id is a slug; everything else is already named by
     * whoever made it.
     */
    fun threadLabel(): String = when (val thread = uiThreadId?.takeIf { it.isNotBlank() }) {
        null, "general" -> "General"
        else -> thread
    }
}

@Serializable
data class SessionList(
    val sessions: List<SessionSummary> = emptyList(),
    val items: List<SessionSummary> = emptyList(),
    val total: Int? = null,
    val limit: Int? = null,
    val offset: Int? = null,
) {
    fun all(): List<SessionSummary> = sessions.ifEmpty { items }
}

/**
 * One `@` reference the composer can offer.
 *
 * The catalog is per session because it reflects that session's agent and its
 * delegates — offering a global list would suggest references that will not
 * resolve.
 */
@Serializable
data class ReferenceItem(
    val id: String = "",
    val label: String? = null,
    val name: String? = null,
    val kind: String? = null,
    val description: String? = null,
) {
    fun display(): String = label?.takeIf { it.isNotBlank() } ?: name?.takeIf { it.isNotBlank() } ?: id
}

@Serializable
data class ReferenceCatalog(
    val references: List<ReferenceItem> = emptyList(),
    val items: List<ReferenceItem> = emptyList(),
) {
    fun all(): List<ReferenceItem> = references.ifEmpty { items }
}

/** A file staged in the composer, before and after upload. */
data class StagedAttachment(
    val localId: String,
    val name: String,
    val remoteId: String? = null,
    val uploading: Boolean = true,
    val failed: Boolean = false,
    /** Why it failed, so the chip can say more than that it did. */
    val error: String? = null,
    val mimeType: String? = null,
    /** An image's own bytes, for the composer's thumbnail; null for other kinds. */
    val preview: ByteArray? = null,
) {
    /**
     * The name shortened from the middle, not the end.
     *
     * Camera and screenshot filenames share a long prefix and differ only in
     * their tail, so truncating the tail renders every one of them identical.
     */
    fun shortName(limit: Int = 13): String {
        if (name.length <= limit) return name
        val head = (limit - 1) / 2
        val tail = limit - 1 - head
        return name.take(head) + "…" + name.takeLast(tail)
    }
}

@Serializable
data class ChatModelSelection(
    @SerialName("model_id") val modelId: String = "",
    val thinking: String = "auto",
)

@Serializable
data class ChatThinkingControl(
    val mode: String = "none",
    val levels: List<String> = emptyList(),
    @SerialName("default") val defaultLevel: String? = null,
)

@Serializable
data class ChatModelLimits(
    @SerialName("context_window_tokens") val contextWindowTokens: Int = 0,
)

@Serializable
data class ChatModelCapabilities(val vision: Boolean = false)

@Serializable
data class ChatModelChoice(
    val id: String = "",
    val provider: String = "",
    val connection: String = "",
    val model: String = "",
    val label: String = "",
    val thinking: ChatThinkingControl = ChatThinkingControl(),
    val choices: Map<String, String> = emptyMap(),
    @SerialName("operation_choices") val operationChoices: Map<String, String>? = null,
    val installed: Boolean = true,
    val limits: ChatModelLimits? = null,
    val capabilities: ChatModelCapabilities = ChatModelCapabilities(),
) {
    val providerDisplayName: String get() = when (provider) {
        "openai" -> "OpenAI"
        "anthropic" -> "Anthropic"
        "gemini" -> "Gemini"
        "deepseek" -> "DeepSeek"
        "minimax" -> "MiniMax"
        "xai" -> "xAI"
        "sarvam" -> "Sarvam"
        "ollama" -> "Ollama"
        "yutori" -> "Yutori"
        "openrouter" -> "OpenRouter"
        "harness-claude_code" -> "Claude Code"
        "harness-codex" -> "Codex"
        "harness-grok" -> "Grok"
        "harness-agy" -> "Antigravity"
        "harness-pi" -> "Pi"
        else -> provider
    }
}

/** Web `thinkingLabel` (`modelCatalog.ts`): known levels get a display name, anything else stays raw. */
fun thinkingLevelLabel(level: String): String = when (level) {
    "auto" -> "Auto"
    "none" -> "Off"
    "on" -> "On"
    "minimal" -> "Minimal"
    "low" -> "Low"
    "medium" -> "Medium"
    "high" -> "High"
    "xhigh" -> "Extra high"
    "max" -> "Max"
    "ultra" -> "Ultra"
    else -> level
}

/**
 * The thinking tag on a composer model chip (web `ComposerModelChip` `thinking`):
 * shown only for a catalog model whose thinking mode is not `none`, with the
 * selected level defaulting to `auto`. VibeDev's coding chip uses this directly.
 */
fun modelChipThinkingLabel(model: ChatModelChoice?, selection: ChatModelSelection?): String? =
    model?.takeIf { it.thinking.mode != "none" }?.let { thinkingLevelLabel(selection?.thinking ?: "auto") }

/** Chat composer rule (web `ChatPanel` `profileThinkingLabel`): only the catalog engines `magician` and `pi`. */
fun chatComposerThinkingLabel(engine: String, model: ChatModelChoice?, selection: ChatModelSelection?): String? =
    if (engine == "magician" || engine == "pi") modelChipThinkingLabel(model, selection) else null

/** The chip's spoken label: `<label>, thinking: High` when a thinking tag is shown (web aria-label). */
fun modelChipAccessibilityLabel(label: String, thinking: String?): String =
    if (thinking != null) "$label, thinking: $thinking" else label

/** Internal route compatibility plus server-owned model selection metadata. */
@Serializable
data class ChatProfile(
    val name: String = "",
    val label: String? = null,
    val provider: String? = null,
    val model: String? = null,
    @SerialName("is_default") val isDefault: Boolean = false,
    val description: String? = null,
    @SerialName("model_selection") val modelSelection: ChatModelSelection? = null,
    @SerialName("model_choice") val modelChoice: ChatModelChoice? = null,
) {
    fun display(): String = label?.takeIf { it.isNotBlank() } ?: name

    /** iOS leads with the actual model when the server publishes one. */
    fun compactDisplay(): String = model?.takeIf { it.isNotBlank() } ?: display()
}

@Serializable
data class ChatModelCatalog(
    val models: List<ChatModelChoice> = emptyList(),
    @SerialName("default_selection") val defaultSelection: ChatModelSelection? = null,
) {
    fun rows(): List<ChatProfile> = models.flatMap { model -> model.choices.map { (thinking, route) ->
        val selection = ChatModelSelection(model.id, thinking)
        ChatProfile(name = route, provider = model.provider, model = model.model,
            isDefault = selection == defaultSelection, modelSelection = selection,
            modelChoice = model.takeIf { thinking == "auto" })
    } }
}

/** A one-time legacy selection migration; typed model/thinking is authoritative. */
data class ChatModelPreferenceResolution(
    val route: String,
    val selection: ChatModelSelection? = null,
    val issue: String? = null,
)

object ChatModelPreference {
    const val storageKey = "modelSelection"
    const val unavailableRoute = "__unavailable_model_selection__"
    const val unavailableMessage = "Your saved model choice is unavailable. Choose a model to continue."

    fun resolve(profiles: List<ChatProfile>, legacyId: String, saved: ChatModelSelection?): ChatModelPreferenceResolution {
        val previous = profiles.firstOrNull { it.name == legacyId }
        val selection = saved ?: previous?.modelSelection ?: LegacyLlmSelections.chatSelection(legacyId)
        if (selection != null) {
            val route = profiles.mapNotNull { it.modelChoice }
                .firstOrNull { it.id == selection.modelId }?.choices?.get(selection.thinking)
            return if (route != null && profiles.any { it.name == route }) {
                ChatModelPreferenceResolution(route, selection)
            } else ChatModelPreferenceResolution(unavailableRoute, selection, unavailableMessage)
        }
        if (legacyId.isNotEmpty()) return ChatModelPreferenceResolution(
            legacyId, issue = if (previous == null) unavailableMessage else null,
        )
        val initial = profiles.firstOrNull { it.isDefault }
            ?: return ChatModelPreferenceResolution("", issue = "Choose a model to continue.")
        return resolve(profiles, initial.name, initial.modelSelection)
    }

    fun requestFields(selection: ChatModelSelection?, engine: String, nativeModel: String): JsonObject = buildJsonObject {
        put("harness_engine", engine)
        if (engine in setOf("magician", "pi")) {
            require(selection != null && selection.modelId.isNotBlank() && selection.thinking.isNotBlank()) { unavailableMessage }
            put("model_selection", buildJsonObject {
                put("model_id", selection.modelId)
                put("thinking", selection.thinking)
            })
        } else put("harness_model", nativeModel)
    }
}

@Serializable
data class ChatHarnessOption(
    val name: String = "magician",
    val installed: Boolean = false,
    val models: List<String> = listOf("default"),
    /** Whose credentials it runs on: `login`, `magician_key` or `none`; absent for `magician`. */
    @SerialName("credentials") val credentialsWire: JsonElement? = null,
) {
    /** The `credentials` string, read leniently: any other shape is no label. */
    val credentials: String? get() = (credentialsWire as? JsonPrimitive)?.takeIf { it.isString }?.content
}

@Serializable
data class ChatHarnessRoster(
    val engines: List<ChatHarnessOption> = emptyList(),
    /** The run engine (`execution.harness_engine`) as it resolves now, and its model. */
    val current: String? = null,
    @kotlinx.serialization.SerialName("run_model") val runModel: String? = null,
    /** Pi's background-run model; null when Pi uses its default. */
    @SerialName("run_model_selection") val runModelSelection: ChatModelSelection? = null,
    /** Agentic flows follow the chat engine ("Same as Chat"); [current] is what that resolves to. */
    @SerialName("follows_chat") val followsChat: Boolean = false,
    /** The server's chat engine (`chat.harness_engine`) and its model: what clients with no pick use. */
    @SerialName("chat_current") val chatCurrent: String? = null,
    @SerialName("chat_model") val chatModel: String? = null,
)

/**
 * Turn a wire message into something the transcript can draw.
 *
 * One place, because history and the live stream must agree: a card that
 * appeared when its event arrived and vanished on reload is worse than one that
 * never appeared.
 */
fun ChatMessageDto.project(index: Int): ChatMessage {
    val content = this.content
    val id = id ?: "history-$index"
    val base = ChatMessage(
        id = id,
        fromUser = fromUser(),
        system = isSystem(),
        text = content.text.orEmpty(),
        structured = presentation,
        // Steps are hydrated from the canonical turn-events endpoint after
        // projection; the message payload has never carried them.
        voiceOrigin = voiceOrigin == true,
        chatTurnId = chatTurnId,
        createdAtMs = createdAt?.let { ChatDateDividers.epochMillis(it.toDouble()) },
        originalAnswer = contextOrigin?.takeIf { !fromUser() && it.sessionId != sessionId }
            ?.let { OriginalAnswerLink(it, chatTurnId, createdAt) },
    )
    return when (content.type) {
        "task_status_update" -> base.copy(
            kind = MessageKind.TaskStatus,
            text = "",
            task = TaskStatusCard(
                taskId = content.taskId.orEmpty().ifBlank { "unknown-task" },
                title = content.displayLabel?.takeIf { it.isNotBlank() }
                    ?: content.taskId.orEmpty().ifBlank { "Task" },
                status = content.status ?: "updated",
                summary = content.summary,
                executionId = content.executionId,
                outputs = content.outputFiles,
                synthesisPending = content.synthesisPending,
                completionKind = content.completionKind,
                uiThreadId = content.uiThreadId,
                startedAtMs = createdAt,
                updatedAtMs = createdAt,
            ),
        )

        // A resolved escalation is the same card with its buttons spent —
        // dropping it would erase the question from the history that
        // explains what happened next.
        "escalation", "escalation_resolved" -> {
            val question = content.question
            val inputType = content.inputType?.trim()?.lowercase()
            // Choice-shaped pauses need options. Form, text, and the other
            // typed kinds do not — dropping those hid a live HITL behind a
            // plain row with no way to answer it.
            val answerableWithoutOptions = inputType in setOf(
                "form", "text", "guidance", "password", "otp", "file_path", "multi_choice",
            )
            val resolution = if (content.type == "escalation_resolved") {
                EscalationRef(content.hitlCorrelationId(), content.executionId)
            } else null
            if (question.isNullOrBlank() ||
                (content.options.isEmpty() && !answerableWithoutOptions)
            ) {
                // A resolution with nothing to draw settles its card; if no
                // card matches, it stays a quiet system note, not a bubble.
                if (resolution == null) base
                else base.copy(
                    system = true,
                    text = content.text?.takeIf { it.isNotBlank() }
                        ?: content.summary?.takeIf { it.isNotBlank() }
                        ?: "Request resolved",
                    resolvesEscalation = resolution,
                )
            }
            else base.copy(
                resolvesEscalation = resolution,
                kind = MessageKind.Escalation,
                text = "",
                escalation = EscalationCard(
                    question = question,
                    options = content.options,
                    correlationId = content.hitlCorrelationId(),
                    executionId = content.executionId,
                    taskId = content.taskId,
                    inputType = content.inputType,
                    escalationType = content.escalationType,
                    sensitive = content.inputSchema?.sensitive,
                    placeholder = content.inputSchema?.placeholder,
                    requiresReview = content.inputSchema?.requiresReview == true,
                    allowOther = content.inputSchema?.allowOther == true,
                    resolved = content.resolved == true || content.type == "escalation_resolved",
                ),
            )
        }

        "attachment" -> base.copy(
            kind = MessageKind.Attachment,
            text = "",
            attachment = (content.filename.orEmpty().ifBlank { "Attachment" }) to
                content.size?.let { humanSize(it) },
        )

        // A tool result carries no words of its own; its blocks are the
        // content, and its summary is the line that introduces them.
        "tool_call_executed", "rich_tool_result" -> base.copy(
            text = content.summary.orEmpty(),
            outputs = content.contentBlocks,
        )

        // "text" and anything newer than this build: show the words. An
        // unknown variant losing its card is a worse failure than the whole
        // history refusing to load.
        else -> base
    }
}

private fun humanSize(bytes: Long): String = when {
    bytes >= 1_048_576 -> "%.1f MB".format(bytes / 1_048_576.0)
    bytes >= 1_024 -> "%.0f KB".format(bytes / 1_024.0)
    else -> "$bytes B"
}

/**
 * The `done` frame's payload — the turn's outcome.
 *
 * Only the fields this client acts on; the rest (usage, notices, queue depth)
 * are ignored by the parser rather than modelled.
 */
@Serializable
data class ChatTurnResult(
    @SerialName("assistant_message") val assistantMessage: ChatMessageDto? = null,
    /** Display messages this turn created after the user's — cards included. */
    val messages: List<ChatMessageDto> = emptyList(),
    @SerialName("tool_executed") val toolExecuted: List<ChatMessageDto> = emptyList(),
    @SerialName("session_title") val sessionTitle: String? = null,
    val queued: QueuedReceipt? = null,
    val cancelled: Boolean = false,
) {
    /**
     * What the transcript should show for this turn, in order.
     *
     * `messages` is the ordered display list when the backend sends one; it
     * already contains the assistant's reply. Falling back to the reply alone
     * keeps older turns working, and de-duplicating by id means neither path
     * can show the same answer twice.
     */
    fun display(): List<ChatMessage> {
        val rows = messages.ifEmpty { listOfNotNull(assistantMessage) }
        return rows.mapIndexed { index, dto -> dto.project(index) }
            .distinctBy { it.id }
    }
}

@Serializable
data class QueuedReceipt(
    val id: String = "",
    val position: Int = 0,
)

/**
 * Composer handling for a chat send. `Ask` is the default and is omitted on
 * the wire; `Plan` and `AcceptInScope` have to be named or the backend treats
 * the turn as ordinary chat.
 */
enum class ChatComposerMode(val wire: String?) {
    Ask(null),
    AcceptInScope("accept_in_scope"),
    Plan("plan"),
}

/** Ask/Accept belongs to Do and survives temporary switches into Plan. */
enum class ChatDoPermission(val mode: ChatComposerMode) {
    Ask(ChatComposerMode.Ask),
    AcceptInScope(ChatComposerMode.AcceptInScope),
    ;

    companion object {
        fun fromMode(mode: ChatComposerMode): ChatDoPermission? = when (mode) {
            ChatComposerMode.Ask -> Ask
            ChatComposerMode.AcceptInScope -> AcceptInScope
            ChatComposerMode.Plan -> null
        }
    }
}

/** Typed model/thinking admission; native CLI engines keep their own model field. */
@Serializable
data class SendMessageRequest(
    val text: String,
    @SerialName("chat_turn_id") val chatTurnId: String,
    @SerialName("source_surface") val sourceSurface: String = "android",
    @SerialName("model_selection") val modelSelection: ChatModelSelection? = null,
    @SerialName("harness_engine") val harnessEngine: String? = null,
    @SerialName("harness_model") val harnessModel: String? = null,
    @SerialName("attachment_ids") val attachmentIds: List<String> = emptyList(),
    /**
     * True when this turn was spoken rather than typed.
     *
     * The backend stamps the turn with its origin, which is how a reply knows
     * it may be spoken back. Android tracked this in state and never sent it,
     * so every dictated turn arrived looking typed.
     */
    @SerialName("voice_origin") val voiceOrigin: Boolean = false,
    /**
     * Let the server finish an accepted turn if Android loses its process or
     * radio. The canonical transcript and realtime socket reconcile the answer
     * after reconnect, without replaying the turn or its tools.
     */
    @SerialName("continue_on_disconnect") val continueOnDisconnect: Boolean = false,
    /**
     * `plan` or `accept_in_scope`. Null (Ask) is omitted rather than sent as
     * `"ask"`, matching the web client.
     */
    val mode: String? = null,
)

/**
 * One SSE frame, named by the events `chat_api.rs` actually emits.
 *
 * The frame names are a fixed vocabulary on the server, so an event this
 * build does not know is ignored rather than guessed at — a new lifecycle
 * delta must not end the turn.
 */
fun decodeStreamFrame(event: String, raw: String): ChatStreamEvent {
    val obj = runCatching {
        chatJson.parseToJsonElement(raw) as? JsonObject
    }.getOrNull()
    fun field(vararg names: String): String {
        for (name in names) {
            obj?.get(name)?.jsonPrimitive?.contentOrNullSafe()?.let { if (it.isNotEmpty()) return it }
        }
        return ""
    }
    return when (event) {
        "token" -> ChatStreamEvent.Token(field("text"))
        "reasoning_delta" -> ChatStreamEvent.ReasoningDelta(field("delta", "text"))
        "reasoning_end" -> ChatStreamEvent.ReasoningEnd
        // `tool_call` names the tool; `tool_call_start` is the finer-grained
        // lifecycle event that replaced it and carries the same fact.
        "tool_call" -> ChatStreamEvent.ToolCall(field("name").ifEmpty { "a tool" })
        "tool_call_start" -> ChatStreamEvent.ToolCall(field("tool_name").ifEmpty { "a tool" })
        "error" -> ChatStreamEvent.Failed(
            field("error", "message", "detail").ifEmpty { "The turn failed." },
        )
        // The turn's real outcome. A malformed payload still ends the turn —
        // leaving the bubble streaming forever is the worse failure.
        "done" -> runCatching {
            val result = chatJson.decodeFromString(ChatTurnResult.serializer(), raw)
            ChatStreamEvent.Done(
                messages = result.display(),
                sessionTitle = result.sessionTitle?.takeIf { it.isNotBlank() },
                queuedPosition = result.queued?.position,
                cancelled = result.cancelled,
            )
        }.getOrElse { ChatStreamEvent.Done() }
        // reasoning_start, tool_call_args_delta, tool_call_end: real events
        // with nothing this surface shows.
        else -> ChatStreamEvent.Ignored(event)
    }
}

private fun kotlinx.serialization.json.JsonPrimitive.contentOrNullSafe(): String? =
    runCatching { content }.getOrNull()

/** The answer to an escalation, in the shape `AgenticResumeValue` expects. */
@Serializable
data class HitlRespondRequest(
    val source: String = "escalation",
    /**
     * The composed answer, already in wire shape.
     *
     * A `JsonObject` rather than one sealed serializable, because the resume
     * API validates the *shape* per input type and the variants disagree about
     * which keys exist. [HitlResponseComposer] is the single place that decides
     * it; letting this type re-derive it would be a second opinion on a
     * contract that only has one.
     */
    val value: JsonObject,
    @SerialName("input_type") val inputType: String? = null,
    val channel: String = "android",
    @SerialName("execution_id") val executionId: String? = null,
)

/**
 * The respond endpoint's answer.
 *
 * `accepted = false` arrives with HTTP 200 and a reason — an escalation that
 * was already answered, or one belonging to another scope. Treating the status
 * code alone as success would mark the card answered when nothing happened.
 */
@Serializable
data class HitlRespondResponse(
    val accepted: Boolean = false,
    val source: String? = null,
    val reason: String? = null,
) {
    /** The refusal, said plainly. */
    fun refusal(): String = when (reason) {
        "already_resolved" -> "That was already answered."
        "scope_mismatch" -> "That escalation belongs to another workspace."
        null -> "Magician did not accept the answer."
        else -> reason
    }
}

/** What the attachments endpoint answers with on success. */
@Serializable
data class AttachmentUploaded(
    @SerialName("attachment_id") val attachmentId: String = "",
    val filename: String? = null,
    @SerialName("mime_type") val mimeType: String? = null,
    val size: Long? = null,
)

@Serializable
data class AttentionFeed(val counts: AttentionCounts = AttentionCounts())

/**
 * What is waiting on the owner.
 *
 * Only the two fields the badge is built from; the lane counts belong to the
 * Attention surface itself.
 */
@Serializable
data class AttentionCounts(
    @SerialName("needs_action") val needsAction: Int = 0,
    val failed: Int = 0,
)

/**
 * The Attention badge, by the same rule every other surface uses.
 *
 * Pending HITL and the feed's needs-action count project the same work, so the
 * larger wins rather than both being counted; failed rows are then added once.
 * Ported from the web's `resolveAttentionBadgeCount`, which iOS also follows —
 * three surfaces disagreeing about how much is waiting would be worse than any
 * of them being slightly stale.
 */
fun resolveAttentionBadgeCount(pendingHitl: Int, needsAction: Int, failed: Int): Int =
    maxOf(maxOf(0, pendingHitl), needsAction) + maxOf(0, failed)

/**
 * Voice preferences the account carries, not the handset.
 *
 * Only the fields this client can honour. The surface profile and per-stage
 * maps are left out deliberately rather than round-tripped blind: sending back
 * a shape nothing here understands is how a client quietly overwrites a
 * choice made on another device.
 */
@Serializable
data class MediaPreferences(
    @SerialName("auto_speak") val autoSpeak: Boolean = false,
    @SerialName("tts_voices") val ttsVoices: Map<String, String> = emptyMap(),
    @SerialName("realtime_voices") val realtimeVoices: Map<String, String> = emptyMap(),
)

/**
 * A preferences write.
 *
 * Every field is optional on the server and absent means "leave alone", so
 * this carries only what changed. Sending the whole record would clobber
 * settings this client does not model.
 */
@Serializable
data class MediaPreferencesUpdate(
    @SerialName("auto_speak") val autoSpeak: Boolean,
)

/** What Settings shows for the backend. */
data class ServiceHealth(
    val reachable: Boolean,
    val version: String? = null,
    val detail: String? = null,
) {
    fun label(): String = when {
        reachable -> "Online"
        detail != null -> detail
        else -> "Unavailable"
    }
}

data class ServiceHealthStack(
    val magician: ServiceHealth,
    val magicutor: ServiceHealth,
    val desktop: ServiceHealth,
    /**
     * The supervisor reports a version and no status.
     *
     * Carried as a bare string rather than forced into a [ServiceHealth]: a
     * version is not a health check, and pretending it is would show
     * "Offline" for a service the endpoint never claimed to be watching.
     */
    val supervisorVersion: String? = null,
    /**
     * The magician-api crate's version, reported beside the core version.
     *
     * Older servers sent the API crate's version as `version`; newer ones send
     * the Magician core version there and this one separately. Null when the
     * server predates the split.
     */
    val apiVersion: String? = null,
)

/**
 * The `versions` block of root `/health`: one entry per component.
 *
 * Every field is optional. Older servers omit the block, and a component that
 * is not deployed (Magicutor, the desktop gateway) comes back null or absent.
 */
@Serializable
data class ServiceVersions(
    val magician: String? = null,
    @SerialName("magician_api") val magicianApi: String? = null,
    val magicutor: String? = null,
    val desktop: String? = null,
)

/** Root `/health`, shared with iOS; dependencies are reported by Magician. */
@Serializable
data class ServiceHealthBody(
    val version: String? = null,
    val magician: String? = null,
    @SerialName("magicutor_status") val magicutorStatus: String? = null,
    @SerialName("tauri_status") val tauriStatus: String? = null,
    // Read opportunistically, as iOS reads them: each alternate spelling has
    // been what the endpoint sent at some point, and a version absent from the
    // body means that service is not deployed rather than that it is broken.
    @SerialName("magicutor_version") val magicutorVersion: String? = null,
    @SerialName("supervisor_version") val supervisorVersion: String? = null,
    val supervisor: String? = null,
    @SerialName("tauri_version") val tauriVersion: String? = null,
    // Newer servers: `version` is the Magician core version, `api_version` the
    // magician-api crate's, and `versions` names every component. All
    // optional, so a body from an older server still decodes.
    @SerialName("api_version") val apiVersion: String? = null,
    val versions: ServiceVersions? = null,
) {
    fun stack(): ServiceHealthStack = ServiceHealthStack(
        magician = health(magician ?: "healthy", present(versions?.magician) ?: present(version)),
        magicutor = health(magicutorStatus, present(versions?.magicutor) ?: present(magicutorVersion)),
        desktop = health(tauriStatus, present(versions?.desktop) ?: present(tauriVersion)),
        supervisorVersion = supervisorVersion ?: supervisor,
        apiVersion = present(versions?.magicianApi) ?: present(apiVersion),
    )

    private fun present(value: String?): String? = value?.trim()?.takeIf(String::isNotEmpty)

    // The version rides every state: a server that names a component's version
    // knows which build is installed even while that component is down.
    private fun health(raw: String?, version: String? = null): ServiceHealth = when (
        raw?.trim()?.lowercase()
    ) {
        "healthy", "online", "ok", "ready" -> ServiceHealth(true, version)
        "offline", "unhealthy", "unreachable", "failed", "error" ->
            ServiceHealth(false, version, detail = "Offline")
        else -> ServiceHealth(false, version, detail = "Not reported")
    }
}

/**
 * Which message a shell chunk belongs to.
 *
 * The card that names the chunk's execution when one does; the newest task
 * card otherwise — iOS's fallback, kept because the first chunks of a run can
 * arrive before the card has learned its execution id. -1 when the transcript
 * has no task card at all, in which case the chunk has nowhere to land and is
 * dropped rather than invented a home.
 */
internal fun shellTargetIndex(messages: List<ChatMessage>, executionId: String): Int {
    val owned = messages.indexOfLast { it.task?.executionId == executionId }
    if (owned >= 0) return owned
    return messages.indexOfLast { it.kind == MessageKind.TaskStatus }
}

@Serializable
data class ConcurrentSessionOrigin(val kind: String = "", @SerialName("parent_session_id") val parentSessionId: String? = null)

@Serializable
data class QueuedTextMessage(val id: String, val text: String? = null,
    @SerialName("attachment_ids") val attachmentIds: List<String> = emptyList(),
    /** When it was queued (epoch seconds or ms); kept raw so a float cannot fail the snapshot. */
    @SerialName("queued_at") val queuedAtRaw: JsonPrimitive? = null) {
    /** Epoch milliseconds, when the server stated it. */
    val queuedAtMillis: Long?
        get() {
            val raw = queuedAtRaw ?: return null
            val value = if (raw.isString) raw.content.trim().toDoubleOrNull() else raw.doubleOrNull
            return value?.let { ChatDateDividers.epochMillis(it) }
        }
}
@Serializable
data class ChatQueueSnapshot(
    val queued: List<QueuedTextMessage> = emptyList(),
    val active: Boolean = false,
    /** The session's latest turn (`turn_state`), read off the same body by [ChatRepository.queueSnapshot]. */
    @kotlinx.serialization.Transient val turnState: ChatTurnStateReport = ChatTurnStateReport.UNSUPPORTED,
)
