package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import androidx.compose.ui.text.font.FontListFontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.font.ResourceFont
import androidx.compose.ui.text.font.FontVariation

class ThemeChromeContractTest {
    /**
     * Every weight is its own static file. Several weights sharing one variable
     * resource (selected by a wght variation) let Android reuse whichever face
     * it cached first, and those files default to Thin — Bold went hairline.
     */
    @OptIn(androidx.compose.ui.text.ExperimentalTextApi::class)
    @Test
    fun longhand_fonts_give_each_weight_its_own_static_file() {
        val fonts = themeFontFamilies("longhand")
        listOf(fonts.brand, fonts.display, fonts.body, fonts.mono).forEach { family ->
            val faces = (family as FontListFontFamily).fonts
            val weights = listOf(FontWeight.Normal, FontWeight.Medium, FontWeight.SemiBold, FontWeight.Bold, FontWeight.ExtraBold)
            val resources = weights.map { weight ->
                val face = faces.single { it.weight == weight } as ResourceFont
                assertEquals(FontVariation.Settings(), face.variationSettings)
                face.resId
            }
            assertEquals(weights.size, resources.toSet().size)
        }
    }

    @Test
    fun longhand_day_and_night_material_containers_keep_the_native_palette() {
        listOf("longhand", "longhand-dark").forEach { id ->
            val palette = Themes.palette(id)
            val scheme = magicanColorScheme(palette)
            assertEquals(palette.surface, scheme.surfaceContainer)
            assertEquals(palette.elevated, scheme.surfaceContainerHigh)
            assertEquals(palette.background, scheme.surfaceContainerLowest)
            assertEquals(palette.secondaryText, scheme.onSurfaceVariant)
        }
    }

    @Test
    fun material_surfaces_follow_the_selected_magican_palette() {
        val palette = Themes.palette("arcane-terminal")
        val scheme = magicanColorScheme(palette)

        assertEquals(palette.background, scheme.background)
        assertEquals(palette.elevated, scheme.surface)
        assertEquals(palette.text, scheme.onSurface)
        assertEquals(palette.accent, scheme.primary)
    }

    @Test
    fun every_bottom_destination_has_distinct_selected_and_resting_icons() {
        Destination.entries.forEach { destination ->
            assertNotEquals(destination.icon(false), destination.icon(true))
        }
    }

    @Test
    fun bottom_navigation_is_centred_more_tightly_without_shrinking_touch_targets() {
        val compactSlot = bottomNavigationSlotWidthDp(containerWidthDp = 320)
        val edgeToEdgeSlot = 320f / Destination.entries.size

        assertTrue(compactSlot < edgeToEdgeSlot)
        assertTrue(compactSlot >= 48f)
    }

    @Test
    fun appearance_choices_keep_the_shared_ios_order() {
        assertEquals(
            listOf("System", "Day", "Night"),
            AppearanceMode.entries.map { it.label },
        )
    }

    @Test
    fun every_theme_variant_uses_the_web_font_roles() {
        assertEquals(Themes.palettes.keys, themeFontNamesById.keys)
        themeFontNamesById.forEach { (id, fonts) ->
            assertEquals(fonts.body, Themes.palette(id).font)
            assertEquals("Outfit", fonts.brand)
            themeFontFamilies(id)
        }

        assertEquals(
            ThemeFontNames("Outfit", "Outfit", "Manrope", "Geist Mono"),
            themeFontNames("longhand"),
        )
        assertEquals("Pixelify Sans", themeFontNames("mario-8bit").body)
        assertEquals("Bricolage Grotesque", themeFontNames("risograph").display)
        assertEquals("Permanent Marker", themeFontNames("mixtape").display)
        assertEquals("Lilita One", themeFontNames("cartoon").display)
    }
}
