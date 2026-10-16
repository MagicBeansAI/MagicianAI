package ai.magicbeans.magdroid.voice

import ai.magicbeans.magdroid.chat.MediaPreferences
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Test

class VoicePersonaCatalogTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `decode TTS and realtime voice choices with separate saved selections`() {
        val catalog = json.decodeFromString(RealtimeVoiceCatalog.serializer(), """{
          "stages":{"tts":[{"option_id":"kokoro","stage":"tts","provider_id":"kokoro",
            "engine_id":"fluid_audio","model_id":"kokoro-82m","label":"Kokoro",
            "availability":"available","variant":"af_heart",
            "capabilities":{"voices":["af_heart","am_michael"],"formats":["wav"]}}]},
          "realtime_voice_profiles":[{"profile_id":"grok","label":"Grok","provider":"grok_voice",
            "model":"voice","topology":"backend_proxied","mode":"assistant","available":true,
            "voice":"eve","voices":[{"id":"eve","label":"Eve"},{"id":"ara","label":"Ara"}]}]
        }""")
        assertEquals(listOf("af_heart", "am_michael"), catalog.optionsFor(NativeAudioStage.Tts).single().capabilities.voices)
        assertEquals("af_heart", catalog.optionsFor(NativeAudioStage.Tts).single().variant)
        assertEquals(listOf("eve", "ara"), catalog.profiles.single().voices.map { it.id })
        val preferences = json.decodeFromString(MediaPreferences.serializer(), """{
            "tts_voices":{"kokoro":"am_michael"},"realtime_voices":{"grok":"ara"}
        }""")
        assertEquals("am_michael", preferences.ttsVoices["kokoro"])
        assertEquals("ara", preferences.realtimeVoices["grok"])
        assertEquals(emptyMap<String, String>(), json.decodeFromString(MediaPreferences.serializer(), "{}").ttsVoices)
    }
    @Test
    fun `pipeline voice follows explicit model or profile fallback order`() {
        fun option(id: String, available: Boolean = true) = AudioStageOption(
            id = "$id:model", stage = "tts", providerId = id, engineId = "online", modelId = "model",
            label = id, availability = if (available) "available" else "unavailable",
        )
        val grok = option("grok")
        val sarvam = option("sarvam-v4")
        val macos = option("macos", false)
        val profile = AudioSurfaceProfile(surface = "hands_free", tts = AudioStageProfile(true, listOf("macos", "sarvam-v4", "grok")))
        val ordered = profile.orderedOptions(NativeAudioStage.Tts, listOf(grok, sarvam, macos))
        assertEquals(listOf("macos", "sarvam-v4", "grok"), ordered.map { it.providerId })
        assertEquals("sarvam-v4", effectiveAudioStageOption(ordered, null)?.providerId)
        assertEquals("grok", effectiveAudioStageOption(ordered, grok.id)?.providerId)
        assertEquals("sarvam-v4", effectiveAudioStageOption(ordered, "stale")?.providerId)
        assertEquals(null, effectiveAudioStageOption(emptyList(), null))
    }

}
