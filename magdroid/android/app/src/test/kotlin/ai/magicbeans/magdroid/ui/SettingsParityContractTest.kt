package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.bridge.BridgeLog
import ai.magicbeans.magdroid.log.CommandLog
import ai.magicbeans.magdroid.voice.VoiceMediaError
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.io.File
import ai.magicbeans.magdroid.identity.ProductIdentity
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
        assertTrue(connection.detail.contains("Connected devices → Connect Android"))
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
    fun `audio notes translates transport failures into actionable settings text`() {
        assertTrue(audioNotesUserMessage(ConnectException(), "failed").startsWith("Magician is offline"))
        assertTrue(audioNotesUserMessage(SocketTimeoutException(), "failed").contains("did not respond in time"))
        assertEquals(
            "No Magician host is configured.",
            audioNotesUserMessage(VoiceMediaError("No Magician host is configured.", false), "failed"),
        )
    }

    @Test
    fun `engine pickers name whose credentials each engine uses`() {
        assertEquals("Codex · your login", catalogEngineLabel("codex", "login"))
        assertEquals("Magician", catalogEngineLabel("magician", "login"))
        assertEquals("Claude Code · Magician’s key", catalogEngineLabel("claude", "magician_key"))
    }

    @Test
    fun `shared settings are not native pages any more`() {
        val pages = SettingsPage.entries.map { it.name }
        listOf("Preferences", "HarnessCredentials", "Budgets").forEach { removed ->
            assertFalse("$removed must live in the embedded web Settings", removed in pages)
        }
        val settings = source("app/src/main/kotlin/ai/magicbeans/magdroid/ui/SettingsScreen.kt")
        listOf(
            "BudgetsSettingsPage", "HarnessCredentialsSettingsPage", "PreferencesSettingsPage",
            "StorageMaintenance", "VoicePersonaControls", "\"Your preferences\"",
        ).forEach { gone -> assertFalse("Settings still shows $gone", settings.contains(gone)) }
        assertTrue(settings.contains("SettingsPage.WebSettings -> return EmbeddedWebSettingsPage("))
        assertTrue(settings.contains("onOpenPage(SettingsPage.WebSettings)"))
        assertEquals("${ProductIdentity.productName} settings", SettingsPage.WebSettings.title)
    }

    @Test
    fun `device settings are five headed sections in order`() {
        assertEquals(
            listOf("Voice & audio", "Notifications & permissions", "Keyboard & privacy", "Connection", "Help & about"),
            DeviceSettingsSection.entries.map { it.title },
        )
        assertEquals(
            listOf(
                DeviceSettingsRow.TalkToAssistant, DeviceSettingsRow.WakesOn, DeviceSettingsRow.PhoneVoice,
                DeviceSettingsRow.StayAvailable, DeviceSettingsRow.CaptureScreen,
                DeviceSettingsRow.ConversationMode, DeviceSettingsRow.SpeakReplies,
            ),
            visibleDeviceSettings("").first { it.first == DeviceSettingsSection.VoiceAndAudio }.second,
        )
        assertEquals(null, DeviceSettingsRow.WebSettings.section)
        assertEquals(DeviceSettingsSection.HelpAndAbout, DeviceSettingsRow.HowToUse.section)
        assertEquals(DeviceSettingsSection.KeyboardAndPrivacy, DeviceSettingsRow.ProtectedApps.section)
        val settings = source("app/src/main/kotlin/ai/magicbeans/magdroid/ui/SettingsScreen.kt")
        assertTrue(settings.contains("WorkSearchField(\"Search settings\""))
        // Section headings are this screen's own bold headings, not the small Work card titles.
        assertTrue(settings.contains("SettingsSectionHeading(title)"))
        assertFalse(settings.contains("WorkGroupTitle("))
        // Every indexed row is drawn by the Settings list.
        DeviceSettingsRow.entries.filter { it.section != null }.forEach { row ->
            assertTrue("$row has no row in Settings", settings.contains("DeviceSettingsRow.${row.name} ->"))
        }
    }

    @Test
    fun `settings has a pinned section chip bar outside the scrolling list`() {
        val settings = source("app/src/main/kotlin/ai/magicbeans/magdroid/ui/SettingsScreen.kt")
        assertTrue(settings.contains("testTag(\"settings-section-bar\")"))
        assertTrue(settings.contains("testTag(\"settings-section-chip-${'$'}{section.name}\")"))
        assertTrue(settings.contains(".semantics { selected = active }"))
        // The bar is drawn before, not inside, the scrolling column.
        val bar = settings.indexOf("SettingsSectionBar(sectionOrder")
        val scroller = settings.indexOf(".verticalScroll(contentScroll)")
        assertTrue(bar in 0 until scroller)
        assertTrue(settings.contains("contentScroll.animateScrollTo("))
        assertTrue(settings.contains("derivedStateOf"))
    }

    @Test
    fun `the active section is the last heading at or above the spy line`() {
        val offsets = listOf("voice" to 100, "alerts" to 600, "keyboard" to 1100, "help" to 1500)
        fun active(scroll: Int, max: Int = 2000) = activeSettingsSection(offsets, scroll, max, linePx = 50)
        // Nothing has reached the line yet: the first section.
        assertEquals("voice", active(0))
        assertEquals("voice", active(549))
        // A heading exactly on the line counts as scrolled past it.
        assertEquals("alerts", active(550))
        assertEquals("alerts", active(1049))
        assertEquals("keyboard", active(1050))
        assertEquals("help", active(1450))
        // At the end of a scrolling list, the last section, even before its heading reaches the line.
        assertEquals("help", active(1200, max = 1200))
        // A list that does not scroll never forces the last section.
        assertEquals("voice", activeSettingsSection(offsets, 0, 0, 50))
        assertEquals(null, activeSettingsSection(emptyList<Pair<String, Int>>(), 0, 0, 50))
    }

    @Test
    fun `search narrows the chip bar to sections with matches`() {
        assertEquals(DeviceSettingsSection.entries.toList(), visibleDeviceSettings("").map { it.first })
        assertEquals(
            listOf(DeviceSettingsSection.VoiceAndAudio, DeviceSettingsSection.NotificationsAndPermissions),
            visibleDeviceSettings("screen").map { it.first },
        )
        // Nothing matches: no chips, so the bar hides.
        assertTrue(visibleDeviceSettings("no-such-setting").isEmpty())
        val settings = source("app/src/main/kotlin/ai/magicbeans/magdroid/ui/SettingsScreen.kt")
        assertTrue(settings.contains("val sectionOrder = sections.map { it.first }"))
        assertTrue(settings.contains("if (sectionOrder.isNotEmpty()) {"))
    }

    @Test
    fun `search keeps rows whose title or keywords hold every word`() {
        fun visible(query: String) = visibleDeviceSettings(query).flatMap { it.second }
        assertEquals(listOf(DeviceSettingsRow.SpeakReplies), visible("speak replies"))
        assertEquals(listOf(DeviceSettingsRow.SpeakReplies), visible("  READ   aloud "))
        assertTrue(DeviceSettingsRow.WakesOn in visible("wake"))
        assertTrue(DeviceSettingsRow.HomeScreenWidget in visible("widget"))
        assertTrue(visible("wake widget").isEmpty())
        assertTrue(visible("no-such-setting").isEmpty())
        // A blank query keeps everything; unavailable rows stay hidden.
        assertEquals(DeviceSettingsRow.entries.count { it.section != null }, visible("").size)
        assertFalse(
            DeviceSettingsRow.RefreshRegistration in
                visibleDeviceSettings("") { it != DeviceSettingsRow.RefreshRegistration }.flatMap { it.second },
        )
        // A section with no match is left out.
        assertEquals(listOf(DeviceSettingsSection.KeyboardAndPrivacy), visibleDeviceSettings("protected").map { it.first })
    }

    @Test
    fun `a search for a shared setting finds the embedded web settings row`() {
        listOf("budgets", "spend", "harness", "credentials", "preferences", "storage", "voices", "trust",
            "rules", "workspaces", "models", "engines", "notes", "notifications", "devices").forEach { word ->
            assertTrue("$word should lead to web Settings", DeviceSettingsRow.WebSettings.matches(word))
        }
        assertTrue(visibleDeviceSettings("budgets").isEmpty())
        assertFalse(DeviceSettingsRow.WebSettings.matches("no-such-setting"))
        val settings = source("app/src/main/kotlin/ai/magicbeans/magdroid/ui/SettingsScreen.kt")
        assertTrue(settings.contains("\"No settings match\""))
    }

    @Test
    fun `android has one consolidated at a glance setup surface`() {
        val settings = source(
            "app/src/main/kotlin/ai/magicbeans/magdroid/ui/SettingsScreen.kt",
        )

        assertEquals(DeviceSettingsSection.NotificationsAndPermissions, DeviceSettingsRow.Notifications.section)
        assertEquals(DeviceSettingsSection.NotificationsAndPermissions, DeviceSettingsRow.HomeScreenWidget.section)
        assertEquals(DeviceSettingsSection.NotificationsAndPermissions, DeviceSettingsRow.RemoteDelivery.section)
        assertEquals(1, settings.split("DeviceSettingsRow.HomeScreenWidget ->").size - 1)
        assertTrue(settings.contains("Home Screen widget"))
        assertTrue(settings.contains("\"Notifications\""))
        assertTrue(settings.contains("\"Allowed\""))
        assertTrue(settings.contains("Allow notifications"))
        assertTrue(settings.contains("Open notification settings"))
        assertTrue(settings.contains("FirebaseApp.getApps(context).isNotEmpty()"))
        assertTrue(settings.contains("Packaged · host-dependent"))
        assertTrue(!settings.contains("else -> \"Available\""))
    }
}
