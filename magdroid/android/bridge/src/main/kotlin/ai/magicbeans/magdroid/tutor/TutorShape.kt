package ai.magicbeans.magdroid.tutor

import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.descriptors.buildClassSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonPrimitive

/**
 * One thing the tutor draws.
 *
 * A single flat record for every shape type, matching `TutorShape.swift`. That
 * looks wasteful and is not: the backend adds shape types faster than a client
 * ships, and a sealed hierarchy would reject a whole storyboard because one
 * step used a rectangle variant this build had never heard of. Unknown types
 * fall through to nothing drawn, and the lesson continues.
 *
 * The field spread is the backend's, kept verbatim — `x`/`y` for boxes, `x1`/
 * `y1` for lines, `from_x`/`to_x` for arrows, `cx`/`cy` for circles. Normalising
 * them here would mean guessing which alias a new type will use.
 *
 * Every [SerialName] below is the wire's own snake_case spelling, matching the
 * `CodingKeys` in `TutorShape.swift` key for key. They were once written in
 * camelCase, which no producer sends: with `ignoreUnknownKeys` the decoder took
 * every one of them as absent, so a storyboard's font sizes, angles, arrow
 * endpoints and — worst — its `clear_previous` lifecycle silently did nothing.
 * A shape's Kotlin property name is not its wire name; do not "tidy" these.
 */
@Serializable
data class TutorShape(
    val type: String = "",
    val id: String? = null,

    // Box
    val x: Double? = null,
    val y: Double? = null,
    val w: Double? = null,
    val h: Double? = null,
    val width: Double? = null,
    val height: Double? = null,

    // Segment
    val x1: Double? = null,
    val y1: Double? = null,
    val x2: Double? = null,
    val y2: Double? = null,
    @SerialName("from_x") val fromX: Double? = null,
    @SerialName("from_y") val fromY: Double? = null,
    @SerialName("to_x") val toX: Double? = null,
    @SerialName("to_y") val toY: Double? = null,

    // Round
    val cx: Double? = null,
    val cy: Double? = null,
    val r: Double? = null,
    val rx: Double? = null,
    val ry: Double? = null,

    // Bezier control points, under each of the three spellings the contract
    // accepts. Read them through [c1x] and friends rather than directly.
    @SerialName("c1x") val rawC1x: Double? = null,
    @SerialName("c1y") val rawC1y: Double? = null,
    @SerialName("c2x") val rawC2x: Double? = null,
    @SerialName("c2y") val rawC2y: Double? = null,
    @SerialName("control1_x") val control1X: Double? = null,
    @SerialName("control1_y") val control1Y: Double? = null,
    @SerialName("control2_x") val control2X: Double? = null,
    @SerialName("control2_y") val control2Y: Double? = null,
    @SerialName("control_x") val controlX: Double? = null,
    @SerialName("control_y") val controlY: Double? = null,

    val points: List<TutorPoint>? = null,
    val d: String? = null,

    val text: String? = null,
    val label: String? = null,
    val formula: String? = null,

    val color: String? = null,
    val fill: String? = null,
    @SerialName("stroke_width") val strokeWidth: Double? = null,
    val opacity: Double? = null,

    /** The space the coordinates are in; the renderer scales from it. */
    @SerialName("coordinate_space") val coordinateSpace: TutorSize? = null,
    @SerialName("capture_image_size") val captureImageSize: TutorSize? = null,

    @SerialName("storyboard_step_id") val storyboardStepId: String? = null,
    @SerialName("tutor_step_label") val tutorStepLabel: String? = null,
    @SerialName("step_label") val stepLabel: String? = null,
    val narration: String? = null,

    @SerialName("delay_ms") val delayMs: Double? = null,
    @SerialName("duration_ms") val durationMs: Double? = null,
    @SerialName("reveal_order") val revealOrder: Int? = null,
    val animate: Boolean? = null,
    @SerialName("wait_for_voice") val waitForVoice: Boolean? = null,

    val children: List<TutorShape>? = null,

    /** Marker radius, for the angle and right-angle markers. */
    val size: Double? = null,
    @SerialName("start_angle") val startAngle: Double? = null,
    @SerialName("end_angle") val endAngle: Double? = null,
    /** `square_on_segment` orientation: "left", "right", or "-1". */
    val side: String? = null,
    val orientation: String? = null,
    @SerialName("font_size") val fontSize: Double? = null,

    // Lifecycle. Without these a long lesson accumulates every mark it drew.
    /** Drop what came before, except shapes marked to persist. */
    @SerialName("clear_previous") val clearPrevious: Boolean? = null,
    /** Survive a `clear_previous` from a later shape. */
    val persist: Boolean? = null,
    /** Stay until the named storyboard step arrives, then go. */
    @SerialName("persist_until_step") val persistUntilStep: String? = null,
    /** Auto-expiry in milliseconds, when the backend sets one. */
    @SerialName("ttl_ms") val ttlMs: Double? = null,
) {
    /** Box width, under either spelling. */
    fun boxWidth(): Double? = w ?: width

    /** Box height, under either spelling. */
    fun boxHeight(): Double? = h ?: height

    /** A segment's ends, whichever alias carried them. */
    fun segment(): Pair<TutorPoint, TutorPoint>? {
        val sx = x1 ?: fromX ?: return null
        val sy = y1 ?: fromY ?: return null
        val ex = x2 ?: toX ?: return null
        val ey = y2 ?: toY ?: return null
        return TutorPoint(sx, sy) to TutorPoint(ex, ey)
    }

    /** The words on the shape, whichever field holds them. */
    fun caption(): String? =
        text?.takeIf { it.isNotBlank() }
            ?: label?.takeIf { it.isNotBlank() }
            ?: formula?.takeIf { it.isNotBlank() }

    /** First control point, under whichever of the three spellings arrived. */
    val c1x: Double? get() = rawC1x ?: control1X ?: controlX
    val c1y: Double? get() = rawC1y ?: control1Y ?: controlY

    /** Second control point. A quadratic curve carries only the first. */
    val c2x: Double? get() = rawC2x ?: control2X
    val c2y: Double? get() = rawC2y ?: control2Y

    /**
     * The shape's numeric fields, by their wire names.
     *
     * This is the bottom layer a recipe expression resolves against — raw field,
     * then injected derived field, then the recipe's own defaults. Only defined
     * values appear, so a coalesce chain can fall through to its next operand
     * rather than stopping on a zero that was never sent.
     */
    fun numericEnvironment(): Map<String, Double> {
        // An explicit map rather than `buildMap`: with a MutableMap receiver in
        // scope, a field named like a map member resolves to the member. `size`
        // silently became `Map.size` — an Int, and not this shape's.
        val env = mutableMapOf<String, Double>()
        fun put(key: String, value: Double?) { if (value != null) env[key] = value }
        put("x", x); put("y", y)
        put("w", w); put("h", h)
        put("width", width); put("height", height)
        put("x1", x1); put("y1", y1); put("x2", x2); put("y2", y2)
        put("from_x", fromX); put("from_y", fromY)
        put("to_x", toX); put("to_y", toY)
        put("cx", cx); put("cy", cy); put("r", r)
        // An ellipse is a first-class request, not a circle that lost precision.
        put("rx", rx); put("ry", ry)
        put("stroke_width", strokeWidth); put("opacity", opacity)
        put("size", size)
        put("start_angle", startAngle); put("end_angle", endAngle)
        put("font_size", fontSize)
        put("c1x", c1x); put("c1y", c1y); put("c2x", c2x); put("c2y", c2y)
        return env
    }

    /** The words on the shape, preferring `text`, as the recipes' `text` ref does. */
    val displayText: String? get() = text ?: formula ?: label

    /**
     * A figure's points, from [points] or from the numbers in SVG [d].
     *
     * The contract offers both for `path`, and reading only [points] meant every
     * `d`-carried figure drew nothing at all. The fallback takes coordinate
     * pairs and ignores the commands, so a curve comes out as its control
     * polygon — crude, and deliberately the same crudeness as
     * `TutorShapeRenderer.swift`. A figure that draws slightly straight on both
     * phones beats one that draws on only one of them.
     */
    fun figurePoints(): List<TutorPoint> {
        points?.let { return it }
        val data = d ?: return emptyList()
        val numbers = NUMBER.findAll(data).mapNotNull { it.value.toDoubleOrNull() }.toList()
        if (numbers.size < 2) return emptyList()
        return (0 until numbers.size - 1 step 2).map { TutorPoint(numbers[it], numbers[it + 1]) }
    }
}

