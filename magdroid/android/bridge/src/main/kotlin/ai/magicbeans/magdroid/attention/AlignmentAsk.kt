package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.HitlResponseValue
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull

/**
 * A preference alignment hold on Android (P6.3).
 *
 * A hold is an ordinary `confirmation` ask whose `input_schema` carries
 * `escalation_trigger: "preference_alignment"` and an `alignment` block: the
 * public projection — choice ids, non-revealing labels, deadline, stage.
 *
 * This block is the public projection, safe on any surface. Android is an
 * owner-private surface since 2026-10-11, so the answer sheet also reads the
 * private detail ([AlignmentDetail]): why the act was held, and the lasting
 * changes, whose exact wording is read back first. When that read fails, the
 * act's own typed choices and a reply still work. Mirrors the web's
 * `alignmentAsk.ts` and iOS's `AlignmentAsk.swift`.
 */
data class AlignmentChoice(val id: String, val label: String)

data class AlignmentProjection(
    val askId: String,
    val choices: List<AlignmentChoice>,
    val freeText: Boolean,
    val deadlineAtMs: Long?,
    val stage: String,
    /**
     * Other gates this one ask also settles (a combined ask): gate names only,
     * e.g. `approval_rule`. Allowing the act approves it for them too.
     */
    val alsoCovers: List<String> = emptyList(),
) {
    val isWordingStage: Boolean get() = stage == "confirm_wording"

    /**
     * The act's choices, offered when the private detail cannot be read. A
     * lasting change needs the private readback, so it is never offered from
     * the public projection alone.
     */
    val actChoices: List<AlignmentChoice>
        get() = choices.filter { it.id !in LASTING_CHOICE_IDS && it.id != "keep_as_is" }

    companion object {
        val LASTING_CHOICE_IDS = setOf("change_preference", "remember_preference", "confirm_wording")
        const val ALSO_APPROVES_NOTE =
            "This also needs your approval. Allowing it approves it too; any other answer leaves it unapproved."
        const val DETAIL_UNAVAILABLE_NOTE =
            "The reason could not be loaded here. Your answer still works."
    }
}

private fun JsonElement?.string(): String? =
    (this as? JsonPrimitive)?.takeIf { it.isString }?.content?.trim()?.takeIf { it.isNotEmpty() }

/**
 * Read `input_schema.alignment` leniently: a malformed block reads as "not a
 * hold" rather than failing the row, the same rule the rest of the
 * attention metadata follows.
 */
fun parseAlignmentProjection(raw: JsonElement?): AlignmentProjection? {
    val block = raw as? JsonObject ?: return null
    if (block["kind"].string() != "preference_alignment") return null
    val askId = block["ask_id"].string() ?: return null
    val choices = (block["choices"] as? JsonArray).orEmpty().mapNotNull { entry ->
        val choice = entry as? JsonObject ?: return@mapNotNull null
        val id = choice["id"].string() ?: return@mapNotNull null
        val label = choice["label"].string() ?: return@mapNotNull null
        AlignmentChoice(id, label)
    }
    if (choices.isEmpty()) return null
    return AlignmentProjection(
        askId = askId,
        choices = choices,
        freeText = (block["free_text"] as? JsonPrimitive)?.booleanOrNull != false,
        deadlineAtMs = (block["deadline_at_ms"] as? JsonPrimitive)?.longOrNull,
        stage = block["stage"].string() ?: "decide",
        alsoCovers = (block["also_covers"] as? JsonArray).orEmpty().mapNotNull { it.string() },
    )
}

/** An answer to a hold: a typed choice, or the owner's words (never consent). */
object AlignmentAnswers {
    fun choice(id: String): HitlResponseValue = HitlResponseValue.Choice(selectedId = id)

    fun reply(words: String): HitlResponseValue? =
        words.trim().takeIf { it.isNotEmpty() }?.let { HitlResponseValue.Text(it) }
}
