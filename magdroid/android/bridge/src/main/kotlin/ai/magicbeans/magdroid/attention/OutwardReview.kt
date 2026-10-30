package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.EscalationOption
import ai.magicbeans.magdroid.chat.HitlResponseValue
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.longOrNull

/** `input_schema.escalation_trigger` of a captured send waiting for review. */
const val OUTWARD_REVIEW_TRIGGER = "outward_review"

/**
 * A captured outward send waiting for the owner's review
 * (docs/plans/2026-10-25-outward-send-review-design.md, Part 2).
 *
 * An ordinary agentic `confirmation` whose `input_schema` carries
 * `escalation_trigger: "outward_review"`, three `options` (Send, Discard,
 * Always allow this agent to send to this recipient) and an `outward_review`
 * record. Answered with the chosen option id as a `choice` value, the shape
 * tool authorizations already post. Every field is optional on the wire: an
 * older server sends none of it, and a missing record still reads as a
 * review once the trigger names one.
 */
data class OutwardReview(
    val capability: String? = null,
    val action: String? = null,
    /** The send's class (`message`, …): `class` on the wire. */
    val sendClass: String? = null,
    val recipients: List<String> = emptyList(),
    val agentId: String? = null,
    /** Null when the server sent none, or `0`. */
    val deadlineAtMs: Long? = null,
    /** The option ids the server listed; empty when it listed none. */
    val offeredIds: List<String> = emptyList(),
    val sendLabel: String = "Send",
    val discardLabel: String = "Discard",
    /** The server's own wording, e.g. "Always allow ambassador to send to a@b.com". Null when not offered. */
    val alwaysAllowLabel: String? = null,
) {
    /** "To a@b.com, c@d.com", or null without recipients. Three named at most. */
    val recipientsLine: String?
        get() {
            if (recipients.isEmpty()) return null
            val named = recipients.take(MAX_NAMED_RECIPIENTS).joinToString(", ")
            val more = recipients.size - MAX_NAMED_RECIPIENTS
            return if (more > 0) "To $named and $more more" else "To $named"
        }

    /** "ambassador · gmail send", from whatever of the agent and call were sent. */
    val viaLine: String?
        get() {
            val call = listOfNotNull(capability, action).joinToString(" ").takeIf { it.isNotEmpty() }
            return listOfNotNull(agentId, call).joinToString(" · ").takeIf { it.isNotEmpty() }
        }

    /** The value a choice posts: the option id, or the plain confirmation an unlisted Send / Discard falls back to. */
    fun answer(choiceId: String): HitlResponseValue = when {
        choiceId in offeredIds -> HitlResponseValue.Choice(selectedId = choiceId)
        choiceId == SEND -> HitlResponseValue.Confirmation(true)
        choiceId == DISCARD -> HitlResponseValue.Confirmation(false)
        else -> HitlResponseValue.Choice(selectedId = choiceId)
    }

    fun sendAnswer(): HitlResponseValue = answer(SEND)
    fun discardAnswer(): HitlResponseValue = answer(DISCARD)

    /** Null when the server did not offer a lasting grant. */
    fun alwaysAllowAnswer(): HitlResponseValue? = alwaysAllowLabel?.let { answer(ALWAYS_ALLOW_RECIPIENT) }

    companion object {
        const val SEND = "send"
        const val DISCARD = "discard"
        const val ALWAYS_ALLOW_RECIPIENT = "always_allow_recipient"
        private const val MAX_NAMED_RECIPIENTS = 3
    }
}

private fun JsonElement?.text(): String? =
    (this as? JsonPrimitive)?.takeIf { it.isString }?.content?.trim()?.takeIf { it.isNotEmpty() }

/**
 * Read an outward review off a confirmation's schema, or null when it is not
 * one. A malformed `outward_review` record reads as an empty one rather than
 * failing the row: the trigger alone is what makes it a review.
 */
fun parseOutwardReview(
    inputType: String,
    escalationTrigger: String?,
    options: List<EscalationOption>,
    raw: JsonElement?,
): OutwardReview? {
    if (inputType != "confirmation" || escalationTrigger?.trim() != OUTWARD_REVIEW_TRIGGER) return null
    val record = raw as? JsonObject
    fun option(id: String) = options.firstOrNull { it.id == id }
    val alwaysAllow = option(OutwardReview.ALWAYS_ALLOW_RECIPIENT)
    return OutwardReview(
        capability = record?.get("capability").text(),
        action = record?.get("action").text(),
        sendClass = record?.get("class").text(),
        recipients = (record?.get("recipients") as? JsonArray).orEmpty().mapNotNull { it.text() },
        agentId = record?.get("agent_id").text(),
        deadlineAtMs = (record?.get("deadline_at_ms") as? JsonPrimitive)?.longOrNull?.takeIf { it > 0 },
        offeredIds = options.map { it.id }.filter { it.isNotBlank() },
        sendLabel = option(OutwardReview.SEND)?.label?.trim()?.takeIf { it.isNotEmpty() } ?: "Send",
        discardLabel = option(OutwardReview.DISCARD)?.label?.trim()?.takeIf { it.isNotEmpty() } ?: "Discard",
        alwaysAllowLabel = alwaysAllow?.let { offered ->
            offered.label.trim().takeIf { it.isNotEmpty() } ?: "Always allow this agent to send to this recipient"
        },
    )
}

/**
 * The muted "Expires …" line under a review: relative within a day, the
 * date beyond it (the window is seven days). Past it the send is settled as
 * not sent, which is what the line says. Null without a deadline.
 */
fun outwardReviewExpiresLine(
    deadlineAtMs: Long?,
    nowMs: Long = System.currentTimeMillis(),
    zone: ZoneId = ZoneId.systemDefault(),
    locale: Locale = Locale.getDefault(),
): String? {
    val ms = deadlineAtMs?.takeIf { it > 0 } ?: return null
    val left = ms - nowMs
    if (left <= 0) return "Expired; it will not be sent."
    val minutes = Math.round(left / 60_000.0).toInt()
    if (minutes < 60) return "Expires in ${maxOf(minutes, 1)} min"
    val hours = Math.round(minutes / 60.0).toInt()
    if (hours < 24) return "Expires in $hours h"
    val at = DateTimeFormatter.ofPattern("EEE d MMM, h:mm a", locale).withZone(zone).format(Instant.ofEpochMilli(ms))
    return "Expires $at"
}
