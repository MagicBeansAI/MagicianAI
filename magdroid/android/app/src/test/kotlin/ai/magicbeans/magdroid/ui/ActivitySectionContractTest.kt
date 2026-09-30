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
        assertTrue(ACTIVITY_SECTION_HORIZONTAL_PADDING_DP < CHAT_BUBBLE_CONTENT_PADDING_DP)
        assertTrue(ACTIVITY_SECTION_VERTICAL_PADDING_DP <= 4)
        assertTrue(ACTIVITY_SECTION_EXPANDED_BOTTOM_PADDING_DP <= 6)
        assertEquals(0, MESSAGE_SPEAK_LEADING_PADDING_DP)
    }

    @Test
    fun `response owns the tint while steps remains unfilled`() {
        assertEquals(Control, chatBubbleColor(fromUser = false))
        assertEquals(Coral, chatBubbleColor(fromUser = true))
        assertEquals(chatBubbleColor(fromUser = true), voiceOriginBadgeColor())
        assertEquals(Color.Transparent, ACTIVITY_SECTION_BACKGROUND)
    }

    @Test
    fun `message typography stays compact and speech action mirrors ios`() {
        assertEquals(13, CHAT_BUBBLE_FONT_SP)
        assertEquals(18, CHAT_BUBBLE_LINE_HEIGHT_SP)
        assertEquals(11, CHAT_BUBBLE_CONTENT_PADDING_DP)
        assertTrue(MESSAGE_SPEAK_FONT_SP <= 10)
        assertTrue(MESSAGE_SPEAK_VERTICAL_PADDING_DP <= 3)
        assertEquals("Speak", messageSpeechActionLabel(active = false))
        assertEquals("Stop", messageSpeechActionLabel(active = true))
    }

    @Test
    fun `message silhouettes stay tail free and structured tones follow ios semantics`() {
        assertTrue(CHAT_BUBBLE_CORNER_DP <= 12)
        assertEquals(
            RoundedCornerShape(CHAT_BUBBLE_CORNER_DP.dp),
            chatBubbleShape(),
        )
        assertEquals(ChatSuccess, structuredToneColor("success"))
        assertEquals(MWarn, structuredToneColor("warning"))
        assertEquals(Danger, structuredToneColor("danger"))
        assertEquals(Coral, structuredToneColor("info"))
        assertEquals(Secondary, structuredToneColor(null))
    }
}
