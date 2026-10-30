package ai.magicbeans.magdroid.preferences

/**
 * Engine-picker wording for an engine row's `credentials` field
 * (`GET /plane/engines`, `GET /coding/models`): `login`, `magician_key` or
 * `none`; anything else (or absent) has no label.
 */
object HarnessCredentials {
    fun label(value: String?): String? = when (value) {
        "login" -> "your login"
        "magician_key" -> "Magician’s key"
        "none" -> "not signed in"
        else -> null
    }

    /** `Codex · your login`; the bare name when there is no label. */
    fun withCredentials(name: String, value: String?): String =
        label(value)?.let { "$name · $it" } ?: name

    /** A picker's engine name with its label; Magician's own engine never carries one. */
    fun engineName(engineId: String, name: String, value: String?): String =
        if (engineId == "magician") name else withCredentials(name, value)
}
