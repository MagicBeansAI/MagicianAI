package ai.magicbeans.magdroid.work

import kotlinx.serialization.json.*

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
