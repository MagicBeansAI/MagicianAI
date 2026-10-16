package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class ConcurrentVoiceListSizingTest {
    private val peek = 18f
    private val bottoms = listOf(46f, 114f, 160f, 228f, 274f, 342f)

    private fun height(content: Float, rows: List<Float>, available: Float? = null, minTranscript: Float = 0f) =
        ConcurrentVoiceListSizing.height(content, rows, available, bottomPadding = 6f, peek = peek, minTranscript = minTranscript)

    @Test
    fun aShortListIsExactlyItsContent() {
        assertEquals(120f, height(120f, listOf(46f, 114f)), 0f)
        assertEquals(234f, height(234f, bottoms.take(4)), 0f)
    }

    @Test
    fun moreThanFourRowsShowFourMeasuredRowsAndAPeek() {
        assertEquals(228f + peek, height(348f, bottoms), 0f)
        assertEquals(228f + peek, height(348f, bottoms, available = 2000f), 0f)
    }

    @Test
    fun fourRowsAndAPeekShowWheneverTheTranscriptKeepsItsMinimum() {
        // TECNO KL8 (720×1600, 2x): ~947 px between the transcript top and the
        // list bottom; four two-line rows plus the peek need ~486 px. The old
        // flat 40% of the window gave 462 px — three and a half rows.
        val tecno = listOf(112f, 225f, 337f, 450f, 562f, 675f)
        assertEquals(450f + peek, height(700f, tecno, available = 947f, minTranscript = 240f), 0f)
        assertEquals(228f + peek, height(348f, bottoms, available = 486f, minTranscript = 240f), 0f)
    }

    @Test
    fun aRoomTooSmallForFourRowsGivesTheListFiftyFivePercentButNeverLessThanOneRow() {
        assertEquals(220f, height(348f, bottoms, available = 400f, minTranscript = 240f), 0f)
        assertEquals(52f, height(348f, bottoms, available = 50f, minTranscript = 240f), 0f)
    }

    @Test
    fun unmeasuredContentIsEmptyAndUnmeasuredRowsFallBackToContent() {
        assertEquals(0f, height(0f, emptyList(), available = 400f), 0f)
        assertEquals(90f, height(90f, emptyList()), 0f)
    }
}
