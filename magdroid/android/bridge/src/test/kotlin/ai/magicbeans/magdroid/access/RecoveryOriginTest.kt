package ai.magicbeans.magdroid.access

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class RecoveryOriginTest {
    @Test
    fun recoveryAcceptsOnlyTheEnrolledOrigin() {
        assertEquals("https://owner.example", requireRecoveryOrigin(
            "https://OWNER.example/", "https://owner.example",
        ))
        assertEquals("http://127.0.0.1:13002", requireRecoveryOrigin(
            "http://127.0.0.1:13002", "http://127.0.0.1:13002/",
        ))
    }

    @Test
    fun changedHostsPortsSchemesAndUnconfiguredClientsRequireEnrollment() {
        val enrolled = "https://owner.example"
        listOf(
            "https://other.example", "https://owner.example.evil.test",
            "https://owner.example:8443", "http://owner.example",
            "https://owner.example@evil.test", "https://owner.example/path",
            "https://owner.example?redirect=evil", "https://owner.example#evil",
            "", "not a URL",
        ).forEach { candidate ->
            assertThrows(IllegalArgumentException::class.java) {
                requireRecoveryOrigin(enrolled, candidate)
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            requireRecoveryOrigin("", enrolled)
        }
        assertThrows(IllegalArgumentException::class.java) {
            requireRecoveryOrigin("http://127.0.0.1:13002", "http://127.0.0.1:3002")
        }
    }
}
