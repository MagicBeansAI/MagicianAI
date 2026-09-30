package ai.magicbeans.magdroid.access

import java.net.URLEncoder
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceEnrollmentLinksTest {
    private val id = "abcdefghijklmnopqrstuvwx"
    private val secret = "abcdefghijklmnopqrstuvwxyzABCDEFGH123456789"

    @Test fun `valid connection URI preserves the exact normalized origin and capability`() {
        val base = URLEncoder.encode("https://ios.example.com", Charsets.UTF_8.name())
        assertEquals(
            DeviceEnrollmentLink("https://ios.example.com", id, secret, "android"),
            DeviceEnrollmentLinks.parse("magican://connect?base=$base&id=$id&secret=$secret&kind=android"),
        )
    }

    @Test fun `local development origin may preserve an explicit port`() {
        val base = URLEncoder.encode("http://127.0.0.1:3002/", Charsets.UTF_8.name())
        assertEquals(
            "http://127.0.0.1:3002",
            DeviceEnrollmentLinks.parse("magican://connect?base=$base&id=$id&secret=$secret&kind=android")?.baseUrl,
        )
    }

    @Test fun `same Wi-Fi origin accepts private addresses without accepting public plaintext`() {
        val local = URLEncoder.encode("http://192.168.68.62:3002/", Charsets.UTF_8.name())
        val parsed = DeviceEnrollmentLinks.parse(
            "magican://connect?base=$local&id=$id&secret=$secret&kind=android",
        )
        assertEquals("http://192.168.68.62:3002", parsed?.baseUrl)
        assertTrue(DeviceEnrollmentLinks.isSameWifiOrigin(parsed!!.baseUrl))
        assertEquals(DeviceConnectionMode.SameWifi, parsed.connectionMode)
        assertNull(
            DeviceEnrollmentLinks.parse(
                "magican://connect?base=http%3A%2F%2F8.8.8.8%3A3002&id=$id&secret=$secret&kind=android",
            ),
        )
    }

    @Test fun `remote origin is classified for the phone route selector`() {
        val base = URLEncoder.encode("https://connect.magican.ai", Charsets.UTF_8.name())
        val parsed = DeviceEnrollmentLinks.parse(
            "magican://connect?base=$base&id=$id&secret=$secret&kind=android",
        )
        assertEquals(DeviceConnectionMode.Remote, parsed?.connectionMode)
    }

    @Test fun `Apps response metadata cannot retarget the scanned transport origin`() {
        assertTrue(
            enrollmentResponseOriginMatches(
                "http://192.168.68.62:3002",
                "https://connect.magican.ai",
                appsAutomation = true,
            ),
        )
        assertTrue(
            !enrollmentResponseOriginMatches(
                "http://192.168.68.62:3002",
                "https://connect.magican.ai",
                appsAutomation = false,
            ),
        )
    }

    @Test fun `legacy Android pair links remain consumable but iOS grants are rejected`() {
        val base = URLEncoder.encode("https://mobile.example", Charsets.UTF_8.name())
        assertEquals(
            "android",
            DeviceEnrollmentLinks.parse("magican://pair?base=$base&id=$id&secret=$secret")?.clientKind,
        )
        assertNull(
            DeviceEnrollmentLinks.parse("magican://connect?base=$base&id=$id&secret=$secret&kind=ios"),
        )
    }

    @Test fun `Apps link requires one exact challenge and an explicit trust method`() {
        val base = URLEncoder.encode("https://mobile.example", Charsets.UTF_8.name())
        val challenge = URLEncoder.encode(
            java.util.Base64.getEncoder().encodeToString(ByteArray(32) { 7 }),
            Charsets.UTF_8.name(),
        )
        val parsed = DeviceEnrollmentLinks.parse(
            "magican://apps-connect?base=$base&id=$id&secret=$secret&challenge=$challenge&trust=play_integrity&project=123456",
        )
        assertTrue(parsed?.appsAutomation == true)
        assertEquals(32, parsed?.challenge?.size)
        assertEquals(AndroidAutomationTrustMode.PlayIntegrity, parsed?.automationTrustMode)
        val privateBuild = DeviceEnrollmentLinks.parse(
            "magican://apps-connect?base=$base&id=$id&secret=$secret&challenge=$challenge&trust=owner_pinned_private_build",
        )
        assertEquals(AndroidAutomationTrustMode.OwnerPinnedPrivateBuild, privateBuild?.automationTrustMode)
        assertNull(privateBuild?.playIntegrityCloudProjectNumber)
        assertNull(
            DeviceEnrollmentLinks.parse(
                "magican://apps-connect?base=$base&id=$id&secret=$secret&challenge=short&trust=play_integrity&project=123456",
            ),
        )
        assertNull(
            DeviceEnrollmentLinks.parse(
                "magican://apps-connect?base=$base&id=$id&secret=$secret&challenge=$challenge",
            ),
        )
        assertNull(
            DeviceEnrollmentLinks.parse(
                "magican://apps-connect?base=$base&id=$id&secret=$secret&challenge=$challenge&trust=owner_pinned_private_build&project=123456",
            ),
        )
    }

    @Test fun `Apps enrollment errors explain the selected trust method`() {
        assertTrue(
            enrollmentFailureMessage(
                403,
                "android_apps_attestation_rejected",
                true,
                "https://connect.magican.ai",
            ).contains("hardware-attestation policy"),
        )
        assertTrue(
            enrollmentFailureMessage(
                403,
                "android_play_integrity_rejected",
                true,
                "https://connect.magican.ai",
            ).contains("Private / self-hosted build"),
        )
        assertTrue(
            enrollmentFailureMessage(
                403,
                null,
                false,
                "https://connect.magican.ai",
            ).contains("secure access route"),
        )
    }

    @Test fun `retained private enrollment keeps the absent Play project absent`() {
        assertNull(
            retainedPlayIntegrityProjectNumber(
                AndroidAutomationTrustMode.OwnerPinnedPrivateBuild,
                0,
            ),
        )
        assertEquals(
            123456L,
            retainedPlayIntegrityProjectNumber(
                AndroidAutomationTrustMode.PlayIntegrity,
                123456,
            ),
        )
        assertNull(
            retainedPlayIntegrityProjectNumber(
                AndroidAutomationTrustMode.PlayIntegrity,
                0,
            ),
        )
    }

    @Test fun `malformed duplicated and redirecting links are rejected`() {
        val cases = listOf(
            null,
            "https://magican.ai/pair?base=https://magican.ai&id=$id&secret=$secret",
            "magican://pair?base=javascript%3Aalert(1)&id=$id&secret=$secret",
            "magican://pair?base=http%3A%2F%2Fmagican.ai&id=$id&secret=$secret",
            "magican://pair?base=https%3A%2F%2Fmagican.ai%2Fapi&id=$id&secret=$secret",
            "magican://pair?base=https%3A%2F%2Fuser%3Apass%40magican.ai&id=$id&secret=$secret",
            "magican://pair?base=https%3A%2F%2Fmagican.ai&id=$id&id=another-ticket-value&secret=$secret",
            "magican://pair?base=https%3A%2F%2Fmagican.ai&id=short&secret=$secret",
            "magican://pair?base=https%3A%2F%2Fmagican.ai&id=$id&secret=short",
            "magican://pair/path?base=https%3A%2F%2Fmagican.ai&id=$id&secret=$secret",
        )
        cases.forEach { assertNull("accepted $it", DeviceEnrollmentLinks.parse(it)) }
    }
}
