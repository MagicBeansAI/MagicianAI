package ai.magicbeans.magdroid.voice

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The live-call panel's caption reducer and mic meter (iOS parity). */
class RealtimeVoiceLiveCallTest {
    @Test
    fun `playback feedback follows the speaker head and clears after drain or interrupt`() {
        val meter = PlaybackLevelMeter()
        val loud = ByteArray(2400) { if (it % 2 == 0) 0 else 0x40 }
        meter.queue(loud, startFrame = 1200, sampleRate = 24000)
        assertEquals(0f, meter.levelAt(0))
        assertTrue(meter.levelAt(1200) > 0.8f)
        assertEquals(0f, meter.levelAt(2400))
        meter.queue(loud, startFrame = 2400, sampleRate = 24000)
        meter.reset()
        assertEquals(0f, meter.levelAt(2400))
    }

    @Test
    fun `a partial grows in place and its final replaces it`() {
        var captions = RealtimeVoiceCaptions.apply(emptyList(), true, "Hel", false, "item-1")
        captions = RealtimeVoiceCaptions.apply(captions, true, "Hello there", false, "item-1")
        assertEquals(1, captions.size)
        assertFalse(captions.single().final)
        // The provider final may carry another item id; the speaker has one open line.
        captions = RealtimeVoiceCaptions.apply(captions, true, "Hello there.", true, "item-2")
        assertEquals(1, captions.size)
        assertTrue(captions.single().final)
        assertEquals("Hello there.", captions.single().text)
    }

    @Test
    fun `assistant lines group by response`() {
        var captions = RealtimeVoiceCaptions.apply(emptyList(), false, "Sure", false, "r1")
        captions = RealtimeVoiceCaptions.apply(captions, false, "Sure, one moment", true, "r1")
        captions = RealtimeVoiceCaptions.apply(captions, false, "Done", false, "r2")
        assertEquals(listOf("Sure, one moment", "Done"), captions.map { it.text })
        assertEquals(listOf(true, false), captions.map { it.final })
    }

    @Test
    fun `captions stay bounded and ids stay unique`() {
        var captions = emptyList<RealtimeVoiceCaption>()
        repeat(RealtimeVoiceCaptions.Max + 4) { index ->
            captions = RealtimeVoiceCaptions.apply(captions, index % 2 == 0, "line $index", true, "s$index")
        }
        assertEquals(RealtimeVoiceCaptions.Max, captions.size)
        assertEquals("line ${RealtimeVoiceCaptions.Max + 3}", captions.last().text)
        assertEquals(captions.size, captions.map { it.id }.toSet().size)
    }

    @Test
    fun `a cleared turn drops only the speaker's unfinished line`() {
        var captions = RealtimeVoiceCaptions.apply(emptyList(), false, "Answer", true, "r1")
        captions = RealtimeVoiceCaptions.apply(captions, true, "um", false, null)
        captions = RealtimeVoiceCaptions.clearUnfinishedUser(captions)
        assertEquals(listOf("Answer"), captions.map { it.text })
    }

    @Test
    fun `silence reads zero and a loud chunk reads high`() {
        assertEquals(0f, MicLevelMeter.levelOf(ByteArray(960), 960))
        val loud = ByteArray(960) { index -> if (index % 2 == 0) 0x00 else 0x40 } // ~0.5 full scale
        assertTrue(MicLevelMeter.levelOf(loud, loud.size) > 0.8f)
    }

    @Test
    fun `the meter throttles and rests when the gate closes`() {
        val meter = MicLevelMeter(intervalNanos = 60_000_000L)
        val loud = ByteArray(960) { index -> if (index % 2 == 0) 0x00 else 0x40 }
        val first = meter.feed(loud, loud.size, gateOpen = true, nowNanos = 100_000_000L)
        assertTrue(first != null && first > 0f)
        assertNull(meter.feed(loud, loud.size, gateOpen = true, nowNanos = 120_000_000L))
        var last: Float? = null
        var now = 200_000_000L
        repeat(40) {
            meter.feed(loud, loud.size, gateOpen = false, nowNanos = now)?.let { last = it }
            now += 60_000_000L
        }
        assertEquals(0f, last)
    }
}
