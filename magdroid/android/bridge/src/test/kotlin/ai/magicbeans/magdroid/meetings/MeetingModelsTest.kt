package ai.magicbeans.magdroid.meetings

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The meetings contract, against `meetings_api.rs`.
 *
 * The shapes here are hand-built `json!` objects on the server rather than
 * derived from a struct, which means nothing on that side enforces the field
 * names. Decoding real-shaped JSON is the only check either end has.
 */
class MeetingModelsTest {

    private fun active(json: String) =
        meetingsJson.decodeFromString(ActiveMeetingsResponse.serializer(), json)

    private fun upcoming(json: String) =
        meetingsJson.decodeFromString(UpcomingMeetingsResponse.serializer(), json)

    /**
     * Passive and attendee are different things wearing one shape.
     *
     * Passive is this phone listening to a room; attendee is a bot that joined
     * a call. Ending one is not the same act as ending the other, so the mode
     * must survive decoding rather than being flattened to "a meeting".
     */
    @Test
    fun `the two modes stay distinguishable`() {
        val response = active(
            """{"meetings":[
                {"session_id":"s1","mode":"passive","status":"Running","title":"Standup",
                 "mic":true,"paused":false},
                {"session_id":"s2","mode":"attendee","status":"Running",
                 "url":"https://meet.example/abc","thread_id":"t9","paused":true}]}""",
        )
        val (room, bot) = response.all()
        assertFalse(room.isAttendee)
        assertTrue(bot.isAttendee)
        assertEquals("t9", bot.threadId)
        assertTrue(bot.paused)
    }

    /**
     * The server has used three names for the same list, and `active` is the
     * one it sends TODAY — verified against the live response, not the model.
     * This test used to pin only the two older names, which is how the decode
     * could silently answer empty for every real session and the "Now"
     * section never rendered a live meeting on any device.
     */
    @Test
    fun `every envelope name the server has used yields the list`() {
        assertEquals(
            1,
            active(
                """{"active":[{"session_id":"a","mode":"passive","status":"Listening",
                    "thread_id":"meeting-x","title":null,"url":null,"mic":true,
                    "paused":false,"latest_summary":null}]}""",
            ).all().size,
        )
        assertEquals(1, active("""{"meetings":[{"session_id":"a","mode":"passive"}]}""").all().size)
        assertEquals(1, active("""{"sessions":[{"session_id":"a","mode":"passive"}]}""").all().size)
        assertTrue(active("""{}""").all().isEmpty())
    }

    /** A row with no title still needs something readable. */
    @Test
    fun `the headline falls back through title, url, then mode`() {
        assertEquals(
            "Standup",
            ActiveMeeting(title = "Standup", url = "https://x").displayTitle,
        )
        assertEquals(
            "https://meet.example/abc",
            ActiveMeeting(mode = "attendee", url = "https://meet.example/abc").displayTitle,
        )
        assertEquals("Meeting", ActiveMeeting(mode = "attendee").displayTitle)
        // A passive session is this room, which is a truer label than "meeting".
        assertEquals("This room", ActiveMeeting(mode = "passive").displayTitle)
    }

    // ── Upcoming ─────────────────────────────────────────────────────────────

    @Test
    fun `an upcoming meeting decodes with its link and liveness`() {
        val response = upcoming(
            """{"meetings":[{"event_id":"e1","title":"Design review",
                             "start":"2026-08-11T10:00:00Z","end":"2026-08-11T11:00:00Z",
                             "meet_url":"https://meet.example/xyz","live_now":true,
                             "account":"work@example.com"}]}""",
        )
        val meeting = response.all().single()
        assertEquals("e1", meeting.id)
        assertTrue(meeting.isJoinable)
        assertTrue(meeting.liveNow)
        assertEquals("work@example.com", meeting.account)
    }

    @Test
    fun `the current events and per-account-errors envelope remains visible`() {
        val response = upcoming(
            """{"events":[{"event_id":"e2","title":"Investor call",
                 "meet_url":"https://meet.google.com/abc"}],
                 "errors":[{"account":"old@example.com","error":"token expired"}]}""",
        )

        assertEquals("Investor call", response.all().single().title)
        assertEquals(
            "Some calendars couldn't be read — check your Google sign-in.",
            response.errorSummary(),
        )
    }

    /** Only something with a link can be joined; the button follows this. */
    @Test
    fun `a meeting with no link is not joinable`() {
        assertFalse(UpcomingMeeting(title = "Lunch").isJoinable)
        assertFalse(UpcomingMeeting(title = "Lunch", meetUrl = "").isJoinable)
        assertTrue(UpcomingMeeting(title = "Call", meetUrl = "https://x").isJoinable)
    }

    /** Without an event id the row still needs a stable key for the list. */
    @Test
    fun `an id is derived when the calendar gives none`() {
        val a = UpcomingMeeting(title = "Sync", start = "2026-08-11T10:00:00Z")
        val b = UpcomingMeeting(title = "Sync", start = "2026-08-11T11:00:00Z")
        assertEquals("Sync|2026-08-11T10:00:00Z", a.id)
        // Two meetings with the same title at different times are two rows.
        assertFalse(a.id == b.id)
    }

    /**
     * A calendar that cannot be read is said out loud, with an empty list.
     *
     * The endpoint is separate precisely so this failure stays contained — it
     * must not read as "you have nothing on".
     */
    @Test
    fun `a calendar error decodes beside an empty list`() {
        val response = upcoming("""{"meetings":[],"error":"gws profile not authorised"}""")
        assertTrue(response.all().isEmpty())
        assertEquals("gws profile not authorised", response.error)
    }

    @Test
    fun `unknown fields do not break either decode`() {
        assertEquals(
            1,
            active("""{"meetings":[{"session_id":"a","mode":"passive","brand_new":1}],"x":2}""")
                .all().size,
        )
        assertEquals(
            1,
            upcoming("""{"meetings":[{"title":"t","something":true}]}""").all().size,
        )
    }
}
