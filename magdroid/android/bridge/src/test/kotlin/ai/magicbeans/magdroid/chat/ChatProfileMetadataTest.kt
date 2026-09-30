package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

private val profileJson = Json { ignoreUnknownKeys = true }

class ChatProfileMetadataTest {
    @Test
    fun `profile metadata used by the chooser survives the wire`() {
        val profile = profileJson.decodeFromString<ChatProfile>(
            """{
                "name":"agentic-luna",
                "provider":"openai",
                "model":"gpt-6-luna",
                "is_default":true,
                "is_adaptive":true,
                "adaptive_description":"Chooses a tier for the turn",
                "adaptive_tier":"advanced"
            }""",
        )

        assertEquals("openai", profile.provider)
        assertEquals("gpt-6-luna", profile.compactDisplay())
        assertTrue(profile.isDefault)
        assertTrue(profile.isAdaptive)
        assertEquals("advanced", profile.adaptiveTier)
        assertEquals("Chooses a tier for the turn", profile.adaptiveDescription)
    }
}
