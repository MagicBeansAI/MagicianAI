package ai.magicbeans.magdroid.observe

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The words the observation uses about itself. They are a disclosure, not
 * decoration: the notification and mini-bar are how someone in the room —
 * or the owner three apps away — knows what is leaving this device.
 */
class ObserveShareTest {

    @Test
    fun `starting wins over everything, including a share`() {
        assertEquals("Starting…", observeNotificationDetail(ObserveState.Starting, 5, true))
        assertEquals("Starting…", observeLiveLabel(ObserveState.Starting, true))
    }

    @Test
    fun `a plain session narrates its chunks`() {
        assertEquals(
            "Audio is being sent to Magician.",
            observeNotificationDetail(ObserveState.Listening, 0, false),
        )
        assertEquals("1 chunk sent.", observeNotificationDetail(ObserveState.Listening, 1, false))
        assertEquals("3 chunks sent.", observeNotificationDetail(ObserveState.Listening, 3, false))
    }

    @Test
    fun `a share changes what the notification admits to`() {
        assertEquals(
            "Audio and screen are being sent to Magician.",
            observeNotificationDetail(ObserveState.Listening, 0, true),
        )
        assertEquals(
            "1 chunk sent · sharing screen.",
            observeNotificationDetail(ObserveState.Listening, 1, true),
        )
        assertEquals(
            "2 chunks sent · sharing screen.",
            observeNotificationDetail(ObserveState.Listening, 2, true),
        )
    }

    @Test
    fun `the mini bar says when the screen is leaving too`() {
        assertEquals("Recording this room", observeLiveLabel(ObserveState.Listening, false))
        assertEquals("Recording · sharing screen", observeLiveLabel(ObserveState.Listening, true))
    }
}
