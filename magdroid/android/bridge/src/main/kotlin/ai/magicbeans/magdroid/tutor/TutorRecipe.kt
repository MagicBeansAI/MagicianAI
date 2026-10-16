package ai.magicbeans.magdroid.tutor

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.intOrNull

/**
 * One value in a draw-op's parameter bag.
 *
 * A parameter is a bare number, a string — which may be a field reference, a
 * coalesce chain, an expression, or a plain literal — or a possibly-nested
 * array holding an `[x, y]` pair or a point list. Decoding is deliberately
 * permissive: these come from a server and from per-scope files, and a recipe
 * shape this build has never seen must cost its own op rather than the lesson.
 */
sealed interface RecipeValue {
    data class Num(val value: Double) : RecipeValue
    data class Str(val value: String) : RecipeValue
    data class Arr(val values: List<RecipeValue>) : RecipeValue

    companion object {
        /** Best-effort decode. Booleans become 1/0, which [RecipeEnvironment.bool] reads back. */
        fun from(element: JsonElement): RecipeValue? = when (element) {
            is JsonArray -> Arr(element.mapNotNull { from(it) })
            is JsonPrimitive -> when {
                // Checked before number so `true` does not become a chance 1.
                !element.isString && element.booleanOrNull != null ->
                    Num(if (element.booleanOrNull == true) 1.0 else 0.0)

                !element.isString && element.doubleOrNull != null -> Num(element.double())
                element.isString -> Str(element.content)
                else -> null
            }

            else -> null
        }

        private fun JsonPrimitive.double(): Double = doubleOrNull ?: 0.0
    }
}

/**
 * One draw op: a name plus its arguments.
 *
 * `op` is reserved and stripped from [params], so the bag holds only arguments.
 */
data class RecipeOp(val op: String, val params: Map<String, RecipeValue>) {
    companion object {
        fun from(element: JsonElement): RecipeOp? {
            val obj = element as? JsonObject ?: return null
            var name = ""
            val params = mutableMapOf<String, RecipeValue>()
            for ((key, value) in obj) {
                if (key == "op") {
                    name = (value as? JsonPrimitive)?.takeIf { it.isString }?.content.orEmpty()
                } else {
                    RecipeValue.from(value)?.let { params[key] = it }
                }
            }
            return RecipeOp(name, params)
        }
    }
}

/**
 * A declarative primitive: which shape `type` (and aliases) it draws, defaults
 * for absent fields, and an ordered list of ops.
 *
 * This is the whole point of the recipe system — a scope can author a new
 * primitive as a JSON file and every client draws it without shipping a build.
 * Mirrors `TutorRecipe.swift`.
 */
data class TutorRecipe(
    val type: String,
    val aliases: List<String> = emptyList(),
    val version: Int? = null,
    val defaults: Map<String, Double> = emptyMap(),
    val draw: List<RecipeOp> = emptyList(),
) {
    /** Every type string this recipe answers to, lowercased. */
    val matchedTypes: List<String> get() = (listOf(type) + aliases).map { it.lowercase() }

    companion object {
        /** At most this many recipes are indexed, per the interpreter contract. */
        const val MAX_RECIPES = 512

        private val json = Json { ignoreUnknownKeys = true; isLenient = true }

        fun from(element: JsonElement): TutorRecipe? {
            val obj = element as? JsonObject ?: return null
            val type = (obj["type"] as? JsonPrimitive)?.takeIf { it.isString }?.content ?: return null
            return TutorRecipe(
                type = type,
                aliases = (obj["aliases"] as? JsonArray)
                    ?.mapNotNull { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.content }
                    .orEmpty(),
                version = (obj["version"] as? JsonPrimitive)?.intOrNull,
                defaults = (obj["defaults"] as? JsonObject)
                    ?.mapNotNull { (key, value) ->
                        (value as? JsonPrimitive)?.doubleOrNull?.let { key to it }
                    }
                    ?.toMap()
                    .orEmpty(),
                draw = (obj["draw"] as? JsonArray)?.mapNotNull { RecipeOp.from(it) }.orEmpty(),
            )
        }

        /**
         * Decode a recipe set: either a bare array or `{ "primitives": [...] }`,
         * both of which the endpoint has served.
         *
         * A single malformed entry is dropped rather than failing the set — one
         * bad per-scope file must not cost every other primitive.
         */
        fun decodeList(raw: String): List<TutorRecipe>? {
            val root = runCatching { json.parseToJsonElement(raw) }.getOrNull() ?: return null
            val array = when (root) {
                is JsonArray -> root
                is JsonObject -> root["primitives"] as? JsonArray ?: return null
                else -> return null
            }
            return array.mapNotNull { from(it) }.take(MAX_RECIPES)
        }
    }
}
