package ai.magicbeans.magdroid.glance

import ai.magicbeans.magdroid.today.TodayResponse
import android.content.Context
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
enum class MagicanGlanceFocus {
    @SerialName("needs_you") NeedsYou,
    @SerialName("active_work") ActiveWork,
    @SerialName("ready") Ready,
}

/** Privacy-safe reduction shared by the Android widget and push receiver. */
@Serializable
data class MagicanGlanceSnapshot(
    val generatedAt: Long = 0,
    val focus: MagicanGlanceFocus = MagicanGlanceFocus.Ready,
    val title: String = "Ready when you are",
    val subtitle: String = "Talk to Magican",
    val needsYouCount: Int = 0,
    val activeWorkCount: Int = 0,
    val taskId: String? = null,
) {
    val destination: String get() = when (focus) {
        MagicanGlanceFocus.NeedsYou -> "magican://attention"
        MagicanGlanceFocus.ActiveWork -> taskId?.takeIf(String::isNotBlank)
            ?.let { "magican://task/${android.net.Uri.encode(it)}" }
            ?: "magican://today?tab=active_work"
        MagicanGlanceFocus.Ready -> "magican://talk"
    }

    fun hasSameVisibleContent(other: MagicanGlanceSnapshot): Boolean =
        focus == other.focus && title == other.title && subtitle == other.subtitle &&
            needsYouCount == other.needsYouCount && activeWorkCount == other.activeWorkCount &&
            taskId == other.taskId

    companion object {
        fun reduce(today: TodayResponse): MagicanGlanceSnapshot {
            val needs = today.counts.needsYou.coerceAtLeast(0)
            val active = today.counts.activeWork.coerceAtLeast(0)
            if (needs > 0) {
                return MagicanGlanceSnapshot(
                    generatedAt = today.generatedAt,
                    focus = MagicanGlanceFocus.NeedsYou,
                    title = "$needs need${if (needs == 1) "s" else ""} you",
                    subtitle = safeLine(today.sections.needsYou.firstOrNull()?.title)
                        ?: "Open Attention",
                    needsYouCount = needs,
                    activeWorkCount = active,
                )
            }
            if (active > 0) {
                val first = today.sections.activeWork.firstOrNull()
                return MagicanGlanceSnapshot(
                    generatedAt = today.generatedAt,
                    focus = MagicanGlanceFocus.ActiveWork,
                    title = safeLine(first?.title) ?: "Work in progress",
                    subtitle = safeLine(first?.reason) ?: safeLine(first?.status) ?: "Working…",
                    needsYouCount = needs,
                    activeWorkCount = active,
                    taskId = first?.taskId?.trim()?.takeIf(String::isNotEmpty),
                )
            }
            return MagicanGlanceSnapshot(generatedAt = today.generatedAt)
        }

        private fun safeLine(raw: String?): String? = raw
            ?.trim()
            ?.split(Regex("\\s+"))
            ?.filter(String::isNotEmpty)
            ?.joinToString(" ")
            ?.take(96)
            ?.takeIf(String::isNotEmpty)
    }
}

object MagicanGlanceStore {
    private const val PREFS = "magican_glance_v1"
    private const val SNAPSHOT = "snapshot"
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    @Synchronized
    fun load(context: Context): MagicanGlanceSnapshot {
        val raw = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getString(SNAPSHOT, null) ?: return MagicanGlanceSnapshot()
        return runCatching { json.decodeFromString(MagicanGlanceSnapshot.serializer(), raw) }
            .getOrDefault(MagicanGlanceSnapshot())
    }

    @Synchronized
    fun save(context: Context, snapshot: MagicanGlanceSnapshot): Boolean {
        val previous = load(context)
        if (!shouldReplaceGlanceSnapshot(previous, snapshot)) return false
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putString(SNAPSHOT, json.encodeToString(MagicanGlanceSnapshot.serializer(), snapshot))
            .apply()
        return !previous.hasSameVisibleContent(snapshot)
    }
}

internal fun shouldReplaceGlanceSnapshot(
    current: MagicanGlanceSnapshot,
    incoming: MagicanGlanceSnapshot,
): Boolean = incoming.generatedAt >= current.generatedAt
