package ai.magicbeans.magdroid.work

import kotlinx.serialization.json.*
import ai.magicbeans.magdroid.chat.ChatModelChoice
import ai.magicbeans.magdroid.chat.ChatModelSelection

/** Quality's bounded route catalog owns eligibility; the shared catalog owns model controls. */
class WorkQualityModels(quality: JsonElement?, catalog: JsonElement?) {
    private val decoder = Json { ignoreUnknownKeys = true }
    private val routes = rows(quality, "profiles")
    private val catalogRoutes = rows(catalog, "profiles")
    private val catalogModels = rows(catalog, "models").mapNotNull { runCatching { decoder.decodeFromJsonElement<ChatModelChoice>(it) }.getOrNull() }
    fun family(route: String): String? = routes.firstOrNull { text(it, "name") == route }?.let { text(it, "family") }
    fun models(excludedFamily: String? = null): List<ChatModelChoice> {
        val eligible = routes.filter { excludedFamily == null || (text(it, "family") != excludedFamily && ((it as? JsonObject)?.get("supports_json") as? JsonPrimitive)?.booleanOrNull != false) }.mapNotNull { text(it, "name") }.toSet()
        return catalogModels.filter { !it.provider.startsWith("harness-") }.mapNotNull { model ->
            val choices = (model.operationChoices ?: model.choices).filterValues { it in eligible }.toMutableMap()
            var defaultLevel = model.thinking.defaultLevel
            if ("auto" !in choices) {
                val assignment = catalogRoutes.firstOrNull { text(it, "name") in eligible && (((it as? JsonObject)?.get("model_selection") as? JsonObject)?.get("model_id") as? JsonPrimitive)?.contentOrNull == model.id }
                val route = assignment?.let { text(it, "name") }
                val level = (((assignment as? JsonObject)?.get("model_selection") as? JsonObject)?.get("thinking") as? JsonPrimitive)?.contentOrNull
                if (route != null) {
                    choices["auto"] = route
                    if (level in model.thinking.levels) { choices[requireNotNull(level)] = route; defaultLevel = level }
                }
            }
            if ("auto" !in choices) null else model.copy(choices = choices, operationChoices = null,
                thinking = model.thinking.copy(levels = model.thinking.levels.filter { it in choices }, defaultLevel = defaultLevel))
        }
    }
    fun selection(route: String, models: List<ChatModelChoice>): ChatModelSelection? {
        models.firstOrNull { it.choices["auto"] == route }?.let { return ChatModelSelection(it.id, "auto") }
        val saved = (catalogRoutes.firstOrNull { text(it, "name") == route } as? JsonObject)?.get("model_selection")
            ?.let { runCatching { decoder.decodeFromJsonElement<ChatModelSelection>(it) }.getOrNull() }
        if (saved != null && models.any { it.id == saved.modelId && saved.thinking in it.choices }) return saved
        return models.firstNotNullOfOrNull { model -> model.choices.entries.firstOrNull { it.value == route }?.let { ChatModelSelection(model.id, it.key) } }
    }
    fun valid(route: String, models: List<ChatModelChoice>): Boolean = family(route) != null && selection(route, models)?.let { selected -> models.any { it.id == selected.modelId && it.installed } } == true
    private fun rows(value: JsonElement?, key: String): List<JsonElement> = ((value as? JsonObject)?.get(key) as? JsonArray)?.toList().orEmpty()
    private fun text(value: JsonElement, key: String): String? = ((value as? JsonObject)?.get(key) as? JsonPrimitive)?.contentOrNull
}

data class WorkSourceReference(val id: String, val title: String, val origin: String, val content: String) {
    val source: String get() = "Reference: $title\nOrigin: $origin\n\n$content"
}

/** Existing Relentless contract, matching the web/iOS guided draft defaults. */
data class WorkQualityDraft(
    val goal: String = "", val source: String = "", val rubric: String = "", val filename: String = "draft.md",
    val references: List<WorkSourceReference> = emptyList(),
    val actor: String = "", val critic: String = "", val attempts: Int = 5, val minutes: Int = 30, val dollars: Int = 40,
) {
    fun proposal(): JsonObject = buildJsonObject {
        put("goal", goal); put("artifact_file", filename); put("maximum_artifact_bytes", 65536)
        put("human_review", "required"); put("handoff_kind", "internal_draft")
        put("actor_profile", actor); putJsonArray("critic_profiles") { repeat(3) { add(critic) } }
        putJsonObject("sources") { references.forEach { put(it.id, it.source) }; if (source.isNotBlank()) put("brief", source) }
        putJsonArray("criteria") {
            add(buildJsonObject {
                put("id", "readable_artifact")
                putJsonObject("specification") {
                    put("kind", "check")
                    putJsonArray("assertions") {
                        add(buildJsonObject { put("kind", "utf8"); put("file", filename) })
                        add(buildJsonObject { put("kind", "byte_length"); put("file", filename); put("minimum", 1); put("maximum", 65536) })
                    }
                }
                put("weight", JsonNull); put("must_pass", true); put("pass_score", JsonNull)
            })
            add(buildJsonObject {
                put("id", "grounding")
                putJsonObject("specification") {
                    put("kind", "grounding")
                    put("instructions", "Review all factual claims against the sealed source notes. Unsupported claims must be removed or corrected; uncertainty must remain visible.")
                }
                put("weight", JsonNull); put("must_pass", true); put("pass_score", 1)
            })
            add(buildJsonObject {
                put("id", "quality")
                putJsonObject("specification") {
                    put("kind", "rubric"); put("instructions", rubric)
                    putJsonArray("anchors") {
                        add(buildJsonObject { put("score", 0); put("description", "Does not satisfy the requested outcome; major omissions or unusable output.") })
                        add(buildJsonObject { put("score", 0.5); put("description", "Useful draft with substantive gaps against the stated requirements.") })
                        add(buildJsonObject { put("score", 1); put("description", "Fully satisfies the stated requirements; complete and ready for the intended local review.") })
                    }
                }
                put("weight", 1); put("must_pass", false); put("pass_score", JsonNull)
            })
        }
        put("score_at_least", 0.8)
        putJsonObject("review") {
            put("panel_size", 3); put("score_quorum", 3); put("min_confidence", 0.8)
            put("max_score_spread", 0.2); put("noise_margin", 0.02); put("max_retries_per_critic", 1)
            put("unknown_review", "revise_supported_defects")
        }
        putJsonObject("budget") {
            put("max_attempts", attempts); put("max_wall_clock_secs", minutes * 60L)
            put("max_cost_microusd", dollars * 1_000_000L); put("plateau_attempts", 3)
        }
    }
}
