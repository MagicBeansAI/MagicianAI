package ai.magicbeans.magdroid.tutor

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The tutor's drawing payload, against `TutorShape.swift`.
 *
 * A shape that fails to parse is a step the lesson skips without saying so, so
 * the tolerances here are the point: alias fields, both point shapes, and
 * unknown types surviving.
 */
class TutorShapeTest {
    private val json = Json { ignoreUnknownKeys = true }

    private fun shape(raw: String) = json.decodeFromString(TutorShape.serializer(), raw)

    /** `[x, y]` and `{"x":…}` both appear in real storyboards. */
    @Test
    fun `points decode from an array or an object`() {
        val fromArray = shape("""{"type":"path","points":[[10,20],[30,40]]}""")
        assertEquals(listOf(TutorPoint(10.0, 20.0), TutorPoint(30.0, 40.0)), fromArray.points)

        val fromObject = shape("""{"type":"path","points":[{"x":10,"y":20},{"x":30,"y":40}]}""")
        assertEquals(fromArray.points, fromObject.points)
    }

    /** A box's size arrives as `w`/`h` or `width`/`height`. */
    @Test
    fun `box size reads either spelling`() {
        assertEquals(30.0, shape("""{"type":"rect","w":30,"h":10}""").boxWidth())
        assertEquals(30.0, shape("""{"type":"rect","width":30,"height":10}""").boxWidth())
        assertEquals(10.0, shape("""{"type":"rect","width":30,"height":10}""").boxHeight())
    }

    /** A segment arrives as `x1..y2` or `from_x..to_y`. */
    @Test
    fun `a segment reads either alias`() {
        val numbered = shape("""{"type":"line","x1":0,"y1":0,"x2":10,"y2":10}""").segment()
        val named = shape("""{"type":"arrow","from_x":0,"from_y":0,"to_x":10,"to_y":10}""").segment()
        assertEquals(numbered, named)
        assertEquals(TutorPoint(0.0, 0.0) to TutorPoint(10.0, 10.0), numbered)
    }

    /** A half-specified segment is not drawn rather than drawn from zero. */
    @Test
    fun `an incomplete segment yields nothing`() {
        assertNull(shape("""{"type":"line","x1":0,"y1":0}""").segment())
    }

    /** Words arrive under three different keys depending on the shape. */
    @Test
    fun `the caption reads text, label or formula`() {
        assertEquals("hello", shape("""{"type":"label","text":"hello"}""").caption())
        assertEquals("side a", shape("""{"type":"unit_label","label":"side a"}""").caption())
        assertEquals("a²+b²", shape("""{"type":"formula","formula":"a²+b²"}""").caption())
        assertNull(shape("""{"type":"label"}""").caption())
    }

    /**
     * The backend adds shape types faster than a client ships. One it has never
     * heard of must cost its own drawing, not the whole storyboard.
     */
    @Test
    fun `an unknown type still parses`() {
        val exotic = shape("""{"type":"hyperbola","x":1,"y":2,"novel_field":7}""")
        assertEquals("hyperbola", exotic.type)
        assertEquals(1.0, exotic.x)
    }

    /** The coordinate space is what the renderer scales from. */
    @Test
    fun `the coordinate space decodes`() {
        val scaled = shape("""{"type":"rect","coordinate_space":{"width":1920,"height":1080}}""")
        assertEquals(1920.0, scaled.coordinateSpace?.width)
        assertEquals(1080.0, scaled.coordinateSpace?.height)
    }

    /** Nested shapes are how a callout carries its own leader line. */
    @Test
    fun `children decode recursively`() {
        val group = shape("""{"type":"callout","children":[{"type":"line","x1":0,"y1":0,"x2":5,"y2":5}]}""")
        assertEquals(1, group.children?.size)
        assertEquals("line", group.children?.first()?.type)
    }

    /** Timing and ordering ride on the shape, not on a separate schedule. */
    @Test
    fun `reveal timing decodes`() {
        val timed = shape("""{"type":"rect","delay_ms":250,"duration_ms":600,"reveal_order":3,"animate":true}""")
        assertEquals(250.0, timed.delayMs)
        assertEquals(600.0, timed.durationMs)
        assertEquals(3, timed.revealOrder)
        assertTrue(timed.animate == true)
    }

