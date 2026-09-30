package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatVoiceOriginTest {
    @Test
    fun `optimistic bubble preserves voice origin before the server echo arrives`() {
        assertTrue(optimisticUserMessage("voice", "hello", voiceOrigin = true).voiceOrigin)
        assertFalse(optimisticUserMessage("typed", "hello", voiceOrigin = false).voiceOrigin)
    }
}
