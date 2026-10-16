package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.meetings.ActiveMeeting
import ai.magicbeans.magdroid.meetings.UpcomingMeeting
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ObserveDeckLogicTest {

    @Test
    fun `status line matches the web deck word for word`() {
        assertEquals("1 capture live", observeCaptureStatusLine(1, 3))
        assertEquals("2 captures live", observeCaptureStatusLine(2, 0))
        assertEquals("1 meeting live — nothing capturing", observeCaptureStatusLine(0, 1))
        assertEquals("3 meetings live — nothing capturing", observeCaptureStatusLine(0, 3))
        assertEquals("Quiet — nothing capturing", observeCaptureStatusLine(0, 0))
    }

    @Test
    fun `now kpi sub text`() {
        assertEquals("1 live capture", nowKpiSub(1, 0))
        assertEquals("2 live meetings", nowKpiSub(0, 2))
        assertEquals("Live captures & meetings", nowKpiSub(0, 0))
    }

    @Test
    fun `active captures count the local capture once`() {
        val server = listOf(ActiveMeeting(sessionId = "a"), ActiveMeeting(sessionId = "b"))
        assertEquals(2, observeActiveCaptureCount(server, localLive = false, localSessionId = null))
        // Local capture already in the server list: not double counted.
        assertEquals(2, observeActiveCaptureCount(server, localLive = true, localSessionId = "a"))
        // Local capture the server has not listed yet (it lags by a poll).
        assertEquals(3, observeActiveCaptureCount(server, localLive = true, localSessionId = "new"))
        assertEquals(1, observeActiveCaptureCount(emptyList(), localLive = true, localSessionId = null))
    }

    @Test
    fun `live calendar meetings count live_now rows`() {
        assertEquals(
            1,
            liveCalendarMeetings(listOf(UpcomingMeeting(title = "a", liveNow = true), UpcomingMeeting(title = "b"))),
        )
    }

    @Test
    fun `audio and notes kpi values`() {
        assertEquals("2", audioKpiValue(true))
        assertEquals("—", audioKpiValue(false))
        assertEquals("7", notesKpiValue(7, 40))
        assertEquals("40", notesKpiValue(null, 40))
        assertEquals("—", notesKpiValue(null, null))
    }

    @Test
    fun `kpi accessibility label`() {
        assertEquals(
            "Now and Live, 1, 1 live capture, selected",
            kpiAccessibilityLabel("Now & Live", "1", "1 live capture", selected = true),
        )
        assertEquals(
            "Sources on, 0, Channels, tabs and feeds",
            kpiAccessibilityLabel("Sources on", "0", "Channels, tabs & feeds", selected = false),
        )
    }

    @Test
    fun `panes parse from wire and deep links`() {
        assertEquals(ObservePane.Sources, ObservePane.fromWire("sources"))
        assertEquals(ObservePane.Audio, ObservePane.fromWire(" AUDIO "))
        assertNull(ObservePane.fromWire("pipelines"))
        assertNull(ObservePane.fromWire(null))
        assertEquals(ObservePane.Notes, ObservePane.fromDeepLink("magican://observe?pane=notes"))
        assertEquals(ObservePane.Now, ObservePane.fromDeepLink("magican://observe?x=1&pane=now"))
        assertNull(ObservePane.fromDeepLink("magican://observe"))
        assertNull(ObservePane.fromDeepLink("magican://observe?pane=bogus"))
    }

    @Test
    fun `observe deep link routes through the shortcut parser`() {
        assertEquals(AppShortcutTarget.OpenObserve(null), AppShortcutLinks.parse("magican://observe"))
        assertEquals(
            AppShortcutTarget.OpenObserve(ObservePane.Sources),
            AppShortcutLinks.parse("magican://observe?pane=sources"),
        )
        assertEquals(AppShortcutTarget.StartListening, AppShortcutLinks.parse("magican://listen"))
    }

    @Test
    fun `permission status separates never asked from blocked`() {
        assertEquals(PermissionStatus.Granted, permissionStatus(true, askedBefore = true, showRationale = false))
        assertEquals(PermissionStatus.NotAsked, permissionStatus(false, askedBefore = false, showRationale = false))
        assertEquals(PermissionStatus.Denied, permissionStatus(false, askedBefore = true, showRationale = true))
        assertEquals(PermissionStatus.Blocked, permissionStatus(false, askedBefore = true, showRationale = false))
    }

    @Test
    fun `relative time buckets`() {
        val now = 10_000_000_000L
        assertEquals("just now", relativeWhen(now - 10_000, now))
        assertEquals("5 min ago", relativeWhen(now - 5 * 60_000, now))
        assertEquals("3 h ago", relativeWhen(now - 3 * 3_600_000, now))
        assertEquals("", relativeWhen(0, now))
    }
}
