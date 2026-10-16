package ai.magicbeans.magdroid.voice

import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `interaction.status` is the only signal that the assistant is still on the
 * request after an utterance ended (Gemini 3.8 Live Extended Thinking's "let
 * me check…" gap). The reducer must mirror the web client: `in_progress` is
 * working, `idle` is not, and a spelling the client does not know can never
 * leave "Working…" on screen.
 */
class RealtimeVoiceWorkingStateTest {
    @Test
    fun `in_progress marks the assistant as working and idle clears it`() {
        val idle = RealtimeVoiceState(phase = RealtimeVoiceState.Phase.Ready)
        assertFalse(idle.assistantWorking)

        val working = idle.withInteractionStatus("in_progress")
        assertTrue(working.assistantWorking)
        assertTrue("other fields survive", working.phase == RealtimeVoiceState.Phase.Ready)

        assertFalse(working.withInteractionStatus("idle").assistantWorking)
    }

    @Test
    fun `status is case and whitespace tolerant`() {
        val state = RealtimeVoiceState()
        assertTrue(state.withInteractionStatus(" IN_PROGRESS ").assistantWorking)
    }

    @Test
    fun `an unknown or missing status never claims the assistant is working`() {
        val working = RealtimeVoiceState(assistantWorking = true)
        assertFalse(working.withInteractionStatus("pondering").assistantWorking)
        assertFalse(working.withInteractionStatus(null).assistantWorking)
        assertFalse(RealtimeVoiceState().withInteractionStatus("thinking").assistantWorking)
    }

    @Test
    fun `an unchanged status returns the same state so collectors do not re-render`() {
        val working = RealtimeVoiceState(assistantWorking = true)
        assertSame(working, working.withInteractionStatus("in_progress"))
        val idle = RealtimeVoiceState()
        assertSame(idle, idle.withInteractionStatus("idle"))
    }
}
