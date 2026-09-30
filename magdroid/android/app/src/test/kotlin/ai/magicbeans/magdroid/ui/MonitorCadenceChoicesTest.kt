package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which cadences the composer offers.
 *
 * The rule exists because of a silent no-op: a monitor PATCH omits `schedule`
 * when no cron is set, so choosing "On demand only" while editing leaves the
 * monitor running on the cadence it already had. The form said one thing and
 * the server did another, with nothing to tell the owner which had happened.
 *
 * Web and iOS withhold the option for exactly this reason; this client offered
 * it.
 */
class MonitorCadenceChoicesTest {

    private val scheduled = listOf(
        "hourly", "daily-9", "daily-18", "weekdays-9", "weekly-mon-9", "monthly-1-9", "custom",
    )

    @Test
    fun `creating a monitor offers on-demand`() {
        val choices = cadenceChoices(isEdit = false, current = "none")
        assertEquals("none", choices.first())
        assertEquals(listOf("none") + scheduled, choices)
    }

    /** On a new monitor the option is real: no schedule is sent, and none is wanted. */
    @Test
    fun `creating offers on-demand whatever is currently picked`() {
        assertTrue(cadenceChoices(isEdit = false, current = "daily-9").contains("none"))
    }

    @Test
    fun `editing a scheduled monitor withholds the option that would do nothing`() {
        val choices = cadenceChoices(isEdit = true, current = "daily-9")
        assertFalse("on-demand is a no-op when editing a scheduled monitor", choices.contains("none"))
        assertEquals(scheduled, choices)
    }

    /**
     * Except when it is already the selection. A monitor with no schedule — or
     * one on an interval this form cannot express — opens at "none", and
     * dropping the row would make it silently appear to be on a cron.
     */
    @Test
    fun `editing an unscheduled monitor keeps the option it is already on`() {
        val choices = cadenceChoices(isEdit = true, current = "none")
        assertTrue(choices.contains("none"))
        assertEquals("none", choices.first())
    }

    /** Every scheduled preset stays available in both modes. */
    @Test
    fun `the scheduled presets are never withheld`() {
        listOf(true, false).forEach { editing ->
            listOf("none", "daily-9", "custom").forEach { current ->
                assertTrue(
                    "isEdit=$editing current=$current",
                    cadenceChoices(editing, current).containsAll(scheduled),
                )
            }
        }
    }
}
