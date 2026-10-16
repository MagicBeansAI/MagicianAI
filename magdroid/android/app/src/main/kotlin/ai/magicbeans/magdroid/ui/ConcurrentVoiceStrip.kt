package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ChatViewModel
import ai.magicbeans.magdroid.voice.VoiceRequest
import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.layout.positionInParent
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/** Same disclosure contract as the web composer: latest line, options, all requests on expansion. */
@Composable
internal fun ConcurrentVoiceStrip(viewModel: ChatViewModel, topCornerRadius: Dp = COMPOSER_CORNER_DP.dp) {
    val coordinator = viewModel.concurrentVoice
    val state by coordinator.state.collectAsStateWithLifecycle()
    val result by viewModel.voiceResult.collectAsStateWithLifecycle()
    var expanded by remember { mutableStateOf(false) }
    val headerInteraction = remember { MutableInteractionSource() }
    val pressed by headerInteraction.collectIsPressedAsState()
    val hovered by headerInteraction.collectIsHoveredAsState()
    val focused by headerInteraction.collectIsFocusedAsState()
    val summaryTextStyle = TextStyle(fontSize = 12.sp, lineHeight = 14.sp,
        platformStyle = PlatformTextStyle(includeFontPadding = false))
    val available = state.available
    val latest = available.firstOrNull()
    if (latest != null) {
        Column(Modifier.fillMaxWidth()) {
            Row(
                Modifier.fillMaxWidth()
                    .clip(RoundedCornerShape(topStart = topCornerRadius, topEnd = topCornerRadius))
                    .background(if (pressed || hovered || focused) Coral.copy(alpha = 0.12f) else Color.Transparent),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Row(
                    Modifier.weight(1f).heightIn(min = 30.dp)
                        .semantics { contentDescription = "${if (expanded) "Collapse" else "Expand"} background requests (${available.size})" }
                        .clickable(interactionSource = headerInteraction, indication = null, role = Role.Button) { expanded = !expanded }
                        .padding(start = 14.dp, end = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(7.dp),
                ) {
                    Icon(Icons.Outlined.ExpandMore, contentDescription = null, tint = Secondary,
                        modifier = Modifier.size(14.dp).rotate(if (expanded) 0f else 180f))
                    Text(if (state.speaking == latest.id) "Speaking" else latest.label, style = summaryTextStyle, maxLines = 1, fontWeight = FontWeight.SemiBold, color = Ink)
                    Text(latest.title, modifier = Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis, style = summaryTextStyle, color = Secondary)
                    Text("${available.size}", style = summaryTextStyle, maxLines = 1, color = Secondary)
                }
                VoiceRequestOptions(latest, viewModel, headerInteraction)
            }
            if (expanded) {
                val scroll = rememberScrollState()
                val density = LocalDensity.current
                // Measured in the list's content space: its full height and each
                // row's bottom edge, so one- and two-line rows each count as a row.
                var contentHeight by remember { mutableFloatStateOf(0f) }
                val rowBottoms = remember { mutableStateMapOf<Int, Float>() }
                var listBottom by remember { mutableStateOf<Float?>(null) }
                val bottoms = (available.indices).mapNotNull { rowBottoms[it] }
                val roomTop = LocalComposerRoomTop.current ?: 0f
                val listHeight = ConcurrentVoiceListSizing.height(
                    contentHeight = contentHeight,
                    rowBottoms = if (bottoms.size == available.size) bottoms else emptyList(),
                    available = listBottom?.let { it - roomTop },
                    bottomPadding = with(density) { LIST_BOTTOM_PADDING_DP.dp.toPx() },
                    peek = with(density) { ConcurrentVoiceListSizing.PEEK_DP.dp.toPx() },
                    minTranscript = with(density) { ConcurrentVoiceListSizing.MIN_TRANSCRIPT_DP.dp.toPx() },
                )
                val thumbColor = Secondary.copy(alpha = 0.55f)
                Column(
                    // A fixed height once measured: the list is its content up to
                    // four rows, not whatever the composer has left over.
                    (if (contentHeight > 0f) Modifier.height(with(density) { listHeight.toDp() }) else Modifier.heightIn(max = 220.dp))
                        // Its bottom edge stays put as it grows (the card is
                        // bottom-anchored), so it measures the room above it.
                        .onGloballyPositioned { listBottom = it.boundsInWindow().bottom }
                        .graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
                        .drawWithContent {
                            drawContent()
                            if (scroll.maxValue > 0) {
                                val fade = 18.dp.toPx()
                                if (scroll.canScrollForward) drawRect(
                                    Brush.verticalGradient(listOf(Color.Black, Color.Transparent), startY = size.height - fade, endY = size.height),
                                    topLeft = Offset(0f, size.height - fade), size = Size(size.width, fade), blendMode = BlendMode.DstIn)
                                val viewport = size.height
                                val thumb = (viewport * viewport / (viewport + scroll.maxValue)).coerceAtLeast(18.dp.toPx())
                                val y = (viewport - thumb) * scroll.value / scroll.maxValue
                                drawRoundRect(thumbColor, topLeft = Offset(size.width - 5.dp.toPx(), y),
                                    size = Size(3.dp.toPx(), thumb), cornerRadius = CornerRadius(1.5.dp.toPx()))
                            }
                        }
                        .verticalScroll(scroll)
                        .onSizeChanged { contentHeight = it.height.toFloat() }
                        .padding(start = 12.dp, end = 12.dp, bottom = LIST_BOTTOM_PADDING_DP.dp),
                ) {
                    state.focus?.let { row ->
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("Next voice topic: ${row.title}", Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis, fontSize = 11.sp, color = Secondary)
                            TextButton(onClick = { coordinator.select(null) }) { Text("New topic", fontSize = 11.sp) }
                        }
                    }
                    available.forEachIndexed { index, row ->
                        if (index > 0) HorizontalDivider(thickness = 0.5.dp, color = Muted.copy(alpha = 0.2f))
                        Row(
                            Modifier.fillMaxWidth()
                                .onGloballyPositioned { rowBottoms[index] = it.positionInParent().y + it.size.height }
                                .padding(vertical = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f).clickable { coordinator.select(row) }) {
                                Text(if (state.speaking == row.id) "Speaking" else row.label, fontSize = 11.sp, fontWeight = FontWeight.SemiBold, color = Secondary)
                                Text(row.title, maxLines = 2, overflow = TextOverflow.Ellipsis, fontSize = 12.sp, color = Ink)
                                row.error?.let { Text(it, fontSize = 11.sp, color = Danger) }
                            }
                            VoiceRequestOptions(row, viewModel)
                        }
                    }
                }
            }
            state.error?.let { Text(it, fontSize = 11.sp, color = Danger, modifier = Modifier.padding(horizontal = 14.dp, vertical = 4.dp)) }
            HorizontalDivider(thickness = 0.5.dp, color = Muted.copy(alpha = 0.2f))
        }
    } else state.error?.let { Text(it, fontSize = 11.sp, color = Danger, modifier = Modifier.padding(horizontal = 14.dp, vertical = 4.dp)) }
    result?.let { (id, text) ->
        AlertDialog(
            onDismissRequest = viewModel::closeVoiceResult,
            title = { Text(state.requests.find { it.id == id }?.title ?: "Voice result", maxLines = 2) },
            text = { SelectionContainer { Text(text, Modifier.heightIn(max = 420.dp).verticalScroll(rememberScrollState())) } },
            confirmButton = { TextButton(onClick = viewModel::closeVoiceResult) { Text("Done") } },
            dismissButton = { state.requests.find { it.id == id }?.let { row -> TextButton(onClick = {
                viewModel.closeVoiceResult(); viewModel.openSession(row.branchSessionId)
            }) { Text("Review work") } } },
        )
    }
}

