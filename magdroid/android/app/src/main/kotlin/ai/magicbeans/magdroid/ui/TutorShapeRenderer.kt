package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.TutorPoint
import ai.magicbeans.magdroid.tutor.TutorPrimitiveRegistry
import ai.magicbeans.magdroid.tutor.TutorShape
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import kotlin.math.cos
import kotlin.math.hypot
import kotlin.math.sin

/**
 * Draws one tutor shape.
 *
 * A port of `TutorShapeRenderer.swift`, type name for type name, so a
 * storyboard the backend wrote for the web or iOS draws the same here. The
 * aliases are kept rather than collapsed: `rect`, `rectangle` and `highlight`
 * are one drawing on purpose, and a generator that emits any of the three is
 * already in the wild.
 *
 * Progress animates a shape in. It is passed through rather than owned here so
 * a caller can scrub, replay, or jump to the end — the renderer stays a pure
 * function of shape and time.
 */
object TutorShapeRenderer {

    /**
     * Draw [shape], scaled from its own coordinate space into [canvas].
     *
     * Unknown types draw nothing and do not throw. The backend adds shapes
     * faster than a client ships, and one unrecognised step must cost its own
     * drawing rather than the rest of the lesson.
     */
    fun DrawScope.renderTutorShape(
        shape: TutorShape,
        canvas: Size,
        progress: Float,
        measurer: TextMeasurer,
    ) {
        val space = shape.coordinateSpace ?: shape.captureImageSize
        val scaleX = if (space != null && space.width > 0) (canvas.width / space.width).toFloat() else 1f
        val scaleY = if (space != null && space.height > 0) (canvas.height / space.height).toFloat() else 1f
        fun px(x: Double, y: Double) = Offset((x * scaleX).toFloat(), (y * scaleY).toFloat())

        val stroke = parseColor(shape.color ?: DEFAULT_INK).copy(
            alpha = (shape.opacity ?: 1.0).toFloat().coerceIn(0f, 1f),
        )
        val fill = parseColor(shape.fill ?: shape.color ?: DEFAULT_INK).copy(
            alpha = (shape.opacity ?: 0.22).toFloat().coerceIn(0f, 1f),
        )
        val width = ((shape.strokeWidth ?: 4.0).coerceAtLeast(1.0)).toFloat()
        val t = progress.coerceIn(0f, 1f)

        // The recipe path first. A primitive the backend serves draws from
        // data, which is what lets a scope author a new shape without an app
        // release; the hand-written branches below are the fallback for a type
        // with no recipe — an empty bundle, or before the first fetch lands.
        // Resolved before the registry is asked, not after. `path` claims
        // `handwriting` as an alias, so a text-backed handwriting shape used to
        // reach the path recipe, find no points and draw nothing — the routing
        // below never ran, because a recipe had already answered for it.
        val figurePoints = shape.figurePoints()
        val kind = shape.type.lowercase()
        val drawAs = if (kind == "handwriting" && figurePoints.isEmpty()) "cursive_text" else kind

        TutorPrimitiveRegistry.shared.recipe(drawAs)?.let { recipe ->
            with(RecipeShapeRenderer) {
                renderRecipeShape(recipe, shape, canvas, progress, measurer)
            }
            return
        }

        when (drawAs) {
            "rect", "rectangle", "highlight", "box", "frame" -> {
                val ox = shape.x ?: return
                val oy = shape.y ?: return
                val bw = shape.boxWidth() ?: return
                val bh = shape.boxHeight() ?: return
                val origin = px(ox, oy)
                val size = Size((bw * scaleX).toFloat(), (bh * scaleY).toFloat() * t)
                if (shape.fill != null) drawRect(fill, origin, size)
                drawRect(stroke, origin, size, style = Stroke(width))
            }

            "line", "axis", "arrow", "side_label" -> {
                val (from, to) = shape.segment() ?: return
                val a = px(from.x, from.y)
                val b = px(to.x, to.y)
                // Grown from the start, so an animating line is drawn rather
                // than faded — which is what makes it read as being drawn.
                val end = Offset(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
                drawLine(stroke, a, end, width)
                if (kind == "arrow" && t > 0.9f) drawArrowHead(a, b, stroke, width)
            }

            "circle" -> {
                val cx = shape.cx ?: shape.x ?: return
                val cy = shape.cy ?: shape.y ?: return
                val radius = shape.r ?: shape.rx ?: return
                val centre = px(cx, cy)
                val rpx = (radius * scaleX).toFloat() * t
                if (shape.fill != null) drawCircle(fill, rpx, centre)
                drawCircle(stroke, rpx, centre, style = Stroke(width))
            }

            "label", "callout", "formula", "unit_label", "timeline_tick" -> {
                val words = shape.caption() ?: return
                val at = px(shape.x ?: 0.0, shape.y ?: 0.0)
                val size = (shape.fontSize ?: LABEL_POINTS).sp
                // Text appears whole once its turn arrives. Typing it out
                // character by character reads as a stutter at this size.
                if (t <= 0.05f) return
                drawText(
                    textMeasurer = measurer,
                    text = words,
                    topLeft = at,
                    style = TextStyle(color = stroke, fontSize = size),
                )
            }

            "path", "polygon", "polyline", "handwriting", "area_fill", "freehand" -> {
                val pts = figurePoints.takeIf { it.size >= 2 } ?: return
                val path = buildPath(pts, t) { p -> px(p.x, p.y) }
                if (kind == "area_fill" || shape.fill != null) drawPath(path, fill)
                drawPath(path, stroke, style = Stroke(width))
            }

            // Handwriting, in a script face at display size. The blackboard
            // writes cursive far larger than a caption — the contract's own
            // default is 92 against a label's 15 — so folding this in with
            // `label` rendered a written word as an ordinary small annotation.
            "cursive_text" -> {
                val words = shape.caption() ?: shape.d ?: return
                val at = px(shape.x ?: return, shape.y ?: return)
                if (t <= 0.15f) return
                val points = (shape.fontSize ?: CURSIVE_POINTS)
                    .coerceIn(CURSIVE_MIN_POINTS, CURSIVE_MAX_POINTS)
                val pixels = (points * scaleX).toFloat().coerceAtLeast(CURSIVE_FLOOR_PX)
                drawText(
                    textMeasurer = measurer,
                    text = words,
                    topLeft = at,
                    style = TextStyle(
                        color = stroke,
                        fontSize = pixels.toSp(),
                        fontFamily = FontFamily.Cursive,
                        fontWeight = FontWeight.Bold,
                    ),
                )
            }

            "right_angle_marker" -> {
                val at = px(shape.x ?: return, shape.y ?: return)
                val side = ((shape.size ?: 12.0) * scaleX).toFloat() * t
                val path = Path().apply {
                    moveTo(at.x, at.y - side)
                    lineTo(at.x + side, at.y - side)
                    lineTo(at.x + side, at.y)
                }
                drawPath(path, stroke, style = Stroke(width))
            }

            "angle_marker", "perpendicular_marker", "parallel_marker", "arc" -> {
                val centre = px(shape.cx ?: shape.x ?: return, shape.cy ?: shape.y ?: return)
                val radius = ((shape.r ?: shape.size ?: 20.0) * scaleX).toFloat()
                val start = (shape.startAngle ?: 0.0).toFloat()
                val sweep = ((shape.endAngle ?: 90.0) - (shape.startAngle ?: 0.0)).toFloat() * t
                drawArc(
                    color = stroke,
                    startAngle = start,
                    sweepAngle = sweep,
                    useCenter = false,
                    topLeft = Offset(centre.x - radius, centre.y - radius),
                    size = Size(radius * 2, radius * 2),
                    style = Stroke(width),
                )
            }

            "square_on_segment" -> {
                val (from, to) = shape.segment() ?: return
                val a = px(from.x, from.y)
                val b = px(to.x, to.y)
                // Raised on the named side, so the square lands where the
                // storyboard meant rather than wherever the normal happened to
                // point.
                val flip = if (shape.side == "right" || shape.side == "-1") -1f else 1f
                val dx = b.x - a.x
                val dy = b.y - a.y
                val nx = -dy * flip * t
                val ny = dx * flip * t
                val path = Path().apply {
                    moveTo(a.x, a.y)
                    lineTo(b.x, b.y)
                    lineTo(b.x + nx, b.y + ny)
                    lineTo(a.x + nx, a.y + ny)
                    close()
                }
                if (shape.fill != null) drawPath(path, fill)
                drawPath(path, stroke, style = Stroke(width))
            }

            "curve" -> {
                val pts = figurePoints.takeIf { it.size >= 2 } ?: return
                drawPath(buildPath(pts, t) { p -> px(p.x, p.y) }, stroke, style = Stroke(width))
            }

            // Unrecognised: nothing drawn, lesson continues.
            else -> Unit
        }
    }

    /** A polyline grown to [t] of its length, so it draws rather than fades. */
    private fun buildPath(points: List<TutorPoint>, t: Float, project: (TutorPoint) -> Offset): Path {
        val path = Path()
        val visible = (points.size * t).toInt().coerceAtLeast(2).coerceAtMost(points.size)
        val first = project(points[0])
        path.moveTo(first.x, first.y)
        for (index in 1 until visible) {
            val p = project(points[index])
            path.lineTo(p.x, p.y)
        }
        return path
    }

    private fun DrawScope.drawArrowHead(from: Offset, to: Offset, color: Color, width: Float) {
        val length = hypot((to.x - from.x).toDouble(), (to.y - from.y).toDouble())
        if (length < 1) return
        val angle = kotlin.math.atan2((to.y - from.y).toDouble(), (to.x - from.x).toDouble())
        val head = (width * 4).coerceAtLeast(8f)
        for (spread in listOf(2.6, -2.6)) {
            drawLine(
                color,
                to,
                Offset(
                    (to.x + head * cos(angle + spread)).toFloat(),
                    (to.y + head * sin(angle + spread)).toFloat(),
                ),
                width,
            )
        }
    }

    private const val DEFAULT_INK = "#ffcc00"
    private const val LABEL_POINTS = 15.0

    // Cursive sizing, matching `TutorShapeRenderer.swift`: the storyboard's
    // point size clamped into a legible band, scaled into the canvas, then
    // floored so a small coordinate space cannot shrink it to nothing.
    private const val CURSIVE_POINTS = 92.0
    private const val CURSIVE_MIN_POINTS = 34.0
    private const val CURSIVE_MAX_POINTS = 180.0
    private const val CURSIVE_FLOOR_PX = 16f

    /**
     * A colour, by name or by hex.
     *
     * Storyboards use both, and the named set is the one iOS answers to. An
     * unknown name falls back to the default ink rather than to transparent —
     * an invisible shape is indistinguishable from a renderer that is broken.
     */
    fun parseColor(raw: String): Color {
        val value = raw.trim().lowercase()
        NAMED[value]?.let { return it }
        val hex = value.removePrefix("#")
        val parsed = when (hex.length) {
            6 -> runCatching { 0xFF000000L or hex.toLong(16) }.getOrNull()
            8 -> runCatching { hex.toLong(16) }.getOrNull()
            else -> null
        }
        return parsed?.let { Color(it.toULong() shl 32) } ?: Color(0xFFFFCC00)
    }

    private val NAMED = mapOf(
        "red" to Color(0xFFFF3B30),
        "orange" to Color(0xFFFF9500),
        "yellow" to Color(0xFFFFCC00),
        "green" to Color(0xFF34C759),
        "blue" to Color(0xFF007AFF),
        "purple" to Color(0xFFAF52DE),
        "white" to Color(0xFFFFFFFF),
        "black" to Color(0xFF000000),
    )
}
