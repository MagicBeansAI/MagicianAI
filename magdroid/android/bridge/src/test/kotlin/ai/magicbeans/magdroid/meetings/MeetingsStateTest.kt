package ai.magicbeans.magdroid.meetings

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.Failures

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What the Observe screen shows while the two halves disagree.
 *
 * The whole point of keeping active and upcoming apart is that one failing must
 * not speak for the other. These are the assertions that hold that line.
 */
class MeetingsStateTest {

    private fun meeting(id: String, mode: String = "attendee") =
        ActiveMeeting(sessionId = id, mode = mode, status = "Running")

    /**
     * A calendar that cannot be read must not make a live meeting look broken.
     *
     * The calendar reaches a CLI that can be slow or unauthorised; the sessions
     * listing is local and usually fine. One error field each is what keeps
     * them from speaking for each other.
     */
    @Test
    fun `a calendar failure leaves the live meeting intact`() {
        val state = MeetingsUiState(
            active = listOf(meeting("s1")),
            upcomingFailure = Failure(FailureKind.Unknown, "gws profile not authorised", ""),
        )
        assertTrue(state.hasNow)
        assertEquals(1, state.visibleActive.size)
        assertNull(state.activeFailure)
    }

    @Test
    fun `a sessions failure leaves the calendar readable`() {
        val state = MeetingsUiState(
            upcoming = listOf(UpcomingMeeting(title = "Design review", meetUrl = "https://x")),
            activeFailure = Failures.offline(),
        )
        assertFalse(state.hasNow)
        assertEquals(1, state.upcoming.size)
        assertNull(state.upcomingFailure)
    }

    /** Ending a meeting removes it at once, like a dismissed card. */
    @Test
    fun `a meeting being ended leaves the list immediately`() {
        val state = MeetingsUiState(
            active = listOf(meeting("s1"), meeting("s2")),
            stopping = setOf("s1"),
        )
        assertEquals(listOf("s2"), state.visibleActive.map { it.sessionId })
    }

    /**
     * `hasNow` follows the raw list, not the filtered one.
     *
     * Ending the last meeting should not collapse the section out from under
     * the tap that ended it — the refresh that follows is what removes it.
     */
    @Test
    fun `the now section survives its last meeting being ended`() {
        val state = MeetingsUiState(active = listOf(meeting("s1")), stopping = setOf("s1"))
        assertTrue(state.hasNow)
        assertTrue(state.visibleActive.isEmpty())
    }

    @Test
    fun `nothing live means no now section`() {
        assertFalse(MeetingsUiState().hasNow)
    }

    /** Loading one half says nothing about the other. */
    @Test
    fun `the two halves load independently`() {
        val state = MeetingsUiState(loadingUpcoming = true, active = listOf(meeting("s1")))
        assertTrue(state.loadingUpcoming)
        assertFalse(state.loadingActive)
        assertTrue(state.hasNow)
    }
}
