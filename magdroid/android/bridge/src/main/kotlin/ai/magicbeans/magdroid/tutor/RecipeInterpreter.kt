package ai.magicbeans.magdroid.tutor

import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.cos
import kotlin.math.sin

/**
 * How wide and tall some text is when drawn.
 *
 * Injected rather than computed here because measurement needs the font, which
 * lives with the canvas. It is also the one part of a recipe that is honestly
 * platform-specific: the web measures its own glyphs and anchors on the
 * baseline, iOS measures with UIFont and anchors centred. The shared golden
 * fixtures avoid measured text for exactly this reason.
 */
fun interface RecipeTextMetrics {
    /** Returns width to height, in the shape's coordinate space. */
    fun measure(text: String, fontSize: Double): Pair<Double, Double>

    companion object {
        /**
         * A rough estimate, for callers with no font to hand.
         *
         * Used only when the drawing surface does not supply real metrics; the
         * shipped canvas always does. It exists so the pure interpreter can be
         * exercised without a Compose environment.
         */
        val Estimated = RecipeTextMetrics { text, fontSize ->
            text.length * fontSize * 0.55 to fontSize * 1.2
        }
    }
}

/**
 * One op, with every argument already resolved against the shape.
 *
 * Geometry is in the shape's own coordinate space, pre-projection — the canvas
 * scales it. [points] carries each op's geometry in the encoding the shared
 * fixtures document: `rect` as `[[x,y],[x+w,y+h]]`, `circle` as
 * `[[cx,cy],[cx+r,cy]]`, `bezier` as `[from, c1, (c2?), to]`, `arc` as its
 * sampled polyline, and everything else as its vertices.
 */
data class ResolvedOp(
    val op: String,
    val points: List<TutorPoint> = emptyList(),
    val closed: Boolean = false,
    val text: String? = null,
    val anchor: String = "center",
    val threshold: Double = 0.15,
    val fontSize: Double? = null,
    val colorName: String? = null,
    val opacity: Double? = null,
    val width: Double = 4.0,
    val dashed: Boolean = false,
    val stroke: Boolean = true,
    val fill: Boolean = false,
    val radius: Double = 6.0,
)

/** The space-coordinate geometry one stroked op produces. The fixture seam. */
data class OpGeometry(val op: String, val points: List<TutorPoint>, val closed: Boolean)

/**
 * Turns a [TutorRecipe] into resolved draw ops.
 *
 * A port of `RecipeInterpreter.swift`, against the normative contract in
 * Appendix A of `docs/archive/plans/2026-07-13-data-driven-tutor-primitives.md`.
 * This is what makes a primitive a *data* decision: a scope drops a JSON file
 * into its `tutor_primitives/` folder and every client draws the new shape
 * without shipping a build.
 *
 * Everything here is pure. The canvas-facing half only projects and strokes
 * what this produces, which is why the shared golden fixtures can assert it —
 * [geometry] is derived from the same resolution the renderer consumes, so the
 * tested seam and the drawn output cannot drift apart.
 */
object RecipeInterpreter {

    /** Hostile-input bounds, per Appendix A. These arrive over the network. */
    const val MAX_OPS = 64
    const val MAX_POINTS_PER_OP = 256

    /** The size a non-cursive label draws at, shared with the measurement seam. */
    const val LABEL_FONT_SIZE = 15.0

    private val STROKED = setOf(
        "line", "polyline", "polygon", "rect", "circle", "arc", "bezier", "arrowhead",
    )

    /**
     * Every op of [recipe], resolved against [shape].
     *
     * An op missing a coordinate it needs is dropped rather than defaulted: a
     * shape with no radius should draw no circle, not a circle at zero.
     */
    fun resolve(
        recipe: TutorRecipe,
        shape: TutorShape,
        metrics: RecipeTextMetrics = RecipeTextMetrics.Estimated,
    ): List<ResolvedOp> {
        val env = RecipeEnvironment(shape, recipe.defaults, metrics)
        return recipe.draw.take(MAX_OPS).mapNotNull { resolveOp(it, env, shape) }
    }

    /**
     * The stroked geometry [recipe] produces for [shape], in space coordinates.
     *
     * Label ops carry no stroked geometry and are omitted. This is the seam the
     * cross-platform fixtures in `docs/components/magician/tutor-primitive-fixtures`
     * assert against — the same files the web and iOS suites load, so an
     * interpreter that drifts reddens a suite on every platform rather than
     * quietly drawing something different on one of them.
     */
    fun geometry(recipe: TutorRecipe, shape: TutorShape): List<OpGeometry> =
        resolve(recipe, shape)
            .filter { it.op in STROKED }
            .map { OpGeometry(it.op, it.points, it.closed) }

