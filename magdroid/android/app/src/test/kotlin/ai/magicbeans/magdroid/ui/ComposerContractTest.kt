package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ChatDoPermission
import ai.magicbeans.magdroid.voice.DictationState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import ai.magicbeans.magdroid.chat.ChatComposerMode
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.unit.dp

class ComposerContractTest {
    @Test
    fun `selected mode is an accent rounded square with readable on-accent text in every family`() {
        val previous = Themes.palettes.entries.first { it.value == activePalette }.key
        try {
            Themes.families.forEach { family ->
                listOf(AppearanceMode.Day, AppearanceMode.Night).forEach { mode ->
                    applyTheme(family.id, mode, systemIsDark = false)
                    ChatComposerMode.entries.forEach { composerMode ->
                        val fill = composerModeFill(composerMode).compositeOver(
                            composerModeTrackColor().compositeOver(Control),
                        )
                        val foreground = composerModeForeground(composerMode)
                        assertEquals(AccentFill, composerModeFill(composerMode))
                        val contrast = (maxOf(foreground.luminance(), fill.luminance()) + .05f) /
                            (minOf(foreground.luminance(), fill.luminance()) + .05f)
                        assertTrue("${family.name} $mode $composerMode contrast $contrast", contrast >= 3f)
                    }
                }
            }
        } finally {
            applyTheme(previous, if (Themes.palette(previous).isDark) AppearanceMode.Night else AppearanceMode.Day, false)
        }
    }

    @Test
    fun `dictation caption teaches tap and hold without losing active guidance`() {
        assertEquals("Tap or hold to talk", dictationHint(DictationState.Idle, holding = false))
        assertEquals(
            "Listening — tap to finish",
            dictationHint(DictationState.Recording, holding = false),
        )
        assertEquals(
            "Listening — release to send",
            dictationHint(DictationState.Recording, holding = true),
        )
        assertEquals("Working on it", dictationHint(DictationState.Transcribing, holding = false))
    }

    @Test
    fun `secondary composer controls stay compact on a phone`() {
        assertTrue(COMPOSER_MODE_FONT_SP <= 9)
        assertTrue(COMPOSER_MODE_VERTICAL_PADDING_DP <= 2)
        assertTrue(COMPOSER_MODE_HORIZONTAL_PADDING_DP <= 7)
        assertTrue(COMPOSER_TOOL_BUTTON_DP <= 30)
        assertTrue(COMPOSER_TOOL_ICON_DP <= 16)
        assertTrue(COMPOSER_PROFILE_VERTICAL_PADDING_DP <= 3)
        assertTrue(COMPOSER_PROFILE_TAG_CORNER_DP <= 4)
        assertEquals(8, COMPOSER_PROFILE_TAG_FONT_SP)
        assertEquals(1, COMPOSER_PROFILE_TAG_VERTICAL_PADDING_DP)
        assertEquals(4, COMPOSER_PROFILE_TAG_HORIZONTAL_PADDING_DP)
        assertEquals(32, COMPOSER_VOICE_DOCK_HEIGHT_DP)
        assertEquals(31, COMPOSER_VOICE_DOCK_PRIMARY_WIDTH_DP)
        assertEquals(22, COMPOSER_VOICE_DOCK_CHEVRON_WIDTH_DP)
    }

    @Test
    fun `composer and profile metadata follow the ios theme hierarchy`() {
        assertEquals(Control, composerSurfaceColor())
        assertEquals(Soft, composerModeTrackColor())
        assertEquals(OnAccent, composerModeForeground(ai.magicbeans.magdroid.chat.ChatComposerMode.Ask))
        assertEquals(OnAccent, composerModeForeground(ai.magicbeans.magdroid.chat.ChatComposerMode.Plan))
        assertEquals(OnAccent, composerModeForeground(ai.magicbeans.magdroid.chat.ChatComposerMode.AcceptInScope))
        assertEquals(AccentFill, composerModeFill(ai.magicbeans.magdroid.chat.ChatComposerMode.Ask))
        assertTrue(COMPOSER_MODE_SEGMENT_CORNER_DP < 6f)
        assertEquals(10, COMPOSER_CORNER_DP)
        assertEquals(Lively.composerRadius, COMPOSER_CORNER_DP.dp)
        assertTrue(COMPOSER_BOTTOM_GAP_DP <= 4)
        assertEquals(AccentFill, composerModeFill(ai.magicbeans.magdroid.chat.ChatComposerMode.AcceptInScope))
        assertEquals(ChatSuccess, chatProfileTierColor("instant"))
        assertEquals(Teal, chatProfileTierColor("normal"))
        assertEquals(ChatDiscovery, chatProfileTierColor("advanced"))
        assertEquals(Secondary, chatProfileTierColor("unknown"))
    }

    @Test
    fun `do split face names and explains its permission posture`() {
        assertEquals("DO · ASK", composerDoLabel(ChatDoPermission.Ask))
        assertEquals("DO · ACCEPT", composerDoLabel(ChatDoPermission.AcceptInScope))
        assertEquals(
            "Prompt before each file edit",
            composerDoGuidance(ChatDoPermission.Ask),
        )
        assertTrue(composerDoGuidance(ChatDoPermission.AcceptInScope).contains("outside the workspace"))
    }
}
