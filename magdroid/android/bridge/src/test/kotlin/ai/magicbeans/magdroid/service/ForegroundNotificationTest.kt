package ai.magicbeans.magdroid.service

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ForegroundNotificationTest {
    @Test
    fun `a visible channel can post the foreground notification`() {
        assertTrue(
            canPostForegroundServiceNotification(
                notificationsEnabled = true,
                channelBlocked = false,
            ),
        )
    }

    @Test
    fun `disabled notifications cannot post the foreground notification`() {
        assertFalse(
            canPostForegroundServiceNotification(
                notificationsEnabled = false,
                channelBlocked = false,
            ),
        )
    }

    @Test
    fun `a blocked channel cannot post the foreground notification`() {
        assertFalse(
            canPostForegroundServiceNotification(
                notificationsEnabled = true,
                channelBlocked = true,
            ),
        )
    }
}