// File scope rather than a companion: a private companion on a @Serializable
// class shadows the generated `serializer()` accessor.
private val NUMBER = Regex("""-?\d*\.?\d+""")

@Serializable
data class TutorSize(val width: Double = 0.0, val height: Double = 0.0)

/**
 * A point, in either shape the backend sends it.
 *
 * `[x, y]` and `{"x":…,"y":…}` both appear in real storyboards. Accepting one
 * would drop every path drawn by whichever generator uses the other.
 */
@Serializable(with = TutorPointSerializer::class)
data class TutorPoint(val x: Double, val y: Double)

object TutorPointSerializer : KSerializer<TutorPoint> {
    override val descriptor: SerialDescriptor = buildClassSerialDescriptor("TutorPoint")

    override fun deserialize(decoder: Decoder): TutorPoint {
        val input = decoder as? JsonDecoder ?: return TutorPoint(0.0, 0.0)
        return when (val element = input.decodeJsonElement()) {
            is JsonArray -> TutorPoint(
                element.getOrNull(0)?.jsonPrimitive?.doubleOrNull ?: 0.0,
                element.getOrNull(1)?.jsonPrimitive?.doubleOrNull ?: 0.0,
            )
            is JsonObject -> TutorPoint(
                element["x"]?.jsonPrimitive?.doubleOrNull ?: 0.0,
                element["y"]?.jsonPrimitive?.doubleOrNull ?: 0.0,
            )
            else -> TutorPoint(0.0, 0.0)
        }
    }

    override fun serialize(encoder: Encoder, value: TutorPoint) {
        encoder.encodeString("[${value.x},${value.y}]")
    }
}
