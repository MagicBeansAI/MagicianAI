package ai.magicbeans.magdroid.log

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test

class CommandLogProjectionTest {
    @Before
    fun resetBefore() = CommandLog.clear()

    @After
    fun resetAfter() = CommandLog.clear()

    @Test
    fun `compose projection follows the authoritative bounded command log`() {
        val entry = CommandLog.Entry(
            timestamp = 42L,
            command = "android_tap",
            latencyMs = 7,
            success = true,
            category = CommandLog.Category.GESTURE,
        )

        CommandLog.add(entry)

        assertEquals(listOf(entry), CommandLog.entries.value)
        assertEquals(listOf(entry), CommandLog.getRecent(10))

        CommandLog.clear()
        assertEquals(emptyList<CommandLog.Entry>(), CommandLog.entries.value)
    }

    @Test
    fun `all transport recorders share the same tool categories`() {
        assertEquals(CommandLog.Category.GESTURE, CommandLog.categoryFor("android_tap"))
        assertEquals(CommandLog.Category.OBSERVE, CommandLog.categoryFor("android_get_ui_tree"))
        assertEquals(CommandLog.Category.WAIT, CommandLog.categoryFor("android_wait_for_idle"))
        assertEquals(CommandLog.Category.INPUT, CommandLog.categoryFor("android_press_key"))
        assertEquals(CommandLog.Category.MANAGE, CommandLog.categoryFor("android_launch_app"))
    }

    @Test
    fun `performance ignores idle stabilization but retains it in history`() {
        CommandLog.add(
            CommandLog.Entry(
                timestamp = 1L,
                command = "android_wait_for_idle",
                latencyMs = 12_000,
                success = true,
                category = CommandLog.Category.WAIT,
            ),
        )
        CommandLog.add(
            CommandLog.Entry(
                timestamp = 2L,
                command = "android_get_ui_tree",
                latencyMs = 18,
                success = true,
                category = CommandLog.Category.OBSERVE,
            ),
        )

        val stats = CommandLog.getPerformanceStats()

        assertEquals(2, CommandLog.size())
        assertEquals(2, CommandLog.entries.value.size)
        assertEquals(1, stats.count)
        assertEquals(18, stats.p50)
        assertEquals(18, stats.p95)
        assertEquals(18, stats.p99)
    }

    @Test
    fun `idle stabilization alone produces no performance sample`() {
        CommandLog.add(
            CommandLog.Entry(
                timestamp = 1L,
                command = "android_wait_for_idle",
                latencyMs = 5_000,
                success = true,
                category = CommandLog.Category.WAIT,
            ),
        )

        assertEquals(CommandLog.Stats(0, 0, 0, 0), CommandLog.getPerformanceStats())
        assertEquals(1, CommandLog.size())
    }
}
