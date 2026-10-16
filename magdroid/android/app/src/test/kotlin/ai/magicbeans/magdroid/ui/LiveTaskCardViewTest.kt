package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ActivityRow
import ai.magicbeans.magdroid.chat.LiveTaskDotTone
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Handyman
import androidx.compose.material.icons.filled.PauseCircle
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.TripOrigin
import androidx.compose.ui.text.font.FontWeight
import org.junit.Assert.assertEquals
import org.junit.Test

/** The live task card's and Steps panel's view-side mappings (no Compose runtime). */
class LiveTaskCardViewTest {
    @Test
    fun `every dot tone maps to the status dot's tone of the same name`() {
        LiveTaskDotTone.entries.forEach { tone ->
            assertEquals(tone.name, liveTaskStatusTone(tone).name)
        }
    }

    @Test
    fun `the card line keeps bold markdown and drops the markers`() {
        val text = liveTaskInlineMarkdown("Picked **three** bikes and __two__ shops")
        assertEquals("Picked three bikes and two shops", text.text)
        assertEquals(2, text.spanStyles.size)
        assertEquals(FontWeight.Bold, text.spanStyles.first().item.fontWeight)
        assertEquals("three", text.text.substring(text.spanStyles.first().start, text.spanStyles.first().end))
    }

    @Test
    fun `file sizes read like the other surfaces`() {
        assertEquals("512 bytes", liveTaskFileSize(512))
        assertEquals("18 KB", liveTaskFileSize(18_432))
        assertEquals("1.2 MB", liveTaskFileSize(1_200_000))
    }

    @Test
    fun `steps marks are icons by status first, then by kind`() {
        assertEquals(Icons.Filled.Warning, activityRowIcon(ActivityRow(label = "x", status = "failed", kind = "tool")))
        assertEquals(Icons.Filled.PauseCircle, activityRowIcon(ActivityRow(label = "x", status = "waiting", kind = "pause")))
        assertEquals(Icons.Outlined.TripOrigin, activityRowIcon(ActivityRow(label = "x", status = "running", kind = "llm")))
        assertEquals(Icons.Filled.Psychology, activityRowIcon(ActivityRow(label = "x", status = "done", kind = "llm")))
        assertEquals(Icons.Filled.Handyman, activityRowIcon(ActivityRow(label = "x", status = "done", kind = "tool")))
        assertEquals(Icons.Filled.CheckCircle, activityRowIcon(ActivityRow(label = "x", status = "done", kind = "step")))
    }

    @Test
    fun `the steps header says waiting on you`() {
        assertEquals("Waiting on you", activityHeaderWaitingLabel())
    }

    @Test
    fun `the floating date fades after a second and a bit`() {
        assertEquals(1_200L, FLOATING_DATE_FADE_MS)
    }

    @Test
    fun `card chips are drawn 24 dp so the header row stays tight`() {
        assertEquals(24, CARD_CHIP_HEIGHT_DP)
    }
}
