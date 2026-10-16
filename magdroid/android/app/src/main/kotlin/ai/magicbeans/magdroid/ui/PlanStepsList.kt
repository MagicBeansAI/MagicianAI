package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.PlanConfidenceBand
import ai.magicbeans.magdroid.tasks.PlanGraph
import ai.magicbeans.magdroid.tasks.PlanJson
import ai.magicbeans.magdroid.tasks.PlanStep
import ai.magicbeans.magdroid.tasks.PlanStepMark
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.SubdirectoryArrowRight
import androidx.compose.material.icons.outlined.AddCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.DragHandle
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.KeyboardArrowUp
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.zIndex
import kotlinx.serialization.json.JsonElement
import kotlin.math.roundToInt

/*
 * Plan Inspector shared pieces (tags, cards, JSON disclosure) and the Steps
 * view: the read-only waterfall and the edit list (drag to reorder, swipe or
 * ✕ to delete, Add step). Port of iOS `PlanStepsView.swift`.
 */

/** Colour and wording rules shared by the inspector's views. */
internal object PlanStyle {
    fun bandColor(confidence: Double, palette: Palette = activePalette): Color = when (PlanConfidenceBand.of(confidence)) {
        PlanConfidenceBand.High -> palette.success
        PlanConfidenceBand.Medium -> palette.warning
        PlanConfidenceBand.Low -> palette.danger
    }

    fun tone(mark: PlanStepMark): StatusTone = when (mark) {
        PlanStepMark.Done -> StatusTone.Done
        PlanStepMark.Failed -> StatusTone.Failed
        PlanStepMark.InProgress -> StatusTone.Running
        PlanStepMark.Pending -> StatusTone.NotStarted
        PlanStepMark.Skipped, PlanStepMark.NotReached -> StatusTone.Cancelled
    }

    fun markLabel(mark: PlanStepMark): String = when (mark) {
        PlanStepMark.Done -> "done"
        PlanStepMark.Failed -> "failed"
        PlanStepMark.InProgress -> "running"
        PlanStepMark.Pending -> "not started"
        PlanStepMark.Skipped -> "skipped"
        PlanStepMark.NotReached -> "not reached"
    }

    fun percent(value: Double?): String =
        if (value == null || !value.isFinite()) "—" else "${(value * 100).roundToInt()}%"

    fun truncate(text: String, length: Int): String = if (text.length > length) text.take(length) + "…" else text

    fun title(step: PlanStep): String = step.task.ifEmpty { step.id }

    fun title(graph: PlanGraph, id: String): String = graph.step(id)?.let { title(it) } ?: id

    fun plural(count: Int, word: String): String = "$count $word${if (count == 1) "" else "s"}"

    fun description(step: PlanStep, number: Int, mark: PlanStepMark?): String {
        val parts = mutableListOf("Step $number", title(step), "confidence ${percent(step.confidence)}")
        val agent = step.providingAgentId
        if (!agent.isNullOrEmpty()) parts.add("delegated to $agent")
        else parts.add(if (step.hasResolvedTool) "tool ${step.tool}" else "no tool resolved")
        if (step.isStart) parts.add("start step")
        mark?.let { parts.add(markLabel(it)) }
        return parts.joinToString(", ")
    }
}

@Composable
internal fun planBodyStyle(size: TextUnit, weight: FontWeight = FontWeight.Normal, color: Color = activePalette.text): TextStyle =
    TextStyle(fontSize = size, fontWeight = weight, color = color, fontFamily = LocalMagicanFontFamilies.current.body)

@Composable
internal fun planMonoStyle(size: TextUnit, weight: FontWeight = FontWeight.Normal, color: Color = activePalette.text): TextStyle =
    TextStyle(fontSize = size, fontWeight = weight, color = color, fontFamily = LocalMagicanFontFamilies.current.mono)

/** A small non-interactive tag: rounded square, tinted by [color]. */
@Composable
internal fun PlanTag(text: String, color: Color? = null, mono: Boolean = false, modifier: Modifier = Modifier) {
    val tint = color ?: activePalette.secondaryText
    val style = if (mono) planMonoStyle(11.sp, FontWeight.SemiBold, tint) else planBodyStyle(11.sp, FontWeight.SemiBold, tint)
    Text(
        text, style = style, maxLines = 1, overflow = TextOverflow.Ellipsis,
        modifier = modifier
            .background(tint.copy(alpha = 0.12f), Lively.chipShape)
            .padding(horizontal = 7.dp, vertical = 3.dp),
    )
}

