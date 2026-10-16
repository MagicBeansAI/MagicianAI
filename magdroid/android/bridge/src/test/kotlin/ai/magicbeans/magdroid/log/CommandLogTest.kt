package ai.magicbeans.magdroid.log

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test

class CommandLogTest {
    @Before
    fun setUp() {
        CommandLog.clear()
    }

    @After
    fun tearDown() {
        CommandLog.clear()
    }

    @Test
    fun `history is bounded and recent reads are newest first`() {
        repeat(301) { index ->
            CommandLog.add(entry(timestamp = index.toLong(), latencyMs = index))
        }

        assertEquals(300, CommandLog.entries.value.size)
        assertEquals(1L, CommandLog.entries.value.first().timestamp)
        assertEquals(listOf(300L, 299L, 298L), CommandLog.getRecent(3).map { it.timestamp })
        assertEquals(emptyList<CommandLog.Entry>(), CommandLog.getRecent(0))
    }

    @Test
    fun `performance statistics use the retained latency window`() {
        listOf(10, 20, 30, 40).forEachIndexed { index, latency ->
            CommandLog.add(entry(timestamp = index.toLong(), latencyMs = latency))
        }

        assertEquals(CommandLog.Stats(count = 4, p50 = 20, p95 = 40, p99 = 40), CommandLog.getPerformanceStats())
        CommandLog.clear()
        assertEquals(CommandLog.Stats(count = 0, p50 = 0, p95 = 0, p99 = 0), CommandLog.getPerformanceStats())
    }

    @Test
    fun `tool categories preserve the App Pilot filters`() {
        assertEquals(CommandLog.Category.GESTURE, CommandLog.categoryFor("android_tap"))
        assertEquals(CommandLog.Category.OBSERVE, CommandLog.categoryFor("android_get_ui_tree"))
        assertEquals(CommandLog.Category.WAIT, CommandLog.categoryFor("android_wait_for_idle"))
        assertEquals(CommandLog.Category.INPUT, CommandLog.categoryFor("android_input_text"))
        assertEquals(CommandLog.Category.MANAGE, CommandLog.categoryFor("android_launch_app"))
    }

    private fun entry(timestamp: Long, latencyMs: Int) = CommandLog.Entry(
        timestamp = timestamp,
        command = "android_tap",
        latencyMs = latencyMs,
        success = true,
        category = CommandLog.Category.GESTURE,
    )
}
