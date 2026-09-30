package ai.magicbeans.magdroid.voice

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The leash on an armed microphone, against `AmbientArm.swift`.
 *
 * The rule exists because somebody arms a microphone and then stops thinking
 * about it. Before this, listening ran until a person noticed and stopped it —
 * which on a phone in a pocket is indistinguishable from forever.
 */
class AmbientArmTest {

    private val minute = 60_000L
    private val start = 1_000_000L

    @Test
    fun `arming sets a window from now`() {
        val arm = AmbientArm.armedAt(start)
        assertEquals(start, arm.armedAtMs)
        assertEquals(start + 30 * minute, arm.expiresAtMs)
        assertFalse(arm.hasExpired(start))
        assertFalse(arm.hasExpired(start + 29 * minute))
        assertTrue(arm.hasExpired(start + 30 * minute))
    }

    @Test
    fun `each extension adds a legible thirty minutes`() {
        val armed = AmbientArm.armedAt(start)
        val once = armed.extended()!!
        assertEquals(start + 60 * minute, once.expiresAtMs)
        // The origin does not move, which is what the ceiling is measured from.
        assertEquals(start, once.armedAtMs)

        val twice = once.extended()!!
        assertEquals(start + 90 * minute, twice.expiresAtMs)
    }

    /**
     * The last extension may add less than a full increment, and the one after
     * it adds nothing at all — reported as null so a caller can say so rather
     * than appear to have added time.
     */
    @Test
    fun `the final extension is clipped to the ceiling, then refused`() {
        // Seven hours and fifty minutes in: ten minutes of headroom left.
        var arm = AmbientArm(start, start + (7 * 60 + 50) * minute)
        val clipped = arm.extended()
        assertNotNull(clipped)
        assertEquals("clipped to eight hours", start + 8 * 60 * minute, clipped!!.expiresAtMs)
        assertTrue(clipped.atCeiling)
        assertNull("nothing left to add", clipped.extended())
    }

    @Test
    fun `a window cannot be armed past the ceiling in the first place`() {
        val greedy = AmbientArm.armedAt(start, windowMs = 24 * 60 * minute)
        assertEquals(start + 8 * 60 * minute, greedy.expiresAtMs)
        assertTrue(greedy.atCeiling)
    }

    /** A lapsed window reads as zero rather than as negative time. */
    @Test
    fun `remaining time is floored at zero`() {
        val arm = AmbientArm.armedAt(start)
        assertEquals(30 * minute, arm.remainingMs(start))
        assertEquals(minute, arm.remainingMs(start + 29 * minute))
        assertEquals(0L, arm.remainingMs(start + 30 * minute))
        assertEquals(0L, arm.remainingMs(start + 99 * minute))
    }

    /**
     * What the notification says. Minutes, because the owner is deciding
     * whether to extend and "24 minutes left" answers that where a clock time
     * makes them do the arithmetic.
     */
    @Test
    fun `the countdown reads in whole minutes`() {
        assertEquals("30 min left", formatRemaining(30 * minute))
        // Rounded up: with fifty seconds to go, "0 min left" is wrong twice.
        assertEquals("1 min left", formatRemaining(50_000))
        assertEquals("59 min left", formatRemaining(59 * minute))
        assertEquals("1h left", formatRemaining(60 * minute))
        assertEquals("2h 30m left", formatRemaining(150 * minute))
        assertEquals("expiring", formatRemaining(0))
        assertEquals("expiring", formatRemaining(-1))
    }
}

/**
 * The window is the owner's setting, not a number chosen in the service.
 *
 * A second leash was added beside the existing one and hardcoded at thirty
 * minutes, so choosing "2 hours" or "Until I stop" was overridden by a window
 * nobody had asked for — the setting was still on screen and silently did
 * nothing. These pin the arithmetic each choice must produce.
 */
class AmbientLeashWindowTest {

    private val start = 1_000_000L
    private val minute = 60_000L

    @org.junit.Test
    fun `each leash choice arms its own window`() {
        listOf(
            AmbientLeash.ThirtyMinutes to 30L,
            AmbientLeash.TwoHours to 120L,
            AmbientLeash.UntilStopped to 8 * 60L,
        ).forEach { (leash, expectedMinutes) ->
            org.junit.Assert.assertEquals(
                "${leash.label} should arm ${expectedMinutes}m",
                expectedMinutes,
                leash.minutes,
            )
            val arm = AmbientArm.armedAt(start, leash.minutes * minute)
            org.junit.Assert.assertEquals(
                start + expectedMinutes * minute,
                arm.expiresAtMs,
            )
        }
    }

    /**
     * "Until I stop" is still bounded at eight hours, which is what its own
     * description promises — the ceiling and the choice agree.
     */
    @org.junit.Test
    fun `until-I-stop arms the full ceiling and cannot be extended past it`() {
        val arm = AmbientArm.armedAt(start, AmbientLeash.UntilStopped.minutes * minute)
        org.junit.Assert.assertEquals(start + 8 * 60 * minute, arm.expiresAtMs)
        org.junit.Assert.assertTrue(arm.atCeiling)
    }

    /** A shorter choice still has room to extend, which is the point of Extend. */
    @org.junit.Test
    fun `a thirty minute window can still be extended`() {
        val arm = AmbientArm.armedAt(start, AmbientLeash.ThirtyMinutes.minutes * minute)
        org.junit.Assert.assertFalse(arm.atCeiling)
        org.junit.Assert.assertEquals(start + 60 * minute, arm.extended()!!.expiresAtMs)
    }
}
