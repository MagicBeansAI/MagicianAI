package ai.magicbeans.magdroid.attention

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull

/**
 * The owner-private read of one preference alignment hold
 * (`GET /hitl/{id}/alignment?surface=android`).
 *
 * Android joined web and iOS as an owner-private surface on 2026-10-11, so it
 * reads why the act was held and can change what Magician remembers. Ports
 * iOS's `AlignmentPrivateProjection` (`magios/Shared/AlignmentAsk.swift`).
 * Lock-screen and push text stay generic: only this authenticated read carries
 * the reason.
 */
data class AlignmentDetailChoice(
    val id: String,
    val label: String,
    /** Changes what Magician remembers rather than deciding the act. */
    val lasting: Boolean = false,
    val consent: Boolean = false,
    /** The owner types new wording first (change a preference). */
    val needsWording: Boolean = false,
)

data class AlignmentIntention(val id: String, val statement: String, val assertionStatus: String?) {
    /** Who the preference came from, in the owner's words. */
    val sourceLabel: String
        get() = when (assertionStatus) {
            "direct_owner_statement", "confirmed" -> "You said"
            "inferred" -> "Magician noticed"
            else -> "Noted"
        }
}

/** The exact wording the server read back, bound to [token]. */
data class AlignmentReadback(val kind: String, val wording: String, val superseded: String?, val token: String)

/** Another gate this ask also settles, with that gate's own reason. */
data class AlignmentCoveredRequirement(val gate: String, val reason: String) {
    val line: String
        get() = reason.ifEmpty {
            when (gate) {
                "approval_rule", "plane_approval" -> "One of your approval rules matched it."
                "criticality" -> "It is a high-impact action."
                else -> "It was marked as needing confirmation."
            }
        }
}

data class AlignmentRemember(val id: String, val statement: String)

data class AlignmentPrivateProjection(
    val stage: String?,
    val explanation: String?,
    val actSummary: String?,
    val intentions: List<AlignmentIntention>,
    val remember: AlignmentRemember?,
    val choices: List<AlignmentDetailChoice>,
    val readback: AlignmentReadback?,
    val deadlineAtMs: Long?,
    val alsoCovers: List<AlignmentCoveredRequirement>,
) {
    val isWordingStage: Boolean get() = stage == "confirm_wording"

    /** What to do with this act. Never a lasting change. */
    val actChoices: List<AlignmentDetailChoice>
        get() = choices.filter { !it.lasting && it.id != "keep_as_is" }

    /** What Magician remembers: kept apart from the act's answer. */
    val lastingChoices: List<AlignmentDetailChoice> get() = choices.filter { it.lasting }
}

data class AlignmentDetail(
    val isPrivate: Boolean,
    val retryReason: String?,
    val alignment: AlignmentPrivateProjection,
)

/** How a private read ended, for the sheet to show. */
sealed interface AlignmentDetailOutcome {
    data class Ready(val detail: AlignmentDetail) : AlignmentDetailOutcome

    /** Already answered or replaced: the owner should open the current one. */
    data object Gone : AlignmentDetailOutcome

    /** Not readable here; the act's own choices still work. */
    data object Unavailable : AlignmentDetailOutcome
}

private fun JsonElement?.text(): String? =
    (this as? JsonPrimitive)?.takeIf { it.isString }?.content?.trim()?.takeIf { it.isNotEmpty() }

private fun JsonElement?.flag(): Boolean = (this as? JsonPrimitive)?.booleanOrNull == true

private fun JsonElement?.objects(): List<JsonObject> = (this as? JsonArray).orEmpty().mapNotNull { it as? JsonObject }

/**
 * Read a private detail body leniently: an unreadable body is `null` (the
 * sheet falls back to the act's own choices), and a malformed entry inside it
 * is skipped rather than failing the whole read.
 */
fun parseAlignmentDetail(body: String): AlignmentDetail? {
    val root = runCatching { Json.parseToJsonElement(body) }.getOrNull() as? JsonObject ?: return null
    val block = root["alignment"] as? JsonObject ?: return null
    val projection = AlignmentPrivateProjection(
        stage = block["stage"].text(),
        explanation = block["explanation"].text(),
        actSummary = block["act_summary"].text(),
        intentions = block["intentions"].objects().mapNotNull { row ->
            val id = row["id"].text() ?: return@mapNotNull null
            val statement = row["statement"].text() ?: return@mapNotNull null
            AlignmentIntention(id, statement, row["assertion_status"].text())
        },
        remember = (block["remember"] as? JsonObject)?.let { row ->
            val id = row["id"].text() ?: return@let null
            val statement = row["statement"].text() ?: return@let null
            AlignmentRemember(id, statement)
        },
        choices = block["choices"].objects().mapNotNull { row ->
            val id = row["id"].text() ?: return@mapNotNull null
            val label = row["label"].text() ?: return@mapNotNull null
            AlignmentDetailChoice(
                id = id,
                label = label,
                lasting = row["lasting"].flag(),
                consent = row["consent"].flag(),
                needsWording = row["needs_wording"].flag(),
            )
        },
        readback = (block["readback"] as? JsonObject)?.let { row ->
            val wording = row["wording"].text() ?: return@let null
            val token = row["token"].text() ?: return@let null
            AlignmentReadback(row["kind"].text() ?: "preference", wording, row["superseded"].text(), token)
        },
        deadlineAtMs = (block["deadline_at_ms"] as? JsonPrimitive)?.longOrNull,
        alsoCovers = block["also_covers"].objects().mapNotNull { row ->
            val gate = row["gate"].text() ?: return@mapNotNull null
            AlignmentCoveredRequirement(gate, row["reason"].text().orEmpty())
        },
    )
    return AlignmentDetail(
        isPrivate = root["private"].flag(),
        retryReason = root["retry_reason"].text(),
        alignment = projection,
    )
}

/** How long the owner has to answer, or `null` when unknown. */
fun alignmentDeadlineLine(deadlineAtMs: Long?, nowMs: Long = System.currentTimeMillis()): String? {
    val ms = deadlineAtMs ?: return null
    val leftSeconds = (ms - nowMs) / 1000.0
    if (leftSeconds <= 0) return "The answer window has closed; a late answer runs nothing."
    val minutes = Math.round(leftSeconds / 60).toInt()
    if (minutes < 60) return "Answer within ${maxOf(minutes, 1)} min; after that it won't run."
    return "Answer within ${Math.round(minutes / 60.0)} h; after that it won't run."
}
