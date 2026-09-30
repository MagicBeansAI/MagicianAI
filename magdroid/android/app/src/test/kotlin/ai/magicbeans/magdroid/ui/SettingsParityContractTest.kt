package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.bridge.BridgeLog
import ai.magicbeans.magdroid.log.CommandLog
import ai.magicbeans.magdroid.voice.VoiceMediaError
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The settings surface must not advertise inert or not-yet-shipped choices. */
class SettingsParityContractTest {
    private fun source(relativePath: String): String {
        val root = File(requireNotNull(System.getProperty("user.dir")))
        val project = generateSequence(root) { it.parentFile }
            .firstOrNull { File(it, "app/src/main/kotlin").isDirectory }
            ?: File(root, "magdroid/android").takeIf {
                File(it, "app/src/main/kotlin").isDirectory
            }
            ?: error("could not locate the Android Gradle project from $root")
        return File(project, relativePath).readText()
    }

    @Test
    fun `how to use explains runtime QR connection instead of a baked endpoint`() {
        val connection = androidUsageTips.first { it.group == "Connect" }
        assertTrue(connection.detail.contains("Devices → Connect Android"))
        assertTrue(connection.detail.contains("Settings → Connection"))
        assertTrue(connection.detail.contains("not built into the app"))
    }

    @Test
    fun `app pilot activity combines connection and tool events newest first`() {
        val activity = mergeActivity(
            bridge = listOf(
                BridgeLog.Line(
                    level = BridgeLog.Level.Warn,
                    tag = "bridge",
                    message = "connection ended",
                    atMs = 10L,
                ),
            ),
            commands = listOf(
                CommandLog.Entry(
                    timestamp = 20L,
                    command = "android_get_ui_tree",
                    latencyMs = 12,
                    success = true,
                    category = CommandLog.Category.OBSERVE,
                ),
            ),
        )

        assertEquals(2, activity.size)
        assertEquals("android_get_ui_tree · 12ms", activity.first().message)
        assertEquals(ActivityTone.Info, activity.first().tone)
        assertEquals("connection ended", activity.last().message)
        assertEquals(ActivityTone.Warn, activity.last().tone)
    }

    @Test
    fun `roadmap contains only the remaining Android work`() {
        assertEquals(
            listOf(
                "Screenshot + spoken request from Assistant / Quick Settings",
                "Inline visual confirmations in the Android assistant",
                "Agent-assisted negotiation from messaging apps",
                "Camera or sketch handoff to VibeDev",
            ),
            androidRoadmap.map { it.name },
        )
        assertTrue(androidRoadmap.none { it.phase == "Available" })

        // These are still pending on iOS, but Android already has the real
        // platform equivalents: mobile pairing in Settings, separate attested
        // enrollment in App Pilot, and Accessibility +
        // screenshot-backed App Copilot. They must not drift back into this
        // pending-only list merely to make the two platforms look identical.
        assertTrue(androidRoadmap.none { "QR" in it.name || "App Copilot" in it.name })
    }

    @Test
    fun `audio notes translates transport failures into actionable settings text`() {
        assertTrue(audioNotesUserMessage(ConnectException(), "failed").startsWith("Magician is offline"))
        assertTrue(audioNotesUserMessage(SocketTimeoutException(), "failed").contains("did not respond in time"))
        assertEquals(
            "No Magician host is configured.",
            audioNotesUserMessage(VoiceMediaError("No Magician host is configured.", false), "failed"),
        )
    }

    @Test
    fun `android has one consolidated at a glance setup surface`() {
        val settings = source(
            "app/src/main/kotlin/ai/magicbeans/magdroid/ui/SettingsScreen.kt",
        )

        assertEquals(1, settings.split("SettingsSection(\"At a glance\")").size - 1)
        assertTrue(settings.contains("Home Screen widget"))
        assertTrue(settings.contains("Alerts and task cards"))
        assertTrue(settings.contains("FirebaseApp.getApps(context).isNotEmpty()"))
        assertTrue(settings.contains("Packaged · host-dependent"))
        assertTrue(!settings.contains("else -> \"Available\""))
    }
}