/** A card container for the inspector's lists. */
@Composable
internal fun PlanCard(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(
        modifier
            .fillMaxWidth()
            .clip(Lively.cardShape)
            .background(activePalette.card)
            .border(1.dp, activePalette.cardBorder, Lively.cardShape)
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) { content() }
}

/** A muted (or error) line inside a pane. */
@Composable
internal fun PlanMessage(text: String, isError: Boolean = false, modifier: Modifier = Modifier) {
    Text(
        text,
        style = planBodyStyle(13.sp, color = if (isError) activePalette.danger else activePalette.secondaryText),
        modifier = modifier.fillMaxWidth(),
    )
}

/** Collapsible pretty-printed JSON. */
@Composable
internal fun PlanJsonDisclosure(title: String, value: JsonElement, expanded: Boolean = false) {
    var open by remember(title) { mutableStateOf(expanded) }
    val motion = rememberAnimatorDurationScale() > 0f
    Column(if (motion) Modifier.animateContentSize() else Modifier) {
        Row(
            Modifier
                .fillMaxWidth()
                .heightIn(min = Lively.tapMin)
                .clickable(onClickLabel = if (open) "Collapse" else "Expand") { open = !open },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(title, style = planBodyStyle(13.sp, FontWeight.SemiBold), modifier = Modifier.weight(1f))
            Icon(
                if (open) Icons.Outlined.KeyboardArrowUp else Icons.Outlined.KeyboardArrowDown,
                contentDescription = null, tint = activePalette.accent,
            )
        }
        if (open) {
            SelectionContainer {
                Text(
                    PlanJson.pretty(value),
                    style = planMonoStyle(11.sp),
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 360.dp)
                        .background(activePalette.surface, RoundedCornerShape(8.dp))
                        .verticalScroll(rememberScrollState())
                        .horizontalScroll(rememberScrollState())
                        .padding(8.dp),
                )
            }
        }
    }
}

// Steps

