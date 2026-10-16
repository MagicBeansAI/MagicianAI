package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.PlanGraph
import ai.magicbeans.magdroid.tasks.PlanGraphLayout
import ai.magicbeans.magdroid.tasks.PlanSize
import ai.magicbeans.magdroid.tasks.PlanStep
import ai.magicbeans.magdroid.tasks.PlanStepMark
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.FitScreen
import androidx.compose.material.icons.outlined.ZoomIn
import androidx.compose.material.icons.outlined.ZoomOut
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.TransformOrigin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.sin

/*
 * The plan as a layered top-to-bottom DAG (`PlanGraphLayout`, canvas units are
 * dp) in a pinch-zoom, drag-pan viewport (0.3–3×, zoom −/+ and Fit). Nodes are
 * compact cards: confidence fill, dashed border when no tool resolved, ▶ on a
 * start step, "via agent" when delegated, the live run mark during a run.
 * Port of iOS `PlanGraphCanvas.swift`.
 */

@Composable
internal fun PlanGraphCanvas(
    graph: PlanGraph,
    marks: Map<String, PlanStepMark>,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val palette = activePalette
    val layout = remember(graph) { PlanGraphLayout(graph) }
    val order = remember(graph) {
        val map = HashMap<String, Int>()
        graph.waterfallOrder.forEachIndexed { index, id -> if (id !in map) map[id] = index + 1 }
        map
    }
    val density = LocalDensity.current.density
    var scale by remember { mutableFloatStateOf(1f) }
    var offsetX by remember { mutableFloatStateOf(0f) }
    var offsetY by remember { mutableFloatStateOf(0f) }
    var fittedFor by remember { mutableStateOf("") }

    BoxWithConstraints(modifier.fillMaxSize().clipToBounds()) {
        val viewportW = constraints.maxWidth.toFloat()
        val viewportH = constraints.maxHeight.toFloat()

        fun clamp(value: Float): Float = PlanGraphLayout.clampScale(value.toDouble()).toFloat()

        fun zoomAround(factor: Float, centerX: Float, centerY: Float) {
            val next = clamp(scale * factor)
            val ratio = next / scale
            offsetX = centerX - (centerX - offsetX) * ratio
            offsetY = centerY - (centerY - offsetY) * ratio
            scale = next
        }

        fun fit() {
            val next = layout.fitScale(PlanSize(viewportW / density.toDouble(), viewportH / density.toDouble())).toFloat()
            scale = next
            offsetX = (viewportW - layout.size.width.toFloat() * density * next) / 2f
            offsetY = max((viewportH - layout.size.height.toFloat() * density * next) / 2f, 0f)
        }

        val fitKey = "${graph.steps.size}|${viewportW.toInt()}x${viewportH.toInt()}"
        LaunchedEffect(fitKey) {
            if (viewportW > 0 && fittedFor != fitKey) {
                fittedFor = fitKey
                fit()
            }
        }

        Box(
            Modifier
                .fillMaxSize()
                .pointerInput(Unit) {
                    detectTransformGestures { centroid, pan, zoom, _ ->
                        if (zoom != 1f) zoomAround(zoom, centroid.x, centroid.y)
                        offsetX += pan.x
                        offsetY += pan.y
                    }
                }
                .semantics { contentDescription = "Plan graph. Pinch to zoom, drag to pan." },
        ) {
            Box(
                Modifier
                    .wrapContentSize(Alignment.TopStart, unbounded = true)
                    .requiredSize(layout.size.width.toFloat().dp, layout.size.height.toFloat().dp)
                    .graphicsLayer {
                        transformOrigin = TransformOrigin(0f, 0f)
                        scaleX = scale
                        scaleY = scale
                        translationX = offsetX
                        translationY = offsetY
                    },
            ) {
                val edgeColor = palette.secondaryText.copy(alpha = 0.55f)
                Canvas(Modifier.fillMaxSize()) {
                    val unit = this.density
                    for (segment in layout.segments(graph)) {
                        val from = Offset(segment.from.x.toFloat() * unit, segment.from.y.toFloat() * unit)
                        val to = Offset(segment.to.x.toFloat() * unit, segment.to.y.toFloat() * unit)
                        val bend = max((to.y - from.y) * 0.45f, 12f * unit)
                        val path = Path().apply {
                            moveTo(from.x, from.y)
                            cubicTo(from.x, from.y + bend, to.x, to.y - bend, to.x, to.y)
                        }
                        drawPath(path, edgeColor, style = Stroke(width = 1.5f * unit))
                        drawArrowhead(to, Offset(to.x, to.y - bend), edgeColor, unit)
                    }
                }
                val metrics = layout.metrics
                for (step in graph.steps) {
                    val center = layout.positions[step.id] ?: continue
                    PlanGraphNode(
                        step = step,
                        number = order[step.id] ?: 0,
                        mark = marks[step.id],
                        modifier = Modifier
                            .offset((center.x - metrics.nodeWidth / 2).toFloat().dp, (center.y - metrics.nodeHeight / 2).toFloat().dp)
                            .size(metrics.nodeWidth.toFloat().dp, metrics.nodeHeight.toFloat().dp),
                        onClick = { onSelect(step.id) },
                    )
                }
            }
        }

        Row(
            Modifier
                .align(Alignment.BottomEnd)
                .padding(12.dp)
                .clip(RoundedCornerShape(Lively.chipRadius + 4.dp))
                .background(palette.card.copy(alpha = 0.95f))
                .border(1.dp, palette.cardBorder, RoundedCornerShape(Lively.chipRadius + 4.dp)),
        ) {
            IconButton(onClick = { zoomAround(0.75f, viewportW / 2, viewportH / 2) }) {
                Icon(Icons.Outlined.ZoomOut, "Zoom out", tint = palette.text)
            }
            IconButton(onClick = { zoomAround(1.3f, viewportW / 2, viewportH / 2) }) {
                Icon(Icons.Outlined.ZoomIn, "Zoom in", tint = palette.text)
            }
            IconButton(onClick = { fit() }) {
                Icon(Icons.Outlined.FitScreen, "Fit to screen", tint = palette.text)
            }
        }
    }
}

