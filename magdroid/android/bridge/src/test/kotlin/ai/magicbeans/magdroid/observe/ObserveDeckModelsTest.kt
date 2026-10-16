package ai.magicbeans.magdroid.observe

import ai.magicbeans.magdroid.voice.RealtimeVoiceCatalog
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The Command Deck's wire shapes, against the web deck's readers. */
class ObserveDeckModelsTest {

    @Test
    fun `recent meetings sort newest first, cap at thirty, skip bad rows`() {
        val rows = (1..40).joinToString(",") {
            """{"thread_id":"meeting-$it","session_id":"s$it","title":"T$it","agent_id":"presto","updated_at":${1_700_000_000_000L + it}}"""
        }
        val parsed = parseRecentMeetings("""{"active":[],"recent":[$rows,{"thread_id":""},"junk"]}""")
        assertEquals(30, parsed.size)
        assertEquals("meeting-40", parsed.first().threadId)
        assertEquals("T40", parsed.first().displayTitle)
    }

    @Test
    fun `recent meeting with no title shows its thread and seconds are normalised`() {
        val parsed = parseRecentMeetings(
            """{"recent":[{"thread_id":"meeting-a","updated_at":1700000000},{"thread_id":"meeting-b","title":null,"updated_at":1700000001000}]}""",
        )
        assertEquals(listOf("meeting-b", "meeting-a"), parsed.map { it.threadId })
        assertEquals("meeting-a", parsed[1].displayTitle)
        assertEquals(1_700_000_000_000L, parsed[1].updatedAtMillis)
        assertTrue(parseRecentMeetings("not json").isEmpty())
        assertTrue(parseRecentMeetings("""{"active":[]}""").isEmpty())
    }

    @Test
    fun `sources on counts channels once plus calendar, tabs and subscriptions`() {
        val channels = listOf(
            ChannelAssistChannel(provider = "gmail", enabled = true),
            ChannelAssistChannel(provider = "whatsapp", enabled = true),
        )
        assertEquals(1 + 1 + 1 + 4, enabledSourcesCount(channels, true, true, 4))
        assertEquals(0, enabledSourcesCount(listOf(ChannelAssistChannel(enabled = false)), false, false, 0))
        assertEquals(0, enabledSourcesCount(null, null, null, null))
        assertEquals(2, enabledSourcesCount(null, true, null, 1))
    }

    @Test
    fun `channel and subscription payloads decode leniently`() {
        val channels = deckJson.decodeFromString(
            ChannelAssistChannelsResponse.serializer(),
            """{"channels":[{"provider":"gmail","provider_display":"Gmail","account_alias":"work","display":"me@x.com",
                "lane":"user_assist","connected":true,"enabled":true,"thread_count":12,"message_count":40,
                "purposes":["verification_codes"],"new_field":1}],"history_lookback_days":7}""",
        ).channels.single()
        assertEquals("Gmail", channels.providerLabel)
        assertEquals("me@x.com", channels.accountLabel)
        assertTrue(channels.hasVerificationCodes)

        val page = deckJson.decodeFromString(
            ObservationSubscriptionPage.serializer(),
            """{"items":[{"subscription_id":"a","display_name":"HN","enabled":true,"consecutive_failures":2},
                {"subscription_id":"b","display_name":"RSS","enabled":false},
                {"subscription_id":"c","display_name":"Blog","enabled":true}],"total":9}""",
        )
        assertEquals(listOf("Retrying", "Paused", "Listening"), page.items.map { it.stateLabel })
        assertEquals(9, page.total)
    }

