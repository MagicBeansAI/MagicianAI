package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.RecipeInterpreter
import ai.magicbeans.magdroid.tutor.RecipeTextMetrics
import ai.magicbeans.magdroid.tutor.ResolvedOp
import ai.magicbeans.magdroid.tutor.TutorPoint
import ai.magicbeans.magdroid.tutor.TutorRecipe
import ai.magicbeans.magdroid.tutor.TutorShape
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.PathMeasure
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import kotlin.math.abs
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.sin

/**
 * Draws a shape from its recipe.
 *
 * The interpreter has already resolved every coordinate against the shape, so
 * nothing here knows what a `right_angle_marker` is — it projects points and
 * strokes them. That separation is what lets a scope add a primitive without
 * an app release: a new recipe is new data for this renderer, not new code.
 *
 * The geometry it consumes is asserted by the shared golden fixtures, which the
 * web and iOS suites load too.
 */
object RecipeShapeRenderer {

    /** Below this the arrowhead is not drawn, so the line arrives before its point. */
    private const val ARROWHEAD_PROGRESS = 0.85f

    private const val CURSIVE_POINTS = 92.0
    private const val CURSIVE_MIN_POINTS = 34.0
    private const val CURSIVE_MAX_POINTS = 180.0
    private const val CURSIVE_FLOOR_PX = 16f

    fun DrawScope.renderRecipeShape(
        recipe: TutorRecipe,
        shape: TutorShape,
        canvas: Size,
        progress: Float,
        measurer: TextMeasurer,
    ) {
        val space = shape.coordinateSpace ?: shape.captureImageSize
        val scaleX = if (space != null && space.width > 0) (canvas.width / space.width).toFloat() else 1f
        val scaleY = if (space != null && space.height > 0) (canvas.height / space.height).toFloat() else 1f
        fun px(point: TutorPoint) = Offset((point.x * scaleX).toFloat(), (point.y * scaleY).toFloat())

        val t = progress.coerceIn(0f, 1f)

        // Real glyph metrics, so a recipe sizing a bubble around its label wraps
        // the text this canvas will actually draw rather than a character count.
        val metrics = RecipeTextMetrics { text, fontSize ->
            val layout = measurer.measure(
                AnnotatedString(text),
                TextStyle(fontSize = fontSize.toFloat().sp, fontWeight = FontWeight.SemiBold),
            )
            val widthInSpace = if (scaleX != 0f) layout.size.width / scaleX else layout.size.width.toFloat()
            val heightInSpace = if (scaleY != 0f) layout.size.height / scaleY else layout.size.height.toFloat()
            widthInSpace.toDouble() to heightInSpace.toDouble()
        }

        RecipeInterpreter.resolve(recipe, shape, metrics).forEach { op ->
            drawOp(op, shape, ::px, scaleX, t, measurer)
        }
    }

    private fun DrawScope.drawOp(
        op: ResolvedOp,
        shape: TutorShape,
        px: (TutorPoint) -> Offset,
        scaleX: Float,
        t: Float,
        measurer: TextMeasurer,
    ) {
        val stroke = colorFor(op.colorName ?: shape.color, op.opacity ?: shape.opacity ?: 1.0)
        val fill = colorFor(
            op.colorName ?: shape.fill ?: shape.color,
            op.opacity ?: shape.opacity ?: 0.22,
        )
        val width = op.width.toFloat()

        when (op.op) {
            "line" -> {
                val path = Path().apply {
                    moveTo(px(op.points[0]).x, px(op.points[0]).y)
                    lineTo(px(op.points[1]).x, px(op.points[1]).y)
                }
                strokeProgressive(path, t, stroke, width, op.dashed)
            }

            "polyline", "polygon" -> {
                val path = pathThrough(op.points, px, close = op.closed)
                if (op.fill) drawPath(path, fill)
                if (op.stroke) strokeProgressive(path, t, stroke, width, op.dashed)
            }

            "rect" -> {
                val start = px(op.points[0])
                val end = px(op.points[1])
                val path = Path().apply {
                    addRoundRect(
                        androidx.compose.ui.geometry.RoundRect(
                            left = minOf(start.x, end.x),
                            top = minOf(start.y, end.y),
                            right = maxOf(start.x, end.x),
                            bottom = maxOf(start.y, end.y),
                            cornerRadius = androidx.compose.ui.geometry.CornerRadius(
                                (op.radius * scaleX).toFloat(),
                            ),
                        ),
                    )
                }
                if (op.fill) drawPath(path, fill)
                if (op.stroke) strokeProgressive(path, t, stroke, width, op.dashed)
            }

            "circle" -> {
                val centre = px(op.points[0])
                val radius = abs(px(op.points[1]).x - centre.x)
                if (op.fill) drawCircle(fill, radius, centre)
                if (op.stroke) {
                    val path = Path().apply {
                        addOval(
                            androidx.compose.ui.geometry.Rect(
                                centre.x - radius,
                                centre.y - radius,
                                centre.x + radius,
                                centre.y + radius,
                            ),
                        )
                    }
                    strokeProgressive(path, t, stroke, width, op.dashed)
                }
            }

            // Already sampled into a polyline by the interpreter.
            "arc" -> strokeProgressive(pathThrough(op.points, px, close = false), t, stroke, width, op.dashed)

            "bezier" -> {
                val path = Path().apply {
                    val from = px(op.points.first())
                    val to = px(op.points.last())
                    moveTo(from.x, from.y)
                    val c1 = px(op.points[1])
                    if (op.points.size >= 4) {
                        val c2 = px(op.points[2])
                        cubicTo(c1.x, c1.y, c2.x, c2.y, to.x, to.y)
                    } else {
                        quadraticTo(c1.x, c1.y, to.x, to.y)
                    }
                }
                strokeProgressive(path, t, stroke, width, op.dashed)
            }

            "arrowhead" -> {
                if (t <= ARROWHEAD_PROGRESS) return
                drawArrowHead(px(op.points[0]), px(op.points[1]), stroke, width)
            }

            "label" -> {
                if (t <= op.threshold.toFloat()) return
                val text = op.text ?: return
                val size = (op.fontSize ?: shape.fontSize ?: RecipeInterpreter.LABEL_FONT_SIZE)
                drawAnchoredText(
                    text = text,
                    at = px(op.points[0]),
                    anchor = op.anchor,
                    measurer = measurer,
                    style = TextStyle(
                        color = stroke,
                        fontSize = size.toFloat().sp,
                        fontWeight = FontWeight.SemiBold,
                    ),
                )
            }

            "cursive_label" -> {
                if (t <= op.threshold.toFloat()) return
                val text = op.text ?: return
                val points = (op.fontSize ?: shape.fontSize ?: CURSIVE_POINTS)
                    .coerceIn(CURSIVE_MIN_POINTS, CURSIVE_MAX_POINTS)
                val pixels = (points * scaleX).toFloat().coerceAtLeast(CURSIVE_FLOOR_PX)
                drawAnchoredText(
                    text = text,
                    at = px(op.points[0]),
                    anchor = op.anchor,
                    measurer = measurer,
                    style = TextStyle(
                        color = stroke,
                        fontSize = pixels.toSp(),
                        fontFamily = FontFamily.Cursive,
                        fontWeight = FontWeight.Bold,
                    ),
                )
            }
        }
    }