private const val LIST_BOTTOM_PADDING_DP = 4f

/**
 * Window y of the top of the room the composer shares with the transcript (the
 * transcript's top edge), so the request list measures the chat's room rather
 * than the whole window down to its bottom edge, status and app bars included.
 * Null where no chat provides it; the list then measures from the window top.
 */
internal val LocalComposerRoomTop = compositionLocalOf<Float?> { null }

/**
 * How tall the expanded request list is: its content, up to four rows plus a
 * peek of the fifth (so it reads as scrollable). Those four rows show whenever
 * the room above the list (the transcript plus the list) can give them and
 * still keep [MIN_TRANSCRIPT_DP] of transcript; when it cannot (keyboard up,
 * a short screen) the list takes [AVAILABLE_FRACTION] of the room instead,
 * and never less than one row. Pixels in, pixels out; rows are measured, so
 * their line count does not matter.
 *
 * It was a flat 40% of the window above the list's bottom edge: on a 720×1600
 * phone (TECNO KL8) that is ~231 dp, less than four two-line rows plus the
 * peek (~243 dp), so it opened to three and a half, fewer with a selected
 * topic's row above them, and two to three with the keyboard up.
 */
internal object ConcurrentVoiceListSizing {
    const val VISIBLE_ROWS = 4
    const val PEEK_DP = 18f
    const val AVAILABLE_FRACTION = 0.55f
    const val MIN_TRANSCRIPT_DP = 120f