    @Test
    fun `calendar, ambient and catch-up summaries`() {
        val cal = deckJson.decodeFromString(
            CalendarObserveStatus.serializer(),
            """{"enabled":true,"accounts":["me@x.com"],"frequency":"daily","time":"07:00","total_synced":3,"last_sync_at":null}""",
        )
        assertEquals("Daily at 07:00", cal.scheduleLabel)
        assertEquals("Hourly", CalendarObserveStatus(frequency = "hourly", time = "07:00").scheduleLabel)

        assertEquals(
            "On · 12 signals today",
            ambientSummary(AmbientStatus(enabled = true, paired = true, acceptedToday = 12)),
        )
        assertEquals("Off · no browser paired · 0 signals", ambientSummary(AmbientStatus()))

        val catchUp = deckJson.decodeFromString(
            CatchUpEnvelope.serializer(),
            """{"status":{"phase":"active","admitted_items":40,"processed_items":12,"remaining_items":28},"options":{}}""",
        ).status
        assertEquals("Catching up · 12 of 40 items processed", catchUpSummary(catchUp))
        assertEquals("Caught up", catchUpSummary(CatchUpStatus(phase = "completed")))
    }

    @Test
    fun `surface profiles decode trimmed and drop blanks`() {
        assertEquals(
            mapOf("meeting" to "meeting-fluid-local-v1"),
            parseSurfaceProfiles("""{"auto_speak":true,"surface_profiles":{"meeting":" meeting-fluid-local-v1 ","listening":""}}"""),
        )
        assertEquals(emptyMap<String, String>(), parseSurfaceProfiles("""{"auto_speak":false}"""))
        assertNull(parseSurfaceProfiles("<html>"))
    }

    @Test
    fun `profile patch carries only the surface and clears its stage overrides`() {
        val body = deckJson.parseToJsonElement(surfaceProfilePatch("listening", "listening-vad-gated-v1")).jsonObject
        assertEquals(setOf("surface_profiles", "surface_stage_options"), body.keys)
        assertEquals("listening-vad-gated-v1", body["surface_profiles"]!!.jsonObject["listening"]!!.jsonPrimitive.content)
        val stages = body["surface_stage_options"]!!.jsonObject["listening"]!!.jsonObject
        assertEquals(setOf("vad", "recording_stt", "streaming_stt", "diarization", "tts"), stages.keys)
        assertTrue(stages.values.all { it.jsonPrimitive.content.isEmpty() })
        assertFalse(surfaceProfilePatch("meeting", null).contains("auto_speak"))
        // Default is an empty id, which the server treats as "clear".
        assertEquals(
            "",
            deckJson.parseToJsonElement(surfaceProfilePatch("meeting", null)).jsonObject["surface_profiles"]!!
                .jsonObject["meeting"]!!.jsonPrimitive.content,
        )
    }

    @Test
    fun `optimistic apply sets and clears`() {
        val start = mapOf("meeting" to "a")
        assertEquals(mapOf("meeting" to "a", "listening" to "b"), applySurfaceProfile(start, "listening", "b"))
        assertEquals(emptyMap<String, String>(), applySurfaceProfile(start, "meeting", null))
        assertEquals(emptyMap<String, String>(), applySurfaceProfile(start, "meeting", "default"))
    }

    @Test
    fun `profile choices are the surface's own, labelled and described`() {
        val catalog = deckJson.decodeFromString(
            RealtimeVoiceCatalog.serializer(),
            """{"surface_profiles":{
                 "meeting-vad-gated-v1":{"surface":"meeting","turn_boundary":"stt_eou",
                   "vad":{"enabled":true,"providers":[]},"streaming_stt":{"enabled":true,"providers":[]}},
                 "meeting-fluid-local-v1":{"surface":"meeting","diarization":{"enabled":true,"providers":[]}},
                 "listening-x-v1":{"surface":"listening"}},
               "default_surface_profiles":{"meeting":"meeting-vad-gated-v1"}}""",
        )
        val choices = audioProfileChoices(catalog, ObserveAudioSurface.Meeting)
        assertEquals(listOf("Meeting Fluid Local", "Meeting Vad Gated"), choices.map { it.label })
        assertEquals("Voice activity · Live transcription · turns: stt eou", choices[1].description)
        assertEquals("Speakers", choices[0].description)
        assertEquals(1, audioProfileChoices(catalog, ObserveAudioSurface.Listening).size)
    }
}