    /**
     * Every multi-word key, in the wire's own spelling.
     *
     * All of these were annotated in camelCase, which no producer sends. With
     * `ignoreUnknownKeys` each one decoded as absent in silence: font sizes,
     * stroke widths, arc angles, arrow endpoints and the whole `clear_previous`
     * lifecycle. Nothing threw and nothing drew wrong enough to notice — the
     * lesson simply came out in default sizes on a board that never cleared.
     */
    @Test
    fun `multi-word keys decode from snake case`() {
        val full = shape(
            """{"type":"cursive_text","x":180,"y":430,"text":"we","font_size":96,
                "stroke_width":6,"start_angle":15,"end_angle":75,
                "from_x":1,"from_y":2,"to_x":3,"to_y":4,
                "coordinate_space":{"width":1920,"height":1080},
                "capture_image_size":{"width":1290,"height":2796},
                "storyboard_step_id":"cursive-we","tutor_step_label":"Cursive we",
                "step_label":"we","wait_for_voice":true,"reveal_order":2,
                "clear_previous":true,"persist_until_step":"step-9","ttl_ms":4000}""",
        )
        assertEquals(96.0, full.fontSize)
        assertEquals(6.0, full.strokeWidth)
        assertEquals(15.0, full.startAngle)
        assertEquals(75.0, full.endAngle)
        assertEquals(TutorPoint(1.0, 2.0) to TutorPoint(3.0, 4.0), full.segment())
        assertEquals(1920.0, full.coordinateSpace?.width)
        assertEquals(1290.0, full.captureImageSize?.width)
        assertEquals("cursive-we", full.storyboardStepId)
        assertEquals("Cursive we", full.tutorStepLabel)
        assertEquals("we", full.stepLabel)
        assertEquals(true, full.waitForVoice)
        assertEquals(2, full.revealOrder)
        assertEquals(true, full.clearPrevious)
        assertEquals("step-9", full.persistUntilStep)
        assertEquals(4000.0, full.ttlMs)
    }

    /**
     * The camelCase spellings are not the contract. Asserting this pins the
     * bug shut: a future "tidy" back to Kotlin-style names fails here rather
     * than silently emptying every one of these fields again.
     */
    @Test
    fun `camelCase keys are not accepted`() {
        val wrong = shape("""{"type":"label","fontSize":96,"strokeWidth":6,"clearPrevious":true}""")
        assertNull(wrong.fontSize)
        assertNull(wrong.strokeWidth)
        assertNull(wrong.clearPrevious)
    }

    /**
     * A `path` may carry SVG data in `d` instead of a point list, and the
     * contract offers both. Reading only `points` drew nothing for every
     * `d`-carried figure — silently, since an empty path is not an error.
     */
    @Test
    fun `a figure's points come from either points or SVG d`() {
        val listed = shape("""{"type":"path","points":[[10,20],[30,40]]}""")
        assertEquals(listOf(TutorPoint(10.0, 20.0), TutorPoint(30.0, 40.0)), listed.figurePoints())

        val svg = shape("""{"type":"path","d":"M 10 20 L 30 40 L 55.5 -6"}""")
        assertEquals(
            listOf(TutorPoint(10.0, 20.0), TutorPoint(30.0, 40.0), TutorPoint(55.5, -6.0)),
            svg.figurePoints(),
        )
    }

    /** Nothing to draw from is an empty list, not a half-built figure. */
    @Test
    fun `a figure with no usable geometry yields no points`() {
        assertTrue(shape("""{"type":"path"}""").figurePoints().isEmpty())
        assertTrue(shape("""{"type":"path","d":"Z"}""").figurePoints().isEmpty())
        // An odd trailing number cannot make a pair and is dropped.
        assertEquals(1, shape("""{"type":"path","d":"M 1 2 L 3"}""").figurePoints().size)
    }

    /** A storyboard is a list, and one bad entry must not take the rest. */
    @Test
    fun `a storyboard of mixed shapes decodes whole`() {
        val shapes = json.decodeFromString(
            kotlinx.serialization.builtins.ListSerializer(TutorShape.serializer()),
            """[{"type":"rect","x":0,"y":0,"w":5,"h":5},
                {"type":"unheard_of"},
                {"type":"circle","cx":10,"cy":10,"r":4}]""",
        )
        assertEquals(3, shapes.size)
        assertEquals(listOf("rect", "unheard_of", "circle"), shapes.map { it.type })
    }
}
