package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatHistoryChromeContractTest {
    @Test
    fun `history panel stays narrower than the old drawer`() {
        assertEquals(300, HISTORY_PANEL_WIDTH_DP)
        assertTrue(HISTORY_PANEL_WIDTH_DP < 320)
    }

    @Test
    fun `history rows retain the ios card scale`() {
        assertEquals(12, HISTORY_ROW_CORNER_DP)
        assertEquals(10, HISTORY_ROW_PADDING_DP)
        assertEquals(14, HISTORY_ROW_TITLE_SP)
        assertTrue(HISTORY_ROW_META_SP < HISTORY_ROW_TITLE_SP)
        assertTrue(HISTORY_ROW_SUMMARY_SP <= HISTORY_ROW_META_SP)
        assertTrue(HISTORY_RESULT_BADGE_SP < HISTORY_ROW_META_SP)
    }
}
