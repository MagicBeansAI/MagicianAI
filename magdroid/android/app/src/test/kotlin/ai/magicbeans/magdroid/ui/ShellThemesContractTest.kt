package ai.magicbeans.magdroid.ui

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Themes live in the side menu, not Settings (iOS `7d05d66d8`). */
class ShellThemesContractTest {
    private fun source(relativePath: String): String {
        val root = File(requireNotNull(System.getProperty("user.dir")))
        val project = generateSequence(root) { it.parentFile }
            .firstOrNull { File(it, "app/src/main/kotlin").isDirectory }
            ?: File(root, "magdroid/android").takeIf { File(it, "app/src/main/kotlin").isDirectory }
            ?: error("could not locate the Android Gradle project from $root")
        return File(project, relativePath).readText()
    }

    private val ui = "app/src/main/kotlin/ai/magicbeans/magdroid/ui"

    @Test
    fun bottom_bar_names_the_unified_destination_work() {
        assertEquals("Work", Destination.Tasks.label)
        assertFalse(Destination.entries.any { it.label == "Tasks" })
    }

    @Test
    fun side_menu_follows_the_ios_order_with_themes_last() {
        val shell = source("$ui/AppShell.kt")
        val labels = Regex("""DrawerAction\(Icons\.Outlined\.\w+, "([^"]+)"""")
            .findAll(shell).map { it.groupValues[1] }.toList()
        assertEquals(
            listOf("Thinking Map", "Notes", "VibeDev", "Audio Notes", "App Pilot", "Settings", "Themes"),
            labels,
        )
        assertTrue(shell.contains("ThemesSheet(onDismiss"))
    }

    @Test
    fun settings_no_longer_carries_the_theme_picker() {
        val settings = source("$ui/SettingsScreen.kt")
        assertFalse(settings.contains("SettingsSection(\"Appearance\")"))
        assertFalse(settings.contains("ThemePickerSheet"))
        assertFalse(settings.contains("ThemeStore"))
    }

    @Test
    fun themes_sheet_applies_on_tap_and_stays_open() {
        val themes = source("$ui/ThemesScreen.kt")
        assertTrue(themes.contains("Themes.families.forEachIndexed"))
        assertTrue(themes.contains("store.setFamily(family.id)"))
        assertTrue(themes.contains("AppearanceModePicker("))
        // Picking a family never dismisses; only Done and the scrim do.
        val pick = themes.substringAfter("store.setFamily(family.id)").substringBefore("}")
        assertFalse(pick.contains("onDismiss"))
    }

    @Test
    fun appearance_switch_is_a_compact_segmented_control() {
        assertTrue(APPEARANCE_SEGMENT_HEIGHT_DP <= 32)
    }
}
