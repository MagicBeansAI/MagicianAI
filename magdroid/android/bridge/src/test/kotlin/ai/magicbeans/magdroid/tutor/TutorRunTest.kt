package ai.magicbeans.magdroid.tutor

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A run, from its event stream.
 *
 * The lifecycle rules are the substance here. Without them a long lesson
 * accumulates every mark it ever drew, which is how a tutor ends up teaching
 * through a screen it has scribbled over.
 */
class TutorRunTest {

    private fun shapeEvent(body: String) =
        """{"event_type":"tutor.draw.shape","payload":{"shape":$body}}"""

    @Test
    fun `a shape event draws`() {
        val run = TutorRun()
        assertTrue(run.apply(shapeEvent("""{"type":"rect","x":0,"y":0,"w":5,"h":5}""")))
        assertEquals(1, run.shapes.size)
        assertEquals("rect", run.shapes.first().type)
    }

    /** `clear_previous` drops what came before. */
    @Test
    fun `clear previous wipes the board`() {
        val run = TutorRun()
        run.apply(shapeEvent("""{"type":"rect"}"""))
        run.apply(shapeEvent("""{"type":"circle"}"""))
        run.apply(shapeEvent("""{"type":"label","clear_previous":true}"""))
        assertEquals(listOf("label"), run.shapes.map { it.type })
    }

    /** Except what was marked to persist — that is the point of the flag. */
    @Test
    fun `persisting shapes survive a clear`() {
        val run = TutorRun()
        run.apply(shapeEvent("""{"type":"rect","persist":true}"""))
        run.apply(shapeEvent("""{"type":"circle"}"""))
        run.apply(shapeEvent("""{"type":"label","clear_previous":true}"""))
        assertEquals(listOf("rect", "label"), run.shapes.map { it.type })
    }

    /** A shape persisting until a step goes when that step arrives. */
    @Test
    fun `a shape expires at the step it was waiting for`() {
        val run = TutorRun()
        run.apply(shapeEvent("""{"type":"rect","persist_until_step":"step-2"}"""))
        run.apply(shapeEvent("""{"type":"circle","storyboard_step_id":"step-1"}"""))
        assertEquals(2, run.shapes.size)

        run.apply(shapeEvent("""{"type":"label","storyboard_step_id":"step-2"}"""))
        assertEquals(listOf("circle", "label"), run.shapes.map { it.type })
    }

    /**
     * The lifecycle flags must answer to the wire's own spelling.
     *
     * These were written in camelCase, which no producer sends. With
     * `ignoreUnknownKeys` the decoder read them as absent and the board never
     * cleared — the exact failure the tests above were meant to prove could not
     * happen, passing only because they spoke the same wrong dialect.
     */
    @Test
    fun `camelCase lifecycle keys are not the contract`() {
        val run = TutorRun()
        run.apply(shapeEvent("""{"type":"rect"}"""))
        run.apply(shapeEvent("""{"type":"label","clearPrevious":true}"""))
        assertEquals(listOf("rect", "label"), run.shapes.map { it.type })
    }

    /** Narration rides on the shape and becomes the caption. */
    @Test
    fun `a shape's narration becomes the caption`() {
        val run = TutorRun()
        run.apply(shapeEvent("""{"type":"rect","narration":"Here is the button"}"""))
        assertEquals("Here is the button", run.caption)
    }

    /**
     * Progress events narrate without changing what is drawn, so a caller can
     * skip the redraw. That is what the return value is for.
     */
    @Test
    fun `progress events narrate without redrawing`() {
        val run = TutorRun()
        assertFalse(run.apply("""{"event_type":"tutor.step.observed","payload":{}}"""))
        assertEquals("Tutor observed the screen", run.caption)
        assertTrue(run.shapes.isEmpty())
    }

    @Test
    fun `completion clears the board`() {
        val run = TutorRun()
        run.apply(shapeEvent("""{"type":"rect"}"""))
        assertTrue(run.apply("""{"event_type":"tutor.run.completed","payload":{}}"""))
        assertTrue(run.shapes.isEmpty())
        assertTrue(run.finished)
        assertNull(run.caption)
    }

    /** A malformed event is ignored rather than ending the lesson. */
    @Test
    fun `rubbish is ignored`() {
        val run = TutorRun()
        assertFalse(run.apply("not json"))
        assertFalse(run.apply("""{"event_type":"something.else","payload":{}}"""))
        assertTrue(run.shapes.isEmpty())
        assertFalse(run.finished)
    }

    /** A payload that is the shape itself, without the wrapper, still draws. */
    @Test
    fun `an unwrapped shape payload still draws`() {
        val run = TutorRun()
        assertTrue(run.apply("""{"event_type":"tutor.draw.shape","payload":{"type":"circle"}}"""))
        assertEquals(listOf("circle"), run.shapes.map { it.type })
    }
}

/**
 * Where a lesson lands, and what happens when the grant behind it is missing.
 */
class TutorRouterTest {

    @Test
    fun `the overlay is used when it is permitted`() {
        val router = TutorRouter(overlayPermitted = { true })
        assertEquals(TutorSurface.Overlay, router.resolve())
        assertEquals("android_tutor_overlay", router.sourceSurface())
        assertNull(router.refusal.value)
    }

    /**
     * Without the grant the lesson still happens, on the blackboard. Somebody
     * who asked a question wants an answer; landing it in the app rather than
     * over the app is a smaller failure than not answering.
     */
    @Test
    fun `a missing grant falls back rather than refusing`() {
        val router = TutorRouter(overlayPermitted = { false })
        assertEquals(TutorSurface.Blackboard, router.resolve())
        assertTrue(router.refusal.value!!.contains("blackboard"))
    }

    /**
     * The announced surface is the one that will draw, not the one that was
     * chosen — it decides where the backend sends the actions.
     */
    @Test
    fun `the announced surface is the resolved one`() {
        val router = TutorRouter(overlayPermitted = { false })
        assertEquals("android_tutor_blackboard", router.sourceSurface())
    }

    /** Choosing the desktop keeps today's behaviour, deliberately available. */
    @Test
    fun `the desktop stays a choice`() {
        val router = TutorRouter(overlayPermitted = { true })
        router.choose(TutorSurface.Desktop)
        assertEquals("android", router.sourceSurface())
    }

    /** Choosing again clears a refusal left by the previous resolve. */
    @Test
    fun `choosing clears a stale refusal`() {
        val router = TutorRouter(overlayPermitted = { false })
        router.resolve()
        assertTrue(router.refusal.value != null)
        router.choose(TutorSurface.Blackboard)
        assertNull(router.refusal.value)
    }
}