/** The read-only waterfall: steps in topological order with level, tool, dependencies and parameters. */
@Composable
internal fun PlanWaterfall(
    graph: PlanGraph,
    marks: Map<String, PlanStepMark>,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
    bottomPadding: Dp = 16.dp,
) {
    val items = remember(graph) { graph.waterfall }
    LazyColumn(
        modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = bottomPadding),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        itemsIndexed(items, key = { _, item -> item.first.id }) { index, (step, level) ->
            WaterfallCard(graph, step, index + 1, level, marks[step.id]) { onSelect(step.id) }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun WaterfallCard(graph: PlanGraph, step: PlanStep, number: Int, level: Int, mark: PlanStepMark?, onClick: () -> Unit) {
    val palette = activePalette
    val running = mark == PlanStepMark.InProgress
    val border = when {
        running -> palette.running
        step.isDelegated -> palette.secondaryAccentColor.copy(alpha = 0.6f)
        else -> palette.cardBorder
    }
    val indent = (minOf(level, 4) * 10).dp
    Row(
        Modifier
            .padding(start = indent)
            .fillMaxWidth()
            .clip(Lively.cardShape)
            .background(palette.card)
            .border(if (running) 2.dp else 1.dp, border, Lively.cardShape)
            .drawBehind {
                // The level rule down the leading edge (web waterfall indent).
                if (level > 0) drawRect(palette.accent.copy(alpha = 0.6f), size = Size((1 + minOf(level, 4)).dp.toPx(), size.height))
            }
            .clickable(onClickLabel = "Open step", onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = PlanStyle.description(step, number, mark) },
    ) {
        Column(
            Modifier.weight(1f).padding(horizontal = 12.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(5.dp),
        ) {
            Row(verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "$number",
                    style = planMonoStyle(11.sp, FontWeight.Bold, palette.onAccent),
                    modifier = Modifier
                        .defaultMinSize(minWidth = 20.dp, minHeight = 20.dp)
                        .background(palette.accentFill, Lively.chipShape)
                        .padding(horizontal = 5.dp, vertical = 2.dp),
                )
                if (level > 0) PlanTag("L$level", mono = true)
                Text(PlanStyle.title(step), style = livelyStepTitleStyle().copy(fontWeight = FontWeight.SemiBold), modifier = Modifier.weight(1f))
                Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    PlanTag(PlanStyle.percent(step.confidence), PlanStyle.bandColor(step.confidence), mono = true)
                    if (mark != null) {
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                            StatusDot(PlanStyle.tone(mark), size = 7.dp)
                            Text(PlanStyle.markLabel(mark), style = livelyStepMetaStyle())
                        }
                    }
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(if (step.isDelegated) "Delegate" else "Tool", style = livelyStepMetaStyle().copy(fontWeight = FontWeight.SemiBold))
                val agent = step.providingAgentId
                when {
                    step.isDelegated && agent != null -> PlanTag("via $agent", palette.secondaryAccentColor)
                    step.hasResolvedTool -> {
                        Text(step.tool.orEmpty(), style = planMonoStyle(Lively.STEP_META_SP.sp), maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text("✓", style = planBodyStyle(10.sp, FontWeight.Bold, palette.success))
                    }
                    else -> Text("⚠ Not resolved", style = livelyStepMetaStyle(palette.warning))
                }
            }
            if (step.dependsOn.isNotEmpty()) {
                Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                    Text("Depends on", style = livelyStepMetaStyle().copy(fontWeight = FontWeight.SemiBold))
                    for (dep in step.dependsOn) {
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                            Icon(Icons.Outlined.SubdirectoryArrowRight, null, tint = palette.secondaryText, modifier = Modifier.size(11.dp))
                            Text(
                                buildString {
                                    append(PlanStyle.truncate(PlanStyle.title(graph, dep), 40))
                                },
                                style = livelyStepMetaStyle(palette.text), maxLines = 1, overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.weight(1f, fill = false),
                            )
                            graph.reason(dep, step.id)?.takeIf { it.isNotEmpty() }?.let {
                                Text("($it)", style = livelyStepMetaStyle(), maxLines = 1, overflow = TextOverflow.Ellipsis)
                            }
                        }
                    }
                }
            }
            if (step.parameters.isNotEmpty()) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("Parameters", style = livelyStepMetaStyle().copy(fontWeight = FontWeight.SemiBold))
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        for (parameter in step.parameters.take(3)) {
                            PlanTag("${parameter.key}: ${PlanStyle.truncate(PlanJson.displayText(parameter.value), 20)}", mono = true)
                        }
                        if (step.parameters.size > 3) PlanTag("+${step.parameters.size - 3} more")
                    }
                }
            }
        }
    }
}

/**
 * The edit list: hold the handle and drag to reorder, swipe left or tap ✕ to
 * delete, tap a row to edit the step, Add step at the end.
 */