private fun DrawScope.drawArrowhead(tip: Offset, control: Offset, color: Color, unit: Float) {
    val length = 9f * unit
    val width = 7f * unit
    val angle = atan2(tip.y - control.y, tip.x - control.x)
    val back = Offset(tip.x - length * cos(angle), tip.y - length * sin(angle))
    val normal = Offset(-sin(angle) * width / 2, cos(angle) * width / 2)
    val path = Path().apply {
        moveTo(tip.x, tip.y)
        lineTo(back.x + normal.x, back.y + normal.y)
        lineTo(back.x - normal.x, back.y - normal.y)
        close()
    }
    drawPath(path, color)
}

/** One node card on the graph. */
@Composable
private fun PlanGraphNode(step: PlanStep, number: Int, mark: PlanStepMark?, modifier: Modifier, onClick: () -> Unit) {
    val palette = activePalette
    val bandColor = PlanStyle.bandColor(step.confidence)
    val running = mark == PlanStepMark.InProgress
    val borderColor = when {
        running -> palette.running
        step.isDelegated -> palette.secondaryAccentColor
        else -> bandColor.copy(alpha = 0.75f)
    }
    val dashed = !step.hasResolvedTool && !step.isDelegated
    val shape = RoundedCornerShape(10.dp)
    Column(
        modifier
            .clip(shape)
            .background(palette.card)
            .background(bandColor.copy(alpha = 0.14f))
            .drawBehind {
                val stroke = (if (running) 2.5f else 1.5f) * density
                drawRoundRect(
                    color = borderColor,
                    topLeft = Offset(stroke / 2, stroke / 2),
                    size = androidx.compose.ui.geometry.Size(size.width - stroke, size.height - stroke),
                    cornerRadius = CornerRadius(10f * density - stroke / 2),
                    style = Stroke(
                        width = stroke,
                        pathEffect = if (dashed) PathEffect.dashPathEffect(floatArrayOf(5f * density, 4f * density)) else null,
                    ),
                )
            }
            .clickable(onClickLabel = "Open step", onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = PlanStyle.description(step, number, mark) }
            .padding(8.dp),
        verticalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            if (step.isStart) {
                Box(
                    Modifier.size(16.dp).background(palette.accentFill, RoundedCornerShape(4.dp)),
                    contentAlignment = Alignment.Center,
                ) { Text("▶︎", style = planBodyStyle(8.sp, FontWeight.Bold, palette.onAccent)) }
            }
            Text("$number", style = planMonoStyle(10.sp, FontWeight.SemiBold, palette.secondaryText))
            Spacer(Modifier.weight(1f))
            Text(PlanStyle.percent(step.confidence), style = planMonoStyle(10.sp, FontWeight.SemiBold, bandColor))
            if (mark != null) StatusDot(PlanStyle.tone(mark), size = 8.dp)
        }
        Text(
            PlanStyle.title(step),
            style = planBodyStyle(11.sp, FontWeight.SemiBold).copy(lineHeight = 13.sp),
            maxLines = 2, overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = true).fillMaxWidth(),
        )
        val agent = step.providingAgentId
        if (!agent.isNullOrEmpty()) {
            Text(
                "via $agent",
                style = planBodyStyle(9.sp, FontWeight.SemiBold, palette.secondaryAccentColor),
                maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier
                    .background(palette.secondaryAccentColor.copy(alpha = 0.14f), RoundedCornerShape(4.dp))
                    .padding(horizontal = 5.dp, vertical = 1.dp),
            )
        } else {
            Text(
                if (step.hasResolvedTool) step.tool.orEmpty() else "No tool",
                style = planMonoStyle(9.sp, color = if (step.hasResolvedTool) palette.secondaryText else palette.warning),
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** The legend under the graph. */
@Composable
internal fun PlanGraphLegend() {
    val palette = activePalette
    Row(
        Modifier
            .fillMaxWidth()
            .background(palette.surface.copy(alpha = 0.6f))
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        LegendSwatch(palette.success, "≥70%")
        LegendSwatch(palette.warning, "40–70%")
        LegendSwatch(palette.danger, "<40%")
        LegendBorder(palette.secondaryText, dashed = true, "No tool")
        Text("▶︎ Start", style = planBodyStyle(11.sp, color = palette.secondaryText))
        LegendBorder(palette.secondaryAccentColor, dashed = false, "Delegated")
    }
}

@Composable
private fun LegendSwatch(color: Color, text: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Box(Modifier.size(12.dp).background(color.copy(alpha = 0.5f), RoundedCornerShape(3.dp)))
        Text(text, style = planBodyStyle(11.sp, color = activePalette.secondaryText))
    }
}

@Composable
private fun LegendBorder(color: Color, dashed: Boolean, text: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Box(
            Modifier.width(14.dp).size(14.dp, 12.dp).drawBehind {
                val stroke = 1.5f * density
                drawRoundRect(
                    color = color,
                    topLeft = Offset(stroke / 2, stroke / 2),
                    size = androidx.compose.ui.geometry.Size(size.width - stroke, size.height - stroke),
                    cornerRadius = CornerRadius(3f * density),
                    style = Stroke(stroke, pathEffect = if (dashed) PathEffect.dashPathEffect(floatArrayOf(3f * density, 2f * density)) else null),
                )
            },
        )
        Text(text, style = planBodyStyle(11.sp, color = activePalette.secondaryText))
    }
}
