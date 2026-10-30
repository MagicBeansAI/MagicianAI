package ai.magicbeans.magdroid.access

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The `X-Magician-App-Version` value sent beside the device id. */
class AppVersionHeaderTest {
    @Test fun `version name and code read as name then code in parentheses`() {
        assertEquals("0.6.13 (40)", MagicianAccess.formatAppVersion("0.6.13", 40))
        assertEquals("0.6.13 (40)", MagicianAccess.formatAppVersion("  0.6.13 ", 40))
    }

    @Test fun `a blank version name sends no header`() {
        assertNull(MagicianAccess.formatAppVersion(null, 40))
        assertNull(MagicianAccess.formatAppVersion("", 40))
        assertNull(MagicianAccess.formatAppVersion("   ", 40))
    }

    @Test fun `the value is printable ASCII and at most 40 characters`() {
        val value = requireNotNull(MagicianAccess.formatAppVersion("0.6.13-é\u0007" + "x".repeat(60), 40))
        assertTrue(value.length <= 40)
        assertTrue(value.all { it in ' '..'~' })
    }

    @Test fun `the header name is the one the runtime reads`() {
        assertEquals("X-Magician-App-Version", MagicianAccess.HEADER_APP_VERSION)
    }
}
