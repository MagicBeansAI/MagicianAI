package ai.magicbeans.magdroid.vibedev

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** A project's health level, and the tone the app colours it with. */
enum class VibeDevHealthLevel { Ok, Warning, Error;

    companion object {
        fun fromWire(value: String?): VibeDevHealthLevel? = when (value?.trim()?.lowercase()) {
            "ok" -> Ok
            "warning" -> Warning
            "error" -> Error
            else -> null
        }
    }
}

/** A one-tap fix the server offers on an issue; only [isSupported] kinds get a button. */
data class VibeDevHealthAction(val kind: String, val label: String) {
    val isSupported: Boolean get() = kind == GIT_INIT

    companion object {
        /** `POST /vibedev/projects/{id}/git-init`. */
        const val GIT_INIT = "git_init"
    }
}

/**
 * One thing wrong with a project; [code] is opaque. [title] is the short
 * headline (absent from older servers), [message] the explanation.
 */
data class VibeDevHealthIssue(
    val code: String,
    val severity: VibeDevHealthLevel,
    val message: String,
    val title: String? = null,
    val action: VibeDevHealthAction? = null,
)

/**
 * A project's `health` (`{ status, issues[] }`), read leniently: absent or
 * unreadable is ok; an unknown status falls back to the worst issue's
 * severity; an issue with no message is dropped.
 */
data class VibeDevProjectHealth(
    val level: VibeDevHealthLevel = VibeDevHealthLevel.Ok,
    val issues: List<VibeDevHealthIssue> = emptyList(),
) {
    val needsAttention: Boolean get() = level != VibeDevHealthLevel.Ok

    /** Web wording; null when the project is fine. */
    val title: String?
        get() = when (level) {
            VibeDevHealthLevel.Error -> "Project needs attention: runs cannot start."
            VibeDevHealthLevel.Warning -> "Project needs attention."
            VibeDevHealthLevel.Ok -> null
        }

    companion object {
        const val GIT_INIT_CONFIRM_TITLE = "Make this folder a git repository?"
        const val GIT_INIT_CONFIRM_BODY = "Magician runs git init in the project folder and commits its current files " +
            "as a first commit on the base branch (main if none is set). Branch runs, merge, pull requests and " +
            "rewind then work for this trusted project."
        const val GIT_INIT_CONFIRM_ACTION = "Make it a git repository"
        const val ATTENTION_DESCRIPTION = "Project needs attention"
        val OK = VibeDevProjectHealth()

        fun decode(wire: JsonElement?): VibeDevProjectHealth {
            val record = wire as? JsonObject ?: return OK
            val issues = (record["issues"] as? JsonArray).orEmpty().mapNotNull { item ->
                val issue = item as? JsonObject ?: return@mapNotNull null
                val message = issue.string("message")?.trim()?.takeIf(String::isNotEmpty) ?: return@mapNotNull null
                val severity = VibeDevHealthLevel.fromWire(issue.string("severity"))
                    ?.takeIf { it != VibeDevHealthLevel.Ok } ?: VibeDevHealthLevel.Warning
                val action = (issue["action"] as? JsonObject)?.let { wire ->
                    val kind = wire.string("kind")?.trim()?.takeIf(String::isNotEmpty)
                    val label = wire.string("label")?.trim()?.takeIf(String::isNotEmpty)
                    if (kind != null && label != null) VibeDevHealthAction(kind, label) else null
                }
                VibeDevHealthIssue(
                    issue.string("code").orEmpty(), severity, message,
                    title = issue.string("title")?.trim()?.takeIf(String::isNotEmpty),
                    action = action,
                )
            }
            val level = VibeDevHealthLevel.fromWire(record.string("status")) ?: when {
                issues.any { it.severity == VibeDevHealthLevel.Error } -> VibeDevHealthLevel.Error
                issues.isNotEmpty() -> VibeDevHealthLevel.Warning
                else -> VibeDevHealthLevel.Ok
            }
            return VibeDevProjectHealth(level, issues)
        }

        private fun JsonObject.string(key: String): String? =
            (get(key) as? JsonPrimitive)?.takeIf { it.isString }?.content
    }
}
