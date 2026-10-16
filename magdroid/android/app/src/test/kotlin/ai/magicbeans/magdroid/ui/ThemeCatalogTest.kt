package ai.magicbeans.magdroid.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontListFontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.font.ResourceFont
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The pastel catalog, the Sorbet default and the retired-theme migration (iOS parity). */
class ThemeCatalogTest {
    @Test
    fun pastel_families_lead_the_picker_with_day_and_night_variants() {
        assertEquals(
            listOf("Sorbet", "Sky", "Candy"),
            Themes.families.take(3).map { it.name },
        )
        assertEquals(
            listOf("sorbet" to "sorbet-dark", "sky" to "sky-dark", "candy" to "candy-dark"),
            Themes.families.take(3).map { it.light to it.dark },
        )
        Themes.families.forEach { family ->
            assertTrue(family.light in Themes.palettes)
            assertTrue(family.dark in Themes.palettes)
        }
    }

    @Test
    fun bubbly_and_soft_machine_are_retired() {
        val names = Themes.families.map { it.name }
        assertFalse("Bubbly" in names)
        assertFalse("Soft Machine" in names)
        listOf("bubbly", "bubbly-dark", "soft-machine", "soft-machine-dark").forEach { id ->
            assertFalse(id in Themes.palettes)
            assertFalse(id in themeFontNamesById)
        }
    }

    @Test
    fun retired_ids_migrate_to_sorbet_keeping_day_or_night() {
        assertEquals("sorbet", Themes.migratedId("bubbly"))
        assertEquals("sorbet-dark", Themes.migratedId("bubbly-dark"))
        assertEquals("sorbet", Themes.migratedId("soft-machine"))
        assertEquals("sorbet-dark", Themes.migratedId("soft-machine-dark"))
        assertEquals("longhand", Themes.migratedId("longhand"))
        assertEquals("sorbet-dark", Themes.resolve("bubbly", AppearanceMode.Night, systemIsDark = false))
        assertEquals(Themes.palettes.getValue("sorbet"), Themes.palette("soft-machine"))
    }

    @Test
    fun sorbet_is_the_default_only_without_a_saved_choice() {
        assertEquals("sorbet", Themes.DEFAULT)
        assertEquals("sorbet", Themes.initialFamily(null))
        assertEquals("longhand", Themes.initialFamily("longhand"))
        assertEquals("jarvis-light", Themes.initialFamily("jarvis-light"))
        assertEquals("sorbet", Themes.initialFamily("bubbly"))
        assertEquals("sorbet", Themes.resolve(Themes.initialFamily(null), AppearanceMode.System, systemIsDark = false))
        assertEquals("sorbet-dark", Themes.resolve(Themes.initialFamily(null), AppearanceMode.System, systemIsDark = true))
    }

    @Test
    fun pastel_palettes_mirror_the_web_tokens() {
        val sorbet = Themes.palette("sorbet")
        assertEquals(Color(0xFFFFF9F4), sorbet.background)
        assertEquals(Color(0xFFFFF4EC), sorbet.surface)
        assertEquals(Color(0xFF2B2230), sorbet.text)
        assertEquals(Color(0xFFE8505F), sorbet.accent)
        assertEquals(Color(0xFFFFFFFF), sorbet.onAccent)
        assertEquals(Color(0xFFFBEEE6), sorbet.softBackground)
        assertEquals(Color(0xFF7A5CF0), sorbet.secondaryAccentColor)
        assertEquals(Color(0xFF4A78E0), sorbet.running)
        assertEquals(
            listOf(Color(0xFFFFE1E4), Color(0xFFECE5FF), Color(0xFFDCF5EA), Color(0xFFFFF9F4)),
            sorbet.washStops,
        )
        assertTrue(Themes.palette("sorbet-dark").isDark)
        assertEquals(Color(0xFFFFFFFF), Themes.palette("sorbet-dark").onAccent)
        assertEquals(Color(0xFFED0014), Themes.palette("sorbet-dark").accentFill)
        assertEquals(Color(0xFF5A68F0), Themes.palette("sky").accent)
        assertEquals(Color(0xFF8CC4FF), Themes.palette("sky-dark").running)
        assertEquals(Color(0xFFE0418F), Themes.palette("candy").accent)
        assertEquals(Color(0xFFFFFFFF), Themes.palette("candy-dark").onAccent)
    }

    @Test
    fun every_theme_has_a_running_colour_and_a_non_flat_wash() {
        Themes.palettes.forEach { (id, palette) ->
            assertNotNull("$id statusRunning", palette.statusRunning)
            assertTrue("$id wash", palette.washStops.size >= 2)
            assertTrue("$id wash flat", palette.washStops.toSet().size >= 2)
        }
        // Running is the theme's blue, not its accent.
        assertTrue(Themes.palette("sorbet").running != Themes.palette("sorbet").accent)
        // A theme without a web wash falls back to background → surface.
        val longhand = Themes.palette("longhand")
        assertEquals(listOf(longhand.background, longhand.surface), longhand.washStops)
    }

    @Test
    fun pastel_fonts_follow_the_web_roles() {
        assertEquals(ThemeFontNames("Outfit", "Fredoka", "Nunito", "Geist Mono"), themeFontNames("sorbet"))
        assertEquals(ThemeFontNames("Outfit", "Outfit", "Nunito", "Geist Mono"), themeFontNames("sky-dark"))
        assertEquals(ThemeFontNames("Outfit", "Baloo 2", "Nunito", "Geist Mono"), themeFontNames("candy"))
        assertEquals(themeFontNames("sorbet"), themeFontNames("bubbly"))
    }

    @Test
    fun nunito_and_baloo_give_each_weight_its_own_static_file() {
        listOf(themeFontFamilies("sorbet").body, themeFontFamilies("candy").display).forEach { family ->
            val faces = (family as FontListFontFamily).fonts
            val weights = listOf(FontWeight.Normal, FontWeight.Medium, FontWeight.SemiBold, FontWeight.Bold, FontWeight.ExtraBold)
            val resources = weights.map { weight -> (faces.single { it.weight == weight } as ResourceFont).resId }
            assertEquals(weights.size, resources.toSet().size)
        }
    }
}
