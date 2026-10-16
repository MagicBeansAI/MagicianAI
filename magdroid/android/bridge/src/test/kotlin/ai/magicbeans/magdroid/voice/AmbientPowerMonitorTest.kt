package ai.magicbeans.magdroid.voice

import ai.magicbeans.magdroid.voice.AmbientPowerMonitor.Readings
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The battery rails for an always-on microphone.
 *
 * Ported from iOS's `AmbientPowerMonitor`, and tested for the same reason it was
 * split out there: these are decisions with wrong answers that ship silently. A
 * rail written as the obvious comparison refuses every window, and the symptom
 * on a device is a feature that "sometimes doesn't arm" — close to unreportable.
 */
class AmbientPowerMonitorTest {

    @Test
    fun `a healthy phone is simply allowed`() {
        assertEquals(AmbientPowerAdmission.Allowed, AmbientPowerMonitor.admit(Readings.healthy))
    }

    /**
     * Battery Saver warns; it does not refuse.
     *
     * Refusing makes the feature unusable for anyone who lives in Battery Saver —
     * it is sticky and often on for days — and the cost is the owner's to accept.
     */
    @Test
    fun `battery saver opens the window and says so`() {
        val admission = AmbientPowerMonitor.admit(
            Readings.healthy.copy(batterySaver = true),
        )
        assertTrue(admission.permitted)
        assertEquals(AmbientPowerBlock.BatterySaver, admission.warning)
    }

    /** A window opened at 8% is a microphone that outlives the phone. */
    @Test
    fun `below the floor is refused`() {
        val admission = AmbientPowerMonitor.admit(
            Readings(batterySaver = false, batteryLevel = 0.08f, isCharging = false),
        )
        assertEquals(AmbientPowerBlock.BatteryLow, admission.refusal)
        assertTrue(!admission.permitted)
    }

    @Test
    fun `exactly at the floor is allowed`() {
        val admission = AmbientPowerMonitor.admit(
            Readings(batterySaver = false, batteryLevel = 0.20f, isCharging = false),
        )
        assertTrue(admission.permitted)
    }

    /**
     * Refusal wins over warning.
     *
     * A phone both in Battery Saver and below the floor is refused, because
     * refusing is the stronger answer and warning about a window that should not
     * open says the wrong thing.
     */
    @Test
    fun `a phone in both states is refused rather than warned`() {
        val admission = AmbientPowerMonitor.admit(
            Readings(batterySaver = true, batteryLevel = 0.05f, isCharging = false),
        )
        assertEquals(AmbientPowerBlock.BatteryLow, admission.refusal)
        assertNull(admission.warning)
    }

    /**
     * Charging is exempt from the floor, deliberately.
     *
     * Read literally, "below 20%" ends the window of a phone on a charger at 15%
     * and climbing — for a feature whose premise is a phone nobody is holding,
     * which is very often a phone that is plugged in. The rail exists to stop a
     * microphone draining a battery towards nothing; one that is filling is not
     * that.
     */
    @Test
    fun `a charging phone below the floor is still allowed`() {
        val admission = AmbientPowerMonitor.admit(
            Readings(batterySaver = false, batteryLevel = 0.05f, isCharging = true),
        )
        assertTrue(admission.permitted)
        assertNull(admission.refusal)
    }

    /**
     * An unreadable level must not refuse everything.
     *
     * Android reports no level before the first battery broadcast, and a rail
     * that trusts `-1 < 0.20` refuses every window immediately and permanently.
     * The safe direction for a refusal is not to fire.
     */
    @Test
    fun `an unknown battery level is allowed rather than refused`() {
        val admission = AmbientPowerMonitor.admit(
            Readings(batterySaver = false, batteryLevel = -1f, isCharging = false),
        )
        assertEquals(AmbientPowerAdmission.Allowed, admission)
        assertNull(AmbientPowerMonitor.mustClose(Readings(false, -1f, false)))
    }

    // ── Closing a live window ────────────────────────────────────────────────

    @Test
    fun `the floor closes a window that is already open`() {
        assertEquals(
            AmbientPowerBlock.BatteryLow,
            AmbientPowerMonitor.mustClose(Readings(false, 0.10f, false)),
        )
    }

    /**
     * Battery Saver never closes a live window.
     *
     * Interrupting somebody mid-sentence to report a setting is worse than the
     * battery it would save; it becomes a warning instead.
     */
    @Test
    fun `battery saver does not close a window somebody is speaking into`() {
        assertNull(AmbientPowerMonitor.mustClose(Readings(true, 1f, false)))
        assertEquals(
            AmbientPowerBlock.BatterySaver,
            AmbientPowerMonitor.admit(Readings(true, 1f, false)).warning,
        )
    }

    @Test
    fun `a charging phone keeps its window through the floor`() {
        assertNull(AmbientPowerMonitor.mustClose(Readings(false, 0.03f, true)))
    }

    /** Every block has something to show in both places it appears. */
    @Test
    fun `each block reads as a sentence and as a label`() {
        AmbientPowerBlock.entries.forEach {
            assertTrue(it.message.isNotBlank())
            assertTrue(it.shortLabel.isNotBlank())
            // The sentence says what stopped, not merely what is true.
            assertTrue(it.message.contains("Magican"))
        }
    }
}
