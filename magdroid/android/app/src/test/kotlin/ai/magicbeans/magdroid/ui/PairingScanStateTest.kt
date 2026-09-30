package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.DeviceEnrollmentLink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PairingScanStateTest {
    @Test
    fun `successful scan trims and returns the enrollment uri`() {
        val result = resolvePairingScan("  magican://pair?base=x  ", missingCameraPermission = false)

        assertEquals("magican://pair?base=x", result.enrollmentUri)
        assertNull(result.issue)
    }

    @Test
    fun `missing camera permission is distinct from ordinary cancellation`() {
        assertEquals(
            PairingScanIssue.CameraPermissionDenied,
            resolvePairingScan(null, missingCameraPermission = true).issue,
        )
        assertEquals(
            PairingScanIssue.Cancelled,
            resolvePairingScan(null, missingCameraPermission = false).issue,
        )
    }

    @Test
    fun `camera marker cannot override a successful scanner payload`() {
        val result = resolvePairingScan("magican://pair?base=x", missingCameraPermission = true)

        assertEquals("magican://pair?base=x", result.enrollmentUri)
        assertNull(result.issue)
    }

    @Test
    fun `a different App Pilot QR resumes the retained exact exchange first`() {
        val retained = DeviceEnrollmentLink(
            "https://connect.magican.ai",
            "retained-enrollment",
            "r".repeat(32),
        )

        assertFalse(shouldResumeRetainedAppsEnrollment(retained.copy(secret = "s".repeat(32)), retained))
        assertTrue(
            shouldResumeRetainedAppsEnrollment(
                retained.copy(enrollmentId = "different-enrollment"),
                retained,
            ),
        )
    }
}
