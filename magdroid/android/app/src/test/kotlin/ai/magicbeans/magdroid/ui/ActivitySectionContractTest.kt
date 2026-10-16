package ai.magicbeans.magdroid.ui

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Regression contract for the shared iOS/Android chat Steps disclosure. */
class ActivitySectionContractTest {
    @Test
    fun `settled empty turns do not draw an empty disclosure`() {
        assertFalse(shouldShowActivitySection(rowCount = 0, isLive = false))
        assertTrue(shouldShowActivitySection(rowCount = 1, isLive = false))
    }

    @Test
    fun `a live turn exposes steps zero while its first event is pending`() {
        assertTrue(shouldShowActivitySection(rowCount = 0, isLive = true))
    }

    @Test
    fun `toggle accessibility uses the same steps language as ios`() {
        assertEquals("Show steps", activitySectionToggleLabel(expanded = false))
        assertEquals("Hide steps", activitySectionToggleLabel(expanded = true))
    }

    @Test
    fun `steps use tighter padding and speak aligns to the response outer edge`() {
        assertTrue(ACTIVITY_SECTION_HORIZONTAL_PADDING_DP < CHAT_BUBBLE_ASSISTANT_PADDING_H_DP)
        assertTrue(ACTIVITY_SECTION_VERTICAL_PADDING_DP <= 4)
        assertTrue(ACTIVITY_SECTION_EXPANDED_BOTTOM_PADDING_DP <= 4)
        assertEquals(0, MESSAGE_SPEAK_LEADING_PADDING_DP)
    }

    @Test
    fun `steps sits on the assistant bubble's surface, as on iOS`() {
        assertEquals(Control, chatBubbleColor(fromUser = false))
        assertEquals(AccentFill, chatBubbleColor(fromUser = true))
        // The voice badge is a tint on the page, so it keeps the bright accent.
        assertEquals(Coral, voiceOriginBadgeColor())
        assertEquals(chatBubbleColor(fromUser = false), ACTIVITY_SECTION_BACKGROUND)
    }

    @Test
    fun `message typography stays compact and speech action mirrors ios`() {
        assertEquals(13, CHAT_BUBBLE_FONT_SP)
        assertEquals(18, CHAT_BUBBLE_LINE_HEIGHT_SP)
        // Assistant bubbles are compact and share the task card's 12 dp inset.
        assertEquals(12, CHAT_BUBBLE_ASSISTANT_PADDING_H_DP)
        assertEquals(9, CHAT_BUBBLE_ASSISTANT_PADDING_V_DP)
        assertTrue(CHAT_BUBBLE_USER_PADDING_H_DP > CHAT_BUBBLE_ASSISTANT_PADDING_H_DP)
        assertTrue(MESSAGE_SPEAK_FONT_SP <= 10)
        assertTrue(MESSAGE_SPEAK_VERTICAL_PADDING_DP <= 3)
        assertEquals("Speak", messageSpeechActionLabel(active = false))
        assertEquals("Stop", messageSpeechActionLabel(active = true))
    }

    @Test
    fun `message silhouettes stay tail free and structured tones follow ios semantics`() {
        // The lively bubble radius (16), a rounded rectangle with no tail.
        assertEquals(16, CHAT_BUBBLE_CORNER_DP)
        assertEquals(Lively.bubbleShape, chatBubbleShape())
        assertEquals(RoundedCornerShape(CHAT_BUBBLE_CORNER_DP.dp), chatBubbleShape())
        assertEquals(ChatSuccess, structuredToneColor("success"))
        assertEquals(MWarn, structuredToneColor("warning"))
        assertEquals(Danger, structuredToneColor("danger"))
        assertEquals(Coral, structuredToneColor("info"))
        assertEquals(Secondary, structuredToneColor(null))
    }

    @Test
    fun `the steps panel stays thin`() {
        // Header ~30 dp with 24 dp buttons; a small clickable's 48 dp touch
        // target comes from Compose's hit testing, not from layout.
        assertTrue(ACTIVITY_HEADER_HEIGHT_DP in 28..32)
        assertEquals(24, ACTIVITY_HEADER_CHIP_HEIGHT_DP)
        assertTrue(ACTIVITY_ROW_PADDING_DP <= 4)
        assertTrue(ACTIVITY_SECTION_TOP_GAP_DP <= 6)
        assertEquals(5, ACTIVITY_PREVIEW_LIMIT)
        assertEquals("Show all 9 steps", activityShowAllLabel(9))
    }

    @Test
    fun `the header drops the steps word and opens the run as inspect`() {
        assertEquals("Inspect", ACTIVITY_INSPECT_LABEL)
        assertEquals("Inspect run", ACTIVITY_INSPECT_DESCRIPTION)
        assertEquals("1 step", activityStepCountDescription(1))
        assertEquals("4 steps", activityStepCountDescription(4))
        assertTrue(ACTIVITY_HEADER_HEIGHT_DP - ACTIVITY_HEADER_CHIP_HEIGHT_DP <= 4)
    }

    @Test
    fun `step dots read their count and fold the rest`() {
        assertEquals("Latest 3 steps, 9 earlier", stepDotsDescription(3, 9))
        assertEquals("3 steps settled", stepDotsDescription(3, 0))
        assertEquals(stepTintColor(1, dark = false), stepTintColor(0, dark = false))
        assertEquals(stepTintColor(5, dark = true), stepTintColor(9, dark = true))
    }

    @Test
    fun `a live header has no spinner, a chevron ring and a breathing latest dot`() {
        val live = activityHeaderLiveIndicators(isLive = true, awaitsUser = false)
        assertFalse(live.spinner)
        assertTrue(live.chevronRing)
        assertTrue(live.breathingDot)
        assertFalse(live.waitingLabel)
        // Live, the count is gone: "+N" and the latest dots say the progress.
        assertFalse(live.stepCount)
        assertTrue(activityHeaderLiveIndicators(isLive = true, awaitsUser = true).waitingLabel)

        val settled = activityHeaderLiveIndicators(isLive = false, awaitsUser = true)
        assertFalse(settled.spinner)
        assertFalse(settled.chevronRing)
        assertFalse(settled.breathingDot)
        assertFalse(settled.waitingLabel)
        assertTrue(settled.stepCount)
    }

    @Test
    fun `an open header gets two dp above and below, the chevron a gap after it`() {
        assertEquals(2, activityHeaderVerticalPaddingDp(expanded = true))
        assertEquals(0, activityHeaderVerticalPaddingDp(expanded = false))
        assertTrue(ACTIVITY_CHEVRON_END_GAP_DP in 4..6)
    }

    @Test
    fun `the latest dot breathes only while live and with animations on`() {
        assertTrue(stepDotBreathes(breatheLatest = true, animatorDurationScale = 1f))
        assertFalse(stepDotBreathes(breatheLatest = true, animatorDurationScale = 0f))
        assertFalse(stepDotBreathes(breatheLatest = false, animatorDurationScale = 1f))
        assertTrue(STEP_DOT_BREATH_MS in 1200..1600)
        assertTrue(ACTIVITY_CHEVRON_RING_DP < ACTIVITY_HEADER_CHIP_HEIGHT_DP)
    }

    @Test
    fun `a step reads on one line, detail inline after its title`() {
        val line = activityRowLine("web_search", "lisbon museums\n open sunday")
        assertEquals("web_search  lisbon museums open sunday", line.text)
        assertEquals(1, line.spanStyles.size)
        assertEquals("web_search", activityRowLine("web_search", null).text)
    }
}