    private fun resolveOp(op: RecipeOp, env: RecipeEnvironment, shape: TutorShape): ResolvedOp? {
        val kind = op.op.lowercase()
        val params = op.params

        // Styling, resolved once. A colour named on the op wins over the
        // shape's; absent, the canvas falls back to the shape's own ink.
        val styled = { points: List<TutorPoint>, closed: Boolean, fillable: Boolean ->
            ResolvedOp(
                op = kind,
                points = points,
                closed = closed,
                colorName = env.stringValue(params["color"]),
                opacity = env.number(params["opacity"]),
                width = maxOf(1.0, env.number(params["width"]) ?: shape.strokeWidth ?: 4.0),
                dashed = env.bool(params["dashed"]) ?: false,
                stroke = env.bool(params["stroke"]) ?: true,
                // `rect`/`circle`/`polygon` fill when the op says so, or when
                // the op is silent and the shape itself carries a fill.
                fill = fillable && (env.bool(params["fill"]) ?: (shape.fill != null)),
                radius = env.number(params["radius"]) ?: 6.0,
            )
        }

        return when (kind) {
            "line" -> {
                val from = env.point(params["from"]) ?: return null
                val to = env.point(params["to"]) ?: return null
                styled(listOf(from, to), false, false)
            }

            "polyline", "polygon" -> {
                val pts = env.points(params["points"], shape).take(MAX_POINTS_PER_OP)
                if (pts.isEmpty()) return null
                styled(pts, kind == "polygon", kind == "polygon")
            }

            "rect" -> {
                val x = env.number(params["x"]) ?: return null
                val y = env.number(params["y"]) ?: return null
                val w = env.number(params["w"]) ?: return null
                val h = env.number(params["h"]) ?: return null
                styled(listOf(TutorPoint(x, y), TutorPoint(x + w, y + h)), true, true)
            }

            "circle" -> {
                val cx = env.number(params["cx"]) ?: return null
                val cy = env.number(params["cy"]) ?: return null
                val r = env.number(params["r"]) ?: return null
                styled(listOf(TutorPoint(cx, cy), TutorPoint(cx + r, cy)), true, true)
            }

            "arc" -> {
                val pts = arcPoints(params, env).take(MAX_POINTS_PER_OP)
                if (pts.isEmpty()) return null
                styled(pts, false, false)
            }

            "bezier" -> {
                val from = env.point(params["from"]) ?: return null
                val to = env.point(params["to"]) ?: return null
                val c1 = env.point(params["c1"]) ?: return null
                val c2 = env.point(params["c2"])
                styled(listOfNotNull(from, c1, c2, to), false, false)
            }

            "arrowhead" -> {
                val from = env.point(params["from"]) ?: return null
                val to = env.point(params["to"]) ?: return null
                styled(listOf(from, to), false, false)
            }

            "label", "cursive_label" -> {
                val text = env.text(params["text"], shape) ?: return null
                val at = env.point(params["at"]) ?: return null
                styled(listOf(at), false, false).copy(
                    text = text,
                    anchor = env.stringValue(params["anchor"])?.lowercase() ?: "center",
                    threshold = env.number(params["threshold"]) ?: 0.15,
                    fontSize = env.number(params["size"]),
                )
            }

            // An op this build has never heard of draws nothing; the rest of
            // the recipe still draws. That is the whole resilience argument for
            // shipping primitives as data.
            else -> null
        }
    }

    /**
     * An arc, sampled into a polyline in space coordinates.
     *
     * `rx`/`ry` each fall back to `r`, so a circular arc keeps its exact
     * geometry while an ellipse stays addressable — the base of a cone is a
     * circle seen at an angle, and drawing it as a circle makes a party hat.
     */
    private fun arcPoints(params: Map<String, RecipeValue>, env: RecipeEnvironment): List<TutorPoint> {
        val cx = env.number(params["cx"]) ?: return emptyList()
        val cy = env.number(params["cy"]) ?: return emptyList()
        val r = env.number(params["r"])
        val rx = env.number(params["rx"]) ?: r ?: return emptyList()
        val ry = env.number(params["ry"]) ?: r ?: return emptyList()
        val a1 = (env.number(params["from"]) ?: 0.0) * Math.PI / 180
        val a2 = (env.number(params["to"]) ?: 90.0) * Math.PI / 180
        // A full ellipse needs more than the 24 steps a quarter-turn was tuned
        // for, or the closing seam shows as a visible polygon edge.
        val sweep = abs(a2 - a1)
        val steps = maxOf(24, minOf(96, ceil(sweep / (Math.PI / 2) * 24).toInt()))
        return (0..steps).map { index ->
            val t = a1 + (a2 - a1) * index / steps
            TutorPoint(cx + cos(t) * rx, cy + sin(t) * ry)
        }
    }
}

