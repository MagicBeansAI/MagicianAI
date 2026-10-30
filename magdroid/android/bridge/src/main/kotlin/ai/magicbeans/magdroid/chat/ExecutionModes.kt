package ai.magicbeans.magdroid.chat

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class ChatGodModeState(
    val enabled: Boolean,
    @SerialName("override") val overrideChoice: Boolean? = null,
    @SerialName("default") val defaultEnabled: Boolean,
    /** God mode's asks approved without asking, except deletes, root and unseen code. */
    @SerialName("auto_approve") val autoApprove: Boolean = false,
)

enum class SuperGodModeDuration(val wire: String) { Hour("1h"), UntilOff("forever") }

@Serializable
data class SuperGodModeState(
    val active: Boolean,
    val enabled: Boolean,
    @SerialName("until_ms") val untilMs: Long? = null,
    @SerialName("remaining_secs") val remainingSecs: Long? = null,
) {
    fun activeAt(nowMs: Long = System.currentTimeMillis()): Boolean = active && (untilMs == null || untilMs > nowMs)
    fun endLabel(nowMs: Long = System.currentTimeMillis()): String = untilMs?.let {
        "${maxOf(1, (it - nowMs + 59_999) / 60_000)} min left"
    } ?: "until turned off"
}