@Composable
internal fun PlanEditList(
    graph: PlanGraph,
    onSelect: (String) -> Unit,
    onMove: (from: Int, toOffset: Int) -> Unit,
    onDelete: (String) -> Unit,
    onAdd: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val palette = activePalette
    val steps by rememberUpdatedState(graph.steps)
    var draggingId by remember { mutableStateOf<String?>(null) }
    var dragOffset by remember { mutableFloatStateOf(0f) }
    val heights = remember { mutableStateMapOf<String, Int>() }

    Column(modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp)) {
        Column(
            Modifier
                .fillMaxWidth()
                .clip(Lively.cardShape)
                .background(palette.card)
                .border(1.dp, palette.cardBorder, Lively.cardShape),
        ) {
            graph.steps.forEachIndexed { index, step ->
                key(step.id) {
                    val dragging = draggingId == step.id
                    val dismiss = rememberSwipeToDismissBoxState(confirmValueChange = { value ->
                        if (value == SwipeToDismissBoxValue.EndToStart) onDelete(step.id)
                        value == SwipeToDismissBoxValue.EndToStart
                    })
                    Column(
                        Modifier
                            .zIndex(if (dragging) 1f else 0f)
                            .graphicsLayer {
                                translationY = if (dragging) dragOffset else 0f
                                shadowElevation = if (dragging) 8f else 0f
                            }
                            .onSizeChanged { heights[step.id] = it.height },
                    ) {
                        if (index > 0) LivelyStepDivider(startInset = 44.dp)
                        SwipeToDismissBox(
                            state = dismiss,
                            enableDismissFromStartToEnd = false,
                            backgroundContent = {
                                Box(
                                    Modifier.fillMaxSize().background(palette.danger.copy(alpha = 0.85f)).padding(end = 20.dp),
                                    contentAlignment = Alignment.CenterEnd,
                                ) { Text("Delete", style = planBodyStyle(13.sp, FontWeight.SemiBold, chatContrastingTextColor(palette.danger))) }
                            },
                        ) {
                            Row(
                                Modifier
                                    .fillMaxWidth()
                                    .background(if (dragging) palette.elevated else palette.card)
                                    .clickable(onClickLabel = "Edit step") { onSelect(step.id) }
                                    .heightIn(min = Lively.tapMin)
                                    .padding(start = 4.dp, end = 4.dp, top = 6.dp, bottom = 6.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Box(
                                    Modifier
                                        .size(Lively.tapMin)
                                        .semantics { contentDescription = "Reorder ${PlanStyle.title(step)}. Hold and drag." }
                                        .pointerInput(step.id) {
                                            detectDragGesturesAfterLongPress(
                                                onDragStart = { draggingId = step.id; dragOffset = 0f },
                                                onDragEnd = { draggingId = null; dragOffset = 0f },
                                                onDragCancel = { draggingId = null; dragOffset = 0f },
                                                onDrag = { change, amount ->
                                                    change.consume()
                                                    dragOffset += amount.y
                                                    val current = steps.indexOfFirst { it.id == step.id }
                                                    if (current < 0) return@detectDragGesturesAfterLongPress
                                                    if (dragOffset > 0 && current < steps.lastIndex) {
                                                        val next = heights[steps[current + 1].id] ?: return@detectDragGesturesAfterLongPress
                                                        if (dragOffset > next / 2f) {
                                                            onMove(current, current + 2)
                                                            dragOffset -= next
                                                        }
                                                    } else if (dragOffset < 0 && current > 0) {
                                                        val previous = heights[steps[current - 1].id] ?: return@detectDragGesturesAfterLongPress
                                                        if (-dragOffset > previous / 2f) {
                                                            onMove(current, current - 1)
                                                            dragOffset += previous
                                                        }
                                                    }
                                                },
                                            )
                                        },
                                    contentAlignment = Alignment.Center,
                                ) { Icon(Icons.Outlined.DragHandle, null, tint = palette.secondaryText, modifier = Modifier.size(18.dp)) }
                                Text("${index + 1}", style = planMonoStyle(12.sp, FontWeight.SemiBold, palette.secondaryText), modifier = Modifier.widthIn(min = 18.dp))
                                EditRowBody(step, Modifier.weight(1f).padding(start = 6.dp))
                                IconButton(onClick = { onDelete(step.id) }) {
                                    Icon(Icons.Outlined.Close, "Delete ${PlanStyle.title(step)}", tint = palette.secondaryText, modifier = Modifier.size(18.dp))
                                }
                                Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, null, tint = palette.secondaryText, modifier = Modifier.size(18.dp))
                            }
                        }
                    }
                }
            }
        }
        Text(
            "Hold the handle and drag to reorder. Swipe left or tap ✕ to delete. Tap a step to edit it and its dependencies.",
            style = planBodyStyle(12.sp, color = palette.secondaryText),
            modifier = Modifier.padding(horizontal = 4.dp, vertical = 8.dp),
        )
        Row(
            Modifier
                .fillMaxWidth()
                .clip(Lively.cardShape)
                .background(palette.card)
                .border(1.dp, palette.cardBorder, Lively.cardShape)
                .clickable(onClickLabel = "Add step", onClick = onAdd)
                .heightIn(min = Lively.tapMin)
                .padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(Icons.Outlined.AddCircle, null, tint = palette.accent)
            Text("Add step", style = planBodyStyle(14.sp, FontWeight.SemiBold, palette.accent))
        }
        Spacer(Modifier.height(24.dp))
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun EditRowBody(step: PlanStep, modifier: Modifier) {
    val palette = activePalette
    Column(modifier, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(PlanStyle.title(step), style = livelyStepTitleStyle().copy(fontWeight = FontWeight.SemiBold))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            PlanTag(PlanStyle.percent(step.confidence), PlanStyle.bandColor(step.confidence), mono = true)
            if (step.isStart) PlanTag("▶︎ start", palette.accent) else PlanTag("after ${step.dependsOn.size}")
            if (step.hasResolvedTool) PlanTag(step.tool.orEmpty(), mono = true) else PlanTag("no tool", palette.warning)
        }
    }
}
