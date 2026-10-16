package ai.magicbeans.magdroid.log

import kotlin.math.ceil
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Bounded, process-local history of tool calls served by the Android bridge.
 *
 * Entries stay in memory only: the App Pilot activity and performance views
 * need recent operational evidence, not a durable log that could accumulate
 * arguments or credentials on disk. The stored entry deliberately contains
 * only the tool name, timing, outcome, and coarse category.
 */
object CommandLog {

    enum class Category { GESTURE, OBSERVE, WAIT, INPUT, MANAGE }

    data class Entry(
        val timestamp: Long,
        val command: String,
        val latencyMs: Int,
        val success: Boolean,
        val category: Category,
    )

    data class Stats(
        val count: Int,
        val p50: Int,
        val p95: Int,
        val p99: Int,
    )

    private const val CAPACITY = 300

    private val _entries = MutableStateFlow<List<Entry>>(emptyList())

    /** Chronological and bounded; presentation surfaces choose their ordering. */
    val entries: StateFlow<List<Entry>> = _entries.asStateFlow()

    @Synchronized
    fun add(entry: Entry) {
        val next = _entries.value + entry
        // Drop the oldest call so a long-lived bridge always describes its
        // most recent work without growing process memory indefinitely.
        _entries.value = if (next.size > CAPACITY) next.takeLast(CAPACITY) else next
    }

    @Synchronized
    fun clear() {
        _entries.value = emptyList()
    }

    /** Number of retained commands, including deliberate wait operations. */
    fun size(): Int = _entries.value.size

    /** Return at most [limit] retained calls, newest first. */
    fun getRecent(limit: Int): List<Entry> {
        if (limit <= 0) return emptyList()
        return _entries.value.asReversed().take(limit)
    }

    /** Latency percentiles over the retained bounded window. */
    fun getPerformanceStats(): Stats {
        val latencies = _entries.value
            .filterNot { it.command == "android_wait_for_idle" }
            .map(Entry::latencyMs)
            .sorted()
        if (latencies.isEmpty()) return Stats(count = 0, p50 = 0, p95 = 0, p99 = 0)
        return Stats(
            count = latencies.size,
            p50 = percentile(latencies, 0.50),
            p95 = percentile(latencies, 0.95),
            p99 = percentile(latencies, 0.99),
        )
    }

    /** Preserve the category vocabulary used by the original MCP bridge. */
    fun categoryFor(command: String): Category = when (command) {
        "android_tap",
        "android_long_press",
        "android_double_tap",
        "android_swipe",
        "android_pinch",
        "android_drag" -> Category.GESTURE

        "android_get_ui_tree",
        "android_screenshot",
        "android_find_elements",
        "android_get_screen_context",
        "android_get_notifications",
        "android_await_otp",
        "android_screenshot_diff",
        "android_accessibility_audit",
        "android_get_recent_toasts",
        "android_get_device_info",
        "android_list_devices" -> Category.OBSERVE

        "android_wait_for_element",
        "android_wait_for_gone",
        "android_wait_for_idle",
        "android_scroll_to_element" -> Category.WAIT

        "android_input_text",
        "android_press_key",
        "android_global_action",
        "android_set_clipboard" -> Category.INPUT

        else -> Category.MANAGE
    }

    private fun percentile(sorted: List<Int>, quantile: Double): Int {
        val rank = ceil(quantile * sorted.size).toInt().coerceIn(1, sorted.size)
        return sorted[rank - 1]
    }
}
