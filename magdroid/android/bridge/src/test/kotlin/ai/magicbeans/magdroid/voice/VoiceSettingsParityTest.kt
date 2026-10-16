package ai.magicbeans.magdroid.voice

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Contract coverage for the choices exposed by Android voice settings. */
class VoiceSettingsParityTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `all ambient modes shown in settings have implementations`() {
        assertEquals(setOf("dictation", "hands_free", "realtime"),
            AmbientVoiceMode.entries.filter { it.built }.map { it.wire }.toSet())
        assertTrue(AmbientVoiceMode.entries.all { it.detail.isNotBlank() })
        assertEquals(AmbientVoiceMode.HandsFree, AmbientVoiceMode.from("hands_free"))
    }

    @Test
    fun `translation always resolves to open mic`() {
        val translation = profile(id = "translate", mode = "translation")
        val conversation = profile(id = "talk", mode = "conversation")
        assertFalse(resolveLivePushToTalk(true, LiveVoiceEngine.Realtime, translation))
        assertTrue(resolveLivePushToTalk(true, LiveVoiceEngine.Realtime, conversation))
        assertTrue(resolveLivePushToTalk(true, LiveVoiceEngine.HandsFree, translation))
        assertFalse(resolveLivePushToTalk(false, LiveVoiceEngine.Realtime, conversation))
    }

    @Test
    fun `reconnect budgets match ios bootstrap and live recovery`() {
        assertEquals(listOf(500L, 1_500L, 3_000L, 5_000L),
            (0..3).map { RealtimeReconnectPolicy.delayMillis(false, it) })
        assertNull(RealtimeReconnectPolicy.delayMillis(false, 4))
        assertEquals(listOf(250L, 750L, 1_500L),
            (0..2).map { RealtimeReconnectPolicy.delayMillis(true, it) })
        assertNull(RealtimeReconnectPolicy.delayMillis(true, 3))
        assertTrue(RealtimeVoiceState(phase = RealtimeVoiceState.Phase.Reconnecting).active)
    }

    @Test
    fun `realtime client accepts its long lived websocket timeout`() {
        // Ktor 3 rejects zero as a timeout while constructing the client. This
        // test guards the application-start path because ChatViewModel owns a
        // realtime session even before the user starts a call.
        createRealtimeVoiceHttpClient().close()
    }

    @Test
    fun `provider catalog preserves defaults availability and audio pipelines`() {
        val catalog = json.decodeFromString(
            RealtimeVoiceCatalog.serializer(),
            """{
              "realtime_voice_profiles":[{
                "profile_id":"luna-live","label":"Luna Live","provider":"openai",
                "model":"gpt-live","topology":"backend_proxied","mode":"conversation",
                "turn_detection_mode":"none","available":true
              }],
              "realtime_voice_default_profile":"luna-live",
              "hands_free_voice":true,
              "surface_profiles":{"phone-fast":{"surface":"hands_free","turn_boundary":"server_vad",
                "streaming_stt":{"enabled":true,"providers":["whisper"]},
                "tts":{"enabled":true,"providers":["voice"]}}},
              "default_surface_profiles":{"hands_free":"phone-fast"},
              "stages":{"streaming_stt":[{"option_id":"whisper-fast","stage":"streaming_stt",
                "provider_id":"whisper","engine_id":"online","model_id":"small",
                "label":"Whisper Fast","availability":"available"}]}
            }""",
        )
        assertEquals("luna-live", catalog.defaultProfileId)
        assertTrue(catalog.profiles.single().native)
        assertTrue(catalog.profiles.single().nativeAndAvailable)
        assertEquals("none", catalog.profiles.single().turnDetectionMode)
        assertTrue(catalog.handsFreeAvailable)
        assertEquals("phone-fast", catalog.profilesFor(NativeAudioSurface.HandsFree).single().first)
        val option = catalog.optionsFor(NativeAudioStage.StreamingStt).single()
        assertTrue(catalog.audioProfiles.getValue("phone-fast")
            .supports(option, NativeAudioStage.StreamingStt))
        assertEquals("Online · Whisper Fast", option.displayLabel)
        assertFalse(catalog.audioProfiles.getValue("phone-fast")
            .usable(NativeAudioSurface.HandsFree, catalog.stages))
    }

    @Test
    fun `hands free requires available vad streaming stt and tts`() {
        val profile = AudioSurfaceProfile(
            surface = "hands_free",
            vad = AudioStageProfile(true, listOf("vad-provider")),
            streamingStt = AudioStageProfile(true, listOf("stt-provider")),
            tts = AudioStageProfile(true, listOf("tts-provider")),
        )
        fun option(stage: NativeAudioStage, provider: String, availability: String = "available") =
            AudioStageOption(provider, stage.wire, provider, "online", "model", provider, availability)
        val complete = mapOf(
            "vad" to listOf(option(NativeAudioStage.Vad, "vad-provider")),
            "streaming_stt" to listOf(option(NativeAudioStage.StreamingStt, "stt-provider")),
            "tts" to listOf(option(NativeAudioStage.Tts, "tts-provider")),
        )
        assertTrue(profile.usable(NativeAudioSurface.HandsFree, complete))
        assertFalse(profile.usable(
            NativeAudioSurface.HandsFree,
            complete + ("tts" to listOf(
                option(NativeAudioStage.Tts, "tts-provider", "unavailable"),
            )),
        ))
    }

    @Test
    fun `wav encoder writes a valid bounded pcm header`() {
        val wav = WavPcm.encode(byteArrayOf(1, 2, 3, 4), sampleRate = 16_000)
        assertEquals("RIFF", wav.copyOfRange(0, 4).toString(Charsets.US_ASCII))
        assertEquals("WAVE", wav.copyOfRange(8, 12).toString(Charsets.US_ASCII))
        assertEquals("data", wav.copyOfRange(36, 40).toString(Charsets.US_ASCII))
        assertEquals(48, wav.size)
        assertEquals(4, littleInt(wav, 40))
        assertEquals(listOf<Byte>(1, 2, 3, 4), wav.drop(WavPcm.HeaderBytes))
    }

    @Test(expected = IllegalArgumentException::class)
    fun `wav encoder rejects a partial pcm16 sample`() {
        WavPcm.encode(byteArrayOf(1))
    }

    @Test
    fun `audio note list decodes the ios shared snake case contract`() {
        val page = json.decodeFromString(
            AudioNotePage.serializer(),
            """{"items":[{"note_id":"note-1","provider":"whisper","used_fallback":true,
              "captured_at":"2026-08-10T10:00:00Z","source_surface":"chat_dictation",
              "transcript":"hello","duration_ms":1200,"mime_type":"audio/wav",
              "note_path":"notes/note-1.md","audio_path":"audio/note-1.wav","bytes":48}],
              "offset":0,"limit":20,"total":21,"has_more":true}""",
        )
        assertEquals("note-1", page.items.single().id)
        assertTrue(page.items.single().usedFallback)
        assertEquals(1_200L, page.items.single().durationMs)
        assertTrue(page.hasMore)
    }

    private fun profile(id: String, mode: String) = RealtimeVoiceProfile(
        id = id,
        label = id,
        provider = "test",
        model = "test",
        topology = "backend_proxied",
        mode = mode,
        available = true,
    )

    private fun littleInt(bytes: ByteArray, offset: Int): Int =
        (bytes[offset].toInt() and 0xff) or
            ((bytes[offset + 1].toInt() and 0xff) shl 8) or
            ((bytes[offset + 2].toInt() and 0xff) shl 16) or
            ((bytes[offset + 3].toInt() and 0xff) shl 24)
}