    private fun pathThrough(points: List<TutorPoint>, px: (TutorPoint) -> Offset, close: Boolean): Path =
        Path().apply {
            val first = px(points.first())
            moveTo(first.x, first.y)
            points.drop(1).forEach { point ->
                val at = px(point)
                lineTo(at.x, at.y)
            }
            if (close) close()
        }

    /**
     * Stroke the leading [progress] of a path, so a shape draws rather than
     * fades. Trimming the path is what makes a line look written; fading its
     * alpha makes it look like a slide transition.
     */
    private fun DrawScope.strokeProgressive(
        path: Path,
        progress: Float,
        color: Color,
        width: Float,
        dashed: Boolean,
    ) {
        val effect = if (dashed) {
            PathEffect.dashPathEffect(floatArrayOf(width * 2, width * 1.5f))
        } else {
            null
        }
        val drawn = if (progress >= 1f) {
            path
        } else {
            Path().also { out ->
                val measure = PathMeasure()
                measure.setPath(path, false)
                measure.getSegment(0f, measure.length * progress, out, true)
            }
        }
        drawPath(drawn, color, style = Stroke(width, pathEffect = effect))
    }

    /**
     * Text placed the way the recipes expect: the anchor names where [at] sits
     * horizontally, and every anchor is vertically centred on it. Compose draws
     * from the top-left, so the offset is applied here — and `text_rise` is half
     * the box for the same reason.
     */
    private fun DrawScope.drawAnchoredText(
        text: String,
        at: Offset,
        anchor: String,
        measurer: TextMeasurer,
        style: TextStyle,
    ) {
        val layout = measurer.measure(AnnotatedString(text), style)
        val dx = when (anchor) {
            "leading" -> 0f
            "trailing" -> -layout.size.width.toFloat()
            else -> -layout.size.width / 2f
        }
        drawText(
            textLayoutResult = layout,
            topLeft = Offset(at.x + dx, at.y - layout.size.height / 2f),
        )
    }

    private fun DrawScope.drawArrowHead(from: Offset, to: Offset, color: Color, width: Float) {
        val angle = atan2((to.y - from.y).toDouble(), (to.x - from.x).toDouble())
        val length = maxOf(12f, width * 4)
        listOf(-1, 1).forEach { side ->
            val spread = angle + side * Math.PI / 6
            drawLine(
                color,
                to,
                Offset(
                    (to.x - length * cos(spread)).toFloat(),
                    (to.y - length * sin(spread)).toFloat(),
                ),
                width,
            )
        }
    }

    /** Named or hex, falling back to the tutor's default ink rather than to nothing. */
    private fun colorFor(raw: String?, opacity: Double): Color =
        TutorShapeRenderer.parseColor(raw ?: "#ffcc00")
            .copy(alpha = opacity.toFloat().coerceIn(0f, 1f))
}
