package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.meetings.ActiveMeeting
import ai.magicbeans.magdroid.meetings.UpcomingMeeting
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Regression contract for the Observe Command Deck (web `/observe` parity). */
class ObserveParityContractTest {
    @Test
    fun `idle now view is header, kpis, launchpad, upcoming, widgets, recent`() {
        assertEquals(
            listOf(
                ObserveDeckSection.CommandHeader,
                ObserveDeckSection.Kpis,
                ObserveDeckSection.Launchpad,
                ObserveDeckSection.Upcoming,
                ObserveDeckSection.Widgets,
                ObserveDeckSection.Recent,
            ),
            observeDeckSections(ObservePane.Now, hasLive = false),
        )
    }

    @Test
    fun `live block sits above every view`() {
        ObservePane.entries.forEach { pane ->
            val sections = observeDeckSections(pane, hasLive = true)
            assertEquals(ObserveDeckSection.CommandHeader, sections[0])
            assertEquals(ObserveDeckSection.Kpis, sections[1])
            assertEquals("pane $pane", ObserveDeckSection.Live, sections[2])
        }
        assertFalse(observeDeckSections(ObservePane.Sources, hasLive = false).contains(ObserveDeckSection.Live))
    }

    @Test
    fun `each view carries its own sections`() {
        assertEquals(
            listOf(ObserveDeckSection.ThisPhone, ObserveDeckSection.WebAccounts),
            observeDeckSections(ObservePane.Sources, false).drop(2),
        )
        assertEquals(listOf(ObserveDeckSection.AudioProfiles), observeDeckSections(ObservePane.Audio, false).drop(2))
        assertEquals(
            listOf(ObserveDeckSection.PublishedNotes, ObserveDeckSection.AudioNotes),
            observeDeckSections(ObservePane.Notes, false).drop(2),
        )
    }

    @Test
    fun `the local capture is not repeated as a server-side live card`() {
        val active = listOf(
            ActiveMeeting(sessionId = "local", mode = "passive"),
            ActiveMeeting(sessionId = "bot", mode = "attendee"),
        )

        assertEquals(listOf("bot"), otherActiveMeetings(active, "local").map { it.sessionId })
        assertEquals(listOf("local", "bot"), otherActiveMeetings(active, null).map { it.sessionId })
    }

    @Test
    fun `live exists only for an actual local or server capture`() {
        assertFalse(observeHasNow(localListening = false, activeMeetingCount = 0))
        assertTrue(observeHasNow(localListening = true, activeMeetingCount = 0))
        assertTrue(observeHasNow(localListening = false, activeMeetingCount = 1))
    }

    @Test
    fun `launchpad hides listen while this phone captures and toggles one form`() {
        assertEquals(LaunchTile.entries, launchpadTiles(localLive = false))
        assertEquals(
            listOf(LaunchTile.JoinAgent, LaunchTile.ShareScreen, LaunchTile.Brainstorm),
            launchpadTiles(localLive = true),
        )
        assertEquals(LaunchTile.Listen, toggleLaunchTile(null, LaunchTile.Listen))
        assertNull(toggleLaunchTile(LaunchTile.Listen, LaunchTile.Listen))
        assertEquals(LaunchTile.JoinAgent, toggleLaunchTile(LaunchTile.Listen, LaunchTile.JoinAgent))
    }

    @Test
    fun `upcoming rows match active sessions by url first, title only as a fallback`() {
        val event = UpcomingMeeting(title = "Standup", meetUrl = "https://meet.google.com/abc-defg-hij?authuser=0")
        val byUrl = ActiveMeeting(sessionId = "u", url = "meet.google.com/ABC-defg-hij/")
        val otherLinkSameTitle = ActiveMeeting(sessionId = "t", title = "standup", url = "https://meet.google.com/zzz")
        assertEquals("u", activeSessionForUpcoming(event, listOf(otherLinkSameTitle, byUrl))?.sessionId)
        assertNull(activeSessionForUpcoming(event, listOf(otherLinkSameTitle)))

        val titleOnly = ActiveMeeting(sessionId = "x", title = "  Standup ")
        assertEquals("x", activeSessionForUpcoming(event, listOf(titleOnly))?.sessionId)
        assertNull(activeSessionForUpcoming(UpcomingMeeting(title = ""), listOf(titleOnly)))
    }
}