    fun height(
        contentHeight: Float,
        rowBottoms: List<Float>,
        available: Float?,
        bottomPadding: Float,
        peek: Float,
        minTranscript: Float = 0f,
    ): Float {
        if (contentHeight <= 0f) return 0f
        var want = contentHeight
        if (rowBottoms.size > VISIBLE_ROWS) want = minOf(want, rowBottoms[VISIBLE_ROWS - 1] + peek)
        if (available == null || available <= 0f) return want
        val floor = rowBottoms.firstOrNull()?.let { it + bottomPadding } ?: 0f
        if (want <= available - minTranscript) return want
        return minOf(want, maxOf(available * AVAILABLE_FRACTION, floor))
    }
}

@Composable
private fun VoiceRequestOptions(row: VoiceRequest, viewModel: ChatViewModel, headerInteraction: MutableInteractionSource? = null) {
    var open by remember(row.id) { mutableStateOf(false) }
    val coordinator = viewModel.concurrentVoice
    Box {
        if (headerInteraction != null) {
            Box(Modifier.width(44.dp).height(30.dp)
                .semantics { contentDescription = "Options for ${row.title}" }
                .clickable(interactionSource = headerInteraction, indication = null, role = Role.Button) { open = true },
                contentAlignment = Alignment.Center) {
                Icon(Icons.Outlined.MoreHoriz, contentDescription = null, tint = Secondary, modifier = Modifier.size(16.dp))
            }
        } else {
            IconButton(onClick = { open = true }, modifier = Modifier.width(32.dp).height(24.dp).semantics { contentDescription = "Options for ${row.title}" }) {
                Icon(Icons.Outlined.MoreHoriz, contentDescription = null, tint = Secondary, modifier = Modifier.size(16.dp))
            }
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(text = { Text("Continue this topic") }, onClick = { open = false; coordinator.select(row) })
            if (row.running) DropdownMenuItem(text = { Text("Cancel request") }, onClick = { open = false; coordinator.action { coordinator.cancel(row.id) } })
            else if (row.speechText != null) DropdownMenuItem(text = { Text("Read aloud") }, onClick = { open = false; coordinator.replay(row.id) })
            if (row.resultMessageId != null) DropdownMenuItem(text = { Text("View result") }, onClick = { open = false; viewModel.showVoiceResult(row.id) })
            DropdownMenuItem(text = { Text("Review work") }, onClick = { open = false; viewModel.openSession(row.branchSessionId) })
            if (row.workStatus !in listOf("accepted", "running")) DropdownMenuItem(text = { Text("Dismiss") }, onClick = { open = false; coordinator.action { coordinator.dismiss(row.id) } })
            DropdownMenuItem(text = { Text("New topic") }, onClick = { open = false; coordinator.select(null) })
        }
    }
}

/** Parent navigation is a composer header, with the same hit area and boundary as work updates. */
@Composable
internal fun ConcurrentOriginStrip(topCornerRadius: Dp = COMPOSER_CORNER_DP.dp, onOpenParent: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val hovered by interaction.collectIsHoveredAsState()
    val focused by interaction.collectIsFocusedAsState()
    Column(Modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().height(30.dp)
                .clip(RoundedCornerShape(topStart = topCornerRadius, topEnd = topCornerRadius))
                .background(if (pressed || hovered || focused) Coral.copy(alpha = 0.12f) else Color.Transparent)
                .clickable(interactionSource = interaction, indication = null, role = Role.Button, onClick = onOpenParent)
                .padding(horizontal = 14.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Concurrent · Started from original conversation ↗", color = Secondary,
                style = TextStyle(fontSize = 12.sp, lineHeight = 14.sp, platformStyle = PlatformTextStyle(includeFontPadding = false)),
                maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        HorizontalDivider(thickness = 0.5.dp, color = Muted.copy(alpha = 0.2f))
    }
}
