package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.TutorShape
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * When each shape draws.
 *
 * The schedule is what turns a diagram into a lesson — shapes arriving in
 * teaching order rather than all at once — so it is worth pinning separately
 * from the drawing.
 */
class TutorScheduleTest {

    private fun shape(delay: Double? = null, duration: Double? = null, order: Int? = null) =
        TutorShape(type = "rect", delayMs = delay, durationMs = duration, revealOrder = order)

    /** With nothing declared, the storyboard still animates in order. */
    @Test
    fun `an untimed storyboard divides evenly`() {
        val windows = scheduleOf(List(4) { shape() })
        assertEquals(4, windows.size)
        assertEquals(0f, windows.first().start)
        assertEquals(1f, windows.last().end, 0.001f)
        // Each takes a quarter, so nothing appears before its turn.
        assertEquals(0.25f, windows[0].end, 0.001f)
        assertEquals(0.75f, windows[3].start, 0.001f)
    }

    /** Declared timings are laid end to end, then normalised to the whole. */
    @Test
    fun `declared timings are honoured in proportion`() {
        val windows = scheduleOf(
            listOf(
                shape(delay = 0.0, duration = 1000.0),
                shape(delay = 0.0, duration = 3000.0),
            ),
        )
        // The first quarter, then the rest — the ratio the storyboard asked for.
        assertEquals(0.25f, windows[0].end, 0.001f)
        assertEquals(0.25f, windows[1].start, 0.001f)
        assertEquals(1f, windows[1].end, 0.001f)
    }

    /** A delay is dead time before the shape, not part of its drawing. */
    @Test
    fun `a delay pushes a shape later without stretching it`() {
        val windows = scheduleOf(listOf(shape(delay = 1000.0, duration = 1000.0)))
        assertTrue("starts after the delay", windows[0].start > 0.4f)
        assertEquals(1f, windows[0].end, 0.001f)
    }

    /** An empty storyboard schedules nothing rather than dividing by zero. */
    @Test
    fun `an empty storyboard is empty`() {
        assertTrue(scheduleOf(emptyList()).isEmpty())
    }

    /**
     * A shape whose turn has passed stays finished. Restarting it would make a
     * long storyboard flicker as later shapes advance.
     */
    @Test
    fun `a window clamps at both ends`() {
        val window = RevealWindow(0.25f, 0.5f)
        assertEquals(0f, window.progressAt(0f))
        assertEquals(0f, window.progressAt(0.25f))
        assertEquals(0.5f, window.progressAt(0.375f), 0.001f)
        assertEquals(1f, window.progressAt(0.5f))
        assertEquals(1f, window.progressAt(1f))
    }

    /** A zero-width window is finished rather than dividing by zero. */
    @Test
    fun `an instant window is already done`() {
        assertEquals(1f, RevealWindow(0.5f, 0.5f).progressAt(0.6f))
    }

    /**
     * A storyboard that declares too little time is floored. Twelve shapes in
     * two seconds is unreadable, and one declaring nothing would finish
     * instantly.
     */
    @Test
    fun `total time has a floor`() {
        assertTrue(totalMillis(List(12) { shape() }) >= 12 * 400)
        assertEquals(0, totalMillis(emptyList()))
    }

    /** Declared time above the floor is used as declared. */
    @Test
    fun `a generous storyboard keeps its own pace`() {
        assertEquals(10_000, totalMillis(listOf(shape(duration = 10_000.0))))
    }
}
