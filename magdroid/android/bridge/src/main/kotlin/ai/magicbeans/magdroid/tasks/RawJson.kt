package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull

/**
 * Loose reads over a raw JSON payload, the Kotlin side of the iOS models'
 * `[String: Any]` casts: a value of the wrong shape reads as absent, never
 * throws. Shared by the task-detail and live-card models that read panel,
 * plan and usage payloads the server is still growing.
 */
internal fun JsonElement?.jObj(): JsonObject? = this as? JsonObject

internal fun JsonElement?.jArr(): JsonArray? = this as? JsonArray

/** The array's object rows, skipping anything else (`as? [[String: Any]]`, loosely). */
internal fun JsonElement?.jObjects(): List<JsonObject>? = jArr()?.mapNotNull { it as? JsonObject }

/** A JSON string only (`as? String`): numbers and booleans are not text. */
internal fun JsonElement?.jStr(): String? =
    (this as? JsonPrimitive)?.takeIf { it.isString }?.content

/** A trimmed, non-empty JSON string. */
internal fun JsonElement?.jText(): String? = jStr()?.trim()?.takeIf { it.isNotEmpty() }

/** A JSON number (`as? NSNumber`); strings are not numbers. */
internal fun JsonElement?.jNum(): Double? =
    (this as? JsonPrimitive)?.takeIf { !it.isString }?.let { it.doubleOrNull ?: it.booleanOrNull?.let { b -> if (b) 1.0 else 0.0 } }

/** An integer from a number, or from a numeric string. */
internal fun JsonElement?.jInt(): Int? {
    val primitive = this as? JsonPrimitive ?: return null
    if (primitive.isString) return primitive.content.trim().toIntOrNull()
    val value = primitive.doubleOrNull ?: return null
    if (!value.isFinite()) return null
    return value.toInt()
}

internal fun JsonElement?.jBool(): Boolean? =
    (this as? JsonPrimitive)?.takeIf { !it.isString }?.booleanOrNull
