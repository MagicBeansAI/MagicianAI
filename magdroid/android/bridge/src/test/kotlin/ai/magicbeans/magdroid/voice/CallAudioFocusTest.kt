package ai.magicbeans.magdroid.voice

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CallAudioFocusTest {
    @Test fun `app speech is allowed only while no live call holds focus`() {
        val focus = CallAudioFocus()
        assertTrue(focus.permitsLocalSpeech())
        val call = Any()
        focus.acquire(call)
        assertFalse(focus.permitsLocalSpeech()); assertTrue(focus.active.value)
        focus.release(call)
        assertTrue(focus.permitsLocalSpeech()); assertFalse(focus.active.value)
    }

    @Test fun `a second call keeps focus until both have hung up`() {
        val focus = CallAudioFocus()
        val chat = Any(); val wake = Any()
        focus.acquire(chat); focus.acquire(wake); focus.acquire(chat)
        focus.release(chat)
        assertFalse(focus.permitsLocalSpeech())
        focus.release(wake)
        assertTrue(focus.permitsLocalSpeech())
    }

    @Test fun `a stale release of an ended call does not free a newer call`() {
        val focus = CallAudioFocus()
        val first = Any(); val second = Any()
        focus.acquire(first); focus.release(first)
        focus.acquire(second); focus.release(first)
        assertFalse(focus.permitsLocalSpeech())
    }
}
