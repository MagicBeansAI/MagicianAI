package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `@tutor`, against `TutorInvokeTests.swift` case for case.
 *
 * The negatives carry the weight. A rule that fires on any sentence containing
 * the word opens a blackboard over an ordinary message, which is what the
 * `contains` check this replaced actually did.
 */
class TutorInvokeTest {

    @Test
    fun `a leading invoke opens a lesson`() {
        listOf(
            "@tutor explain recursion",
            "@Tutor x",
            "hey tutor teach me",
            "hey, tutur x",
            "@tutur typo tolerant",
            "@tutor: explain recursion",
        ).forEach { assertTrue(it, TutorInvoke.isTutorInvoke(it)) }
    }

    /**
     * Anything not at the front, and anything that merely starts with the same
     * letters, stays an ordinary message. `@copilot` is excluded on purpose: it
     * addresses the Pilot agent, which drives the handset and draws nothing.
     */
    @Test
    fun `a mention that is not an invoke stays a message`() {
        listOf(
            "tutor x",
            "@task do it",
            "@copilot open mail",
            "just plain text",
            "email@tutor.com",
            "@tutor@example.com sent this",
            "hey tutor@example.com wrote",
            "@tutoring session",
            "please @tutor help",
        ).forEach { assertFalse(it, TutorInvoke.isTutorInvoke(it)) }
    }

    @Test
    fun `stripping leaves the bare concept`() {
        assertEquals("explain recursion", TutorInvoke.strip("@tutor explain recursion"))
        assertEquals("explain recursion", TutorInvoke.strip("hey tutor, explain recursion"))
        assertEquals("explain recursion", TutorInvoke.strip("@tutor: explain recursion"))
        assertEquals("big O", TutorInvoke.strip("  @Tutor   big O  "))
        // Nothing to strip: trimmed, otherwise untouched.
        assertEquals("explain recursion", TutorInvoke.strip("explain recursion"))
    }

    @Test
    fun `a staged image picks the overlay canvas`() {
        assertEquals(TutorCanvasMode.ScreenOverlay, TutorInvoke.mode(hasImage = true))
        assertEquals(TutorCanvasMode.Blackboard, TutorInvoke.mode(hasImage = false))
    }

    @Test
    fun `the spoken form normalises to the typed one`() {
        assertEquals(
            TutorInvoke.VoiceInvocation(
                feature = TutorInvoke.VoiceFeature.Tutor,
                canvasMode = TutorCanvasMode.Blackboard,
                quick = false,
                normalizedText = "@tutor explain recursion",
            ),
            TutorInvoke.parseVoiceGuidedFlow("Tutor explain recursion"),
        )
        assertEquals(
            TutorInvoke.VoiceInvocation(
                feature = TutorInvoke.VoiceFeature.Tutor,
                canvasMode = TutorCanvasMode.Blackboard,
                quick = true,
                normalizedText = "@tutor #quick blackboard explain recursion",
            ),
            TutorInvoke.parseVoiceGuidedFlow("Start Tutor Quick blackboard explain recursion"),
        )
    }

    @Test
    fun `screen and app copilot both ask for the overlay`() {
        assertEquals(
            TutorCanvasMode.ScreenOverlay,
            TutorInvoke.parseVoiceGuidedFlow("Tutor screen explain this graph")?.canvasMode,
        )
        val copilot = TutorInvoke.parseVoiceGuidedFlow("App Copilot show me how to create a note")
        assertEquals(TutorInvoke.VoiceFeature.AppCopilot, copilot?.feature)
        assertEquals(TutorCanvasMode.ScreenOverlay, copilot?.canvasMode)
    }

    /** Only a leading command takes over. */
    @Test
    fun `an incidental mention is not a spoken command`() {
        listOf(
            "Can you compare tutor products?",
            "I mentioned app copilot later in this sentence",
            "Please ask hey tutor to explain this",
        ).forEach { assertNull(it, TutorInvoke.parseVoiceGuidedFlow(it)) }
    }

    @Test
    fun `an explicit blackboard beats incidental screen wording`() {
        assertEquals(
            TutorCanvasMode.Blackboard,
            TutorInvoke.parseVoiceGuidedFlow(
                "Tutor blackboard explain what a screen reader does",
            )?.canvasMode,
        )
    }

    /**
     * The subject of the lesson never authorises a capture. Asking about a
     * screenshot is not asking to take one — inferring it from the topic would
     * photograph the handset because of a noun.
     */
    @Test
    fun `topic wording never authorises a capture`() {
        listOf(
            "Tutor explain my app",
            "Tutor explain the current window",
            "Tutor explain why the screenshot is blurry",
        ).forEach {
            val invocation = TutorInvoke.parseVoiceGuidedFlow(it)
            assertEquals(it, TutorCanvasMode.Blackboard, invocation?.canvasMode)
            assertFalse(it, invocation?.requiresScreenCapture ?: true)
        }
    }

    /** The selector right after the command decides, not the words after it. */
    @Test
    fun `the command prefix stays authoritative`() {
        assertEquals(
            TutorCanvasMode.ScreenOverlay,
            TutorInvoke.parseVoiceGuidedFlow("Tutor screen explain the blackboard controls")?.canvasMode,
        )
        assertEquals(
            TutorCanvasMode.Blackboard,
            TutorInvoke.parseVoiceGuidedFlow("Tutor blackboard explain how screenshots work")?.canvasMode,
        )
    }
}
