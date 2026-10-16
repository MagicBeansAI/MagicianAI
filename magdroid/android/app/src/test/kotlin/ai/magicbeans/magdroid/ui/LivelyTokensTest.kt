package ai.magicbeans.magdroid.ui

import androidx.compose.ui.unit.dp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Shared lively tokens, mirrored from iOS `LivelyTokens.swift`. */
class LivelyTokensTest {
    @Test
    fun radii_match_ios() {
        assertEquals(6.dp, Lively.chipRadius)
        assertEquals(14.dp, Lively.cardRadius)
        assertEquals(10.dp, Lively.composerRadius)
        assertEquals(16.dp, Lively.bubbleRadius)
        assertEquals(48.dp, Lively.tapMin)
    }

    @Test
    fun status_tones_use_the_web_status_language() {
        val sorbet = Themes.palette("sorbet")
        assertEquals(sorbet.running, statusToneColor(StatusTone.Running, sorbet))
        assertEquals(androidx.compose.ui.graphics.Color(0xFF4A78E0), statusToneColor(StatusTone.Running, sorbet))
        assertEquals(sorbet.warning, statusToneColor(StatusTone.Waiting, sorbet))
        assertEquals(sorbet.warning, statusToneColor(StatusTone.Partial, sorbet))
        assertEquals(sorbet.success, statusToneColor(StatusTone.Done, sorbet))
        assertEquals(sorbet.danger, statusToneColor(StatusTone.Failed, sorbet))
        assertEquals(sorbet.secondaryAccentColor, statusToneColor(StatusTone.NotStarted, sorbet))
        assertEquals(sorbet.secondaryText, statusToneColor(StatusTone.Cancelled, sorbet))
        // Running is never the accent.
        Themes.families.map { Themes.palette(it.light) }.forEach { palette ->
            assertEquals(palette.running, statusToneColor(StatusTone.Running, palette))
        }
    }

    @Test
    fun only_live_tones_move_and_never_with_animations_off() {
        assertTrue(statusDotAnimates(StatusTone.Running, 1f))
        assertTrue(statusDotAnimates(StatusTone.Waiting, 0.5f))
        assertFalse(statusDotAnimates(StatusTone.Running, 0f))
        assertFalse(statusDotAnimates(StatusTone.Waiting, 0f))
        listOf(StatusTone.Done, StatusTone.Failed, StatusTone.Partial, StatusTone.NotStarted, StatusTone.Cancelled)
            .forEach { assertFalse(statusDotAnimates(it, 1f)) }
    }

    @Test
    fun step_lists_are_compact_with_a_subtle_hairline() {
        assertEquals(12f, Lively.STEP_TITLE_SP)
        assertEquals(10.5f, Lively.STEP_META_SP)
        assertEquals(6.dp, Lively.stepRowPadding)
        val sorbet = Themes.palette("sorbet")
        val hairline = livelyStepHairlineColor(sorbet)
        assertEquals(sorbet.controlBorder.alpha * 0.6f, hairline.alpha, 1f / 255f)
        assertTrue(hairline.alpha < sorbet.controlBorder.alpha)
    }
}