/**
 * What an identifier in a recipe expression means for one shape.
 *
 * Resolution order is normative: the shape's own field, then an injected
 * derived field, then the recipe's defaults. The derived fields exist so a
 * recipe can write `sx` instead of spelling out `from_x|x1` every time.
 */
class RecipeEnvironment(
    shape: TutorShape,
    private val defaults: Map<String, Double>,
    metrics: RecipeTextMetrics = RecipeTextMetrics.Estimated,
) {
    private val base: Map<String, Double> = shape.numericEnvironment()

    private val derived: Map<String, Double> = buildDerived(shape, defaults, metrics)

    private fun buildDerived(
        shape: TutorShape,
        defaults: Map<String, Double>,
        metrics: RecipeTextMetrics,
    ): Map<String, Double> {
        // An explicit map, not `buildMap`: with a MutableMap receiver in scope a
        // local `put` taking a nullable calls itself rather than the member, and
        // recurses until the stack goes.
        val out = mutableMapOf<String, Double>()
        fun put(key: String, value: Double?) { if (value != null) out[key] = value }

        val sx = shape.fromX ?: shape.x1
        val sy = shape.fromY ?: shape.y1
        val ex = shape.toX ?: shape.x2
        val ey = shape.toY ?: shape.y2
        put("sx", sx); put("sy", sy); put("ex", ex); put("ey", ey)
        put("cx", shape.cx ?: shape.x)
        put("cy", shape.cy ?: shape.y)
        put("w", shape.w ?: shape.width)
        put("h", shape.h ?: shape.height)
        if (sx != null && ex != null) put("mx", (sx + ex) / 2)
        if (sy != null && ey != null) put("my", (sy + ey) / 2)

        val right = shape.side == "right" || shape.side == "-1" || shape.orientation == "right"
        put("side_sign", if (right) -1.0 else 1.0)

        val words = shape.displayText ?: shape.d
        if (words != null) {
            put("text_len", words.length.toDouble())
            // The measured glyph box, so a recipe can size a background around
            // real text. `text_len` stays for recipes that want a count; sizing
            // from a count is what let a long label run out of its own bubble.
            val size = shape.fontSize ?: defaults["font_size"] ?: RecipeInterpreter.LABEL_FONT_SIZE
            val (width, height) = metrics.measure(words, size)
            put("text_w", width)
            put("text_h", height)
            // Ink ABOVE the anchor point — half the box, not the ascent,
            // because this canvas anchors labels vertically centred. The web
            // anchors on the baseline and sets the same name to the ascent.
            // Anchor-relative is what lets one recipe be correct on both.
            put("text_rise", height / 2)
        }
        return out
    }

    /** Raw field, then derived, then the recipe's default. */
    fun resolve(name: String): Double? = base[name] ?: derived[name] ?: defaults[name]

    /** A numeric parameter: a literal, or an expression resolved against this shape. */
    fun number(value: RecipeValue?): Double? = when (value) {
        is RecipeValue.Num -> value.value
        is RecipeValue.Str -> RecipeExpression.evaluate(value.value) { resolve(it) }
            ?.takeUnless { it.isNaN() }

        else -> null
    }

    /** An `[x, y]` pair, each component resolved on its own. */
    fun point(value: RecipeValue?): TutorPoint? {
        val items = (value as? RecipeValue.Arr)?.values ?: return null
        if (items.size < 2) return null
        val x = number(items[0]) ?: return null
        val y = number(items[1]) ?: return null
        return TutorPoint(x, y)
    }

    /**
     * A `points` parameter: a literal `[[x,y]…]`, or a field reference naming
     * the shape's own point list.
     */
    fun points(value: RecipeValue?, shape: TutorShape): List<TutorPoint> = when (value) {
        is RecipeValue.Arr -> value.values.mapNotNull { point(it) }
        is RecipeValue.Str -> shape.figurePoints()
        else -> emptyList()
    }

    /** A parameter's raw string — for `anchor` and `color`, never evaluated. */
    fun stringValue(value: RecipeValue?): String? = (value as? RecipeValue.Str)?.value

    /** A text parameter: a field reference chain like `"text|d"`, else a literal. */
    fun text(value: RecipeValue?, shape: TutorShape): String? {
        val raw = (value as? RecipeValue.Str)?.value ?: return null
        for (token in raw.split('|').map { it.trim() }) {
            when (token) {
                "text" -> shape.displayText?.let { return it }
                "label" -> shape.label?.let { return it }
                "formula" -> shape.formula?.let { return it }
                "d" -> shape.d?.let { return it }
                else -> return token
            }
        }
        return null
    }

    fun bool(value: RecipeValue?): Boolean? = when (value) {
        is RecipeValue.Num -> value.value != 0.0
        is RecipeValue.Str -> when (value.value.lowercase()) {
            "true" -> true
            "false" -> false
            else -> null
        }

        else -> null
    }
}
