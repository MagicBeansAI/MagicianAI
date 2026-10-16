package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.time.LocalDateTime
import java.time.ZoneId
import java.util.Locale

/** One-for-one port of iOS `ChatDateDividersTests` (web `dayLabel.ts`). */
class ChatDateDividersTest {
    private val zone: ZoneId = ZoneId.of("Asia/Kolkata")
    private val locale: Locale = Locale.US

    /** Thu 1 Oct 2026, 12:00 IST. */
    private val now: Long get() = date(2026, 10, 1, 12)

    private fun date(y: Int, m: Int, d: Int, h: Int, min: Int = 0): Long =
        LocalDateTime.of(y, m, d, h, min).atZone(zone).toInstant().toEpochMilli()

    @Test fun first_dated_message_and_each_day_change_open_a_divider() {
        val stamps = listOf<Long?>(
            date(2026, 9, 29, 9), date(2026, 9, 29, 22),
            date(2026, 9, 30, 0, 5),
            date(2026, 10, 1, 8), date(2026, 10, 1, 11),
        )
        assertEquals(listOf(0, 2, 3), ChatDateDividers.insertionIndices(stamps, zone))
    }

    @Test fun undated_messages_never_open_a_day_nor_break_the_run() {
        val stamps = listOf(null, date(2026, 10, 1, 8), null, date(2026, 10, 1, 9), date(2026, 10, 2, 1), null)
        assertEquals(listOf(1, 4), ChatDateDividers.insertionIndices(stamps, zone))
        assertEquals(emptyList<Int>(), ChatDateDividers.insertionIndices(emptyList(), zone))
        assertEquals(emptyList<Int>(), ChatDateDividers.insertionIndices(listOf(null, null), zone))
    }

    @Test fun day_boundary_follows_the_local_calendar() {
        // 23:59 and 00:01 local are different days even 2 minutes apart.
        val stamps = listOf<Long?>(date(2026, 9, 30, 23, 59), date(2026, 10, 1, 0, 1))
        assertEquals(listOf(0, 1), ChatDateDividers.insertionIndices(stamps, zone))
    }

    @Test fun labels() {
        fun label(d: Long) = ChatDateDividers.label(d, nowMillis = now, zone = zone, locale = locale)
        assertEquals("Today", label(date(2026, 10, 1, 0, 1)))
        assertEquals("Yesterday", label(date(2026, 9, 30, 23, 59)))
        assertEquals("Tuesday", label(date(2026, 9, 29, 10)))
        assertEquals("Friday", label(date(2026, 9, 25, 10))) // 6 days back
        assertEquals("Sep 24, 2026", label(date(2026, 9, 24, 10))) // 7 days back
        assertEquals("Dec 31, 2025", label(date(2025, 12, 31, 10)))
    }

    @Test fun labels_map_every_divider_index() {
        val stamps = listOf(date(2026, 9, 30, 9), null, date(2026, 10, 1, 9))
        assertEquals(
            mapOf(0 to "Yesterday", 2 to "Today"),
            ChatDateDividers.labels(stamps, nowMillis = now, zone = zone, locale = locale),
        )
    }

    @Test fun epoch_accepts_milliseconds_and_seconds() {
        assertEquals(1_759_300_000_000L, ChatDateDividers.epochMillis(1_759_300_000_000.0))
        assertEquals(1_759_300_000_000L, ChatDateDividers.epochMillis(1_759_300_000.0))
        assertNull(ChatDateDividers.epochMillis(0.0))
        assertNull(ChatDateDividers.epochMillis(Double.NaN))
    }

    @Test fun floating_label_is_the_topmost_visible_dated_message() {
        val stamps = listOf(date(2026, 9, 30, 8), null, date(2026, 9, 30, 9), date(2026, 10, 1, 9))
        assertEquals("Yesterday", ChatDateDividers.floatingLabel(stamps, setOf(3, 1, 2), now, zone, locale))
        assertNull(ChatDateDividers.floatingLabel(stamps, setOf(1), now, zone, locale))
        assertNull(ChatDateDividers.floatingLabel(stamps, emptySet(), now, zone, locale))
    }

    @Test fun floating_label_never_doubles_a_visible_divider() {
        val stamps = listOf(date(2026, 9, 30, 8), null, date(2026, 9, 30, 9), date(2026, 10, 1, 9))
        // Yesterday's divider (index 0) is on screen: no pill.
        assertNull(ChatDateDividers.floatingLabel(stamps, setOf(0, 1, 2), now, zone, locale))
        // Only Today's divider is on screen; the top message is Yesterday's.
        assertEquals("Yesterday", ChatDateDividers.floatingLabel(stamps, setOf(2, 3), now, zone, locale))
    }
}
