package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Voice preferences carried by the account, against
 * `media_rails/preferences.rs` and `put_media_preferences_handler`.
 *
 * These settings belong to the person, not the handset. Muting the assistant
 * on one device and being talked at by another is the failure this exists to
 * prevent.
 */
class MediaPreferencesTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `the account's spoken-reply choice is read`() {
        val prefs = json.decodeFromString(
            MediaPreferences.serializer(),
            """{"schema_version": 1, "auto_speak": true, "voice_mode": "realtime",
                "require_voice_prefix": true}""",
        )
        assertTrue(prefs.autoSpeak)
    }

    /**
     * The record carries settings this client does not model. They must decode
     * without complaint and, crucially, never be echoed back.
     */
    @Test
    fun `unmodelled settings decode and are not carried`() {
        val prefs = json.decodeFromString(
            MediaPreferences.serializer(),
            """{"auto_speak": false,
                "surface_profiles": {"dictation": "fast-v1"},
                "surface_stage_options": {"dictation": {"recording_stt": "whisper"}}}""",
        )
        assertFalse(prefs.autoSpeak)

        // A write carries only what this client owns. Round-tripping the
        // profile maps blind is how one device silently overwrites a choice
        // made on another.
        val body = chatRequestJson.encodeToString(
            MediaPreferencesUpdate.serializer(),
            MediaPreferencesUpdate(autoSpeak = false),
        )
        assertFalse("surface_profiles must not be echoed", body.contains("surface_profiles"))
        assertFalse("stage options must not be echoed", body.contains("surface_stage_options"))
        assertFalse("voice_mode must not be echoed", body.contains("voice_mode"))
    }

    /** Absent means default, not a parse failure — older records lack fields. */
    @Test
    fun `an empty record is readable`() {
        assertFalse(json.decodeFromString(MediaPreferences.serializer(), "{}").autoSpeak)
    }

    /**
     * Scope is the bearer, not the body. The write names only the field this
     * client owns so a missing `auto_speak` cannot be read as "leave alone".
     */
    @Test
    fun `a write names its value`() {
        val body = chatRequestJson.encodeToString(
            MediaPreferencesUpdate.serializer(),
            MediaPreferencesUpdate(autoSpeak = true),
        )
        val obj = json.parseToJsonElement(body).jsonObject
        assertEquals(true, obj["auto_speak"]?.jsonPrimitive?.content?.toBoolean())
        assertFalse("principal must not ride the body", body.contains("principal"))
        assertFalse("workspace must not ride the body", body.contains("workspace"))
    }

    /**
     * `auto_speak` must be present even when false. Encoding defaults off
     * would drop it, and the server reads a missing field as "leave alone" —
     * so muting would appear to succeed and change nothing.
     */
    @Test
    fun `muting is written, not omitted`() {
        val body = chatRequestJson.encodeToString(
            MediaPreferencesUpdate.serializer(),
            MediaPreferencesUpdate(autoSpeak = false),
        )
        assertTrue("auto_speak missing from the write", body.contains("\"auto_speak\":false"))
    }
}
