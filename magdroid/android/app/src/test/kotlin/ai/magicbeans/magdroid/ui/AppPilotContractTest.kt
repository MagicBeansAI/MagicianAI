package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.bridge.BridgeLog
import ai.magicbeans.magdroid.log.CommandLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Regression contract for the single canonical App Pilot surface. */
class AppPilotContractTest {
    @Test
    fun `canonical surface folds setup into status and keeps activity distinct`() {
        assertEquals(listOf("Status", "Activity"), AppPilotTab.entries.map(AppPilotTab::label))
    }

    @Test
    fun `mcp status distinguishes mobile pairing from automation enrollment`() {
        assertEquals("Disabled", magicianMcpStatus(false, false, false, false))
        assertEquals("Not paired", magicianMcpStatus(true, false, false, false))
        assertEquals("Needs setup", magicianMcpStatus(true, true, false, false))
        assertEquals("Connecting", magicianMcpStatus(true, true, true, false))
        assertEquals(
            "Re-enroll",
            magicianMcpStatus(
                true,
                true,
                true,
                false,
                "bridge connection ended: The installed Magdroid APK no longer matches its Apps enrollment",
            ),
        )
        assertEquals("Retrying", magicianMcpStatus(true, true, true, false, "network unavailable"))
        assertEquals("Running", magicianMcpStatus(true, true, true, true))
    }

    @Test
    fun `mobile and App Pilot QR codes cannot cross pairing surfaces`() {
        assertEquals(null, pairingPurposeMismatch(PairingPurpose.MobileAccess, false))
        assertEquals(null, pairingPurposeMismatch(PairingPurpose.AppAutomation, true))
        assertEquals(
            "This is an App Pilot enrollment code. Scan it from App Pilot.",
            pairingPurposeMismatch(PairingPurpose.MobileAccess, true),
        )
        assertEquals(
            "This is a mobile connection code. Scan it from Settings → Connection.",
            pairingPurposeMismatch(PairingPurpose.AppAutomation, false),
        )
    }

    @Test
    fun `setup includes every operational grant without counting the pairing camera`() {
        assertEquals(
            setOf(
                AppPilotGrantKind.Accessibility,
                AppPilotGrantKind.NotificationAccess,
                AppPilotGrantKind.Notifications,
                AppPilotGrantKind.Battery,
                AppPilotGrantKind.ScreenCapture,
                AppPilotGrantKind.Microphone,
                AppPilotGrantKind.Assistant,
                AppPilotGrantKind.Overlay,
            ),
            APP_PILOT_GRANT_KINDS.toSet(),
        )
    }

    @Test
    fun `setup puts missing grants first without scrambling either group`() {
        val ordered = prioritizeMissing(
            listOf("accessibility" to true, "notifications" to false, "battery" to false, "overlay" to true),
        ) { it.second }

        assertEquals(
            listOf("notifications", "battery", "accessibility", "overlay"),
            ordered.map { it.first },
        )
    }

    @Test
    fun `activity keeps transport events global and tool events filterable`() {
        val lines = mergeActivity(
            bridge = listOf(BridgeLog.Line(BridgeLog.Level.Info, "bridge", "connected", 20L)),
            commands = listOf(
                CommandLog.Entry(10L, "android_tap", 7, true, CommandLog.Category.GESTURE),
            ),
        )

        assertNull(lines.first().category)
        assertEquals(CommandLog.Category.GESTURE, lines.last().category)
    }
}
