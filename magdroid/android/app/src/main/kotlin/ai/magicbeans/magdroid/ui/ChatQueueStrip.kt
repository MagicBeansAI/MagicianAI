package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ChatUiState
import ai.magicbeans.magdroid.chat.ComposerRows
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
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
import androidx.compose.material3.Icon
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The queued messages, as the first row inside the composer card (web
 * `QueueInspector`, iOS `QueueSheet`): a one-line summary that expands in
 * place to each message, numbered and timed, with Stop & send, Run in
 * parallel, Copy and Remove, and Clear all / Stop turn for the whole queue.
 */
@Composable
internal fun ChatQueueStrip(
    state: ChatUiState,
    action: (String, String) -> Unit,
    topCornerRadius: androidx.compose.ui.unit.Dp = COMPOSER_CORNER_DP.dp,
    onClearAll: () -> Unit = {},
    onStopTurn: () -> Unit = {},
) {
    if (state.queueSessionId != state.activeSessionId || state.queuedMessages.isEmpty()) return
    var expanded by remember(state.activeSessionId) { mutableStateOf(false) }
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val hovered by interaction.collectIsHoveredAsState()
    val focused by interaction.collectIsFocusedAsState()
    val clipboard = LocalClipboardManager.current
    val haptics = LocalHapticFeedback.current
    val busy = state.queueMutationInFlight
    val turnRunning = state.sending || state.serverRunning
    Column {
        Row(Modifier.fillMaxWidth().height(30.dp)
            .clip(RoundedCornerShape(topStart = topCornerRadius, topEnd = topCornerRadius))
            .background(if (pressed || hovered || focused) Coral.copy(alpha = 0.12f) else Color.Transparent)
            .semantics { contentDescription = "${if (expanded) "Collapse" else "Expand"} queued messages" }
            .clickable(interactionSource = interaction, indication = null, role = Role.Button) { expanded = !expanded }
            .padding(horizontal = 14.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("${state.queuedMessages.size} queued · ${state.queuedMessages.first().text.orEmpty()}", color = Secondary,
                style = TextStyle(fontSize = 12.sp, lineHeight = 14.sp, platformStyle = PlatformTextStyle(includeFontPadding = false)),
                maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            Icon(Icons.Outlined.ExpandMore, contentDescription = null, tint = Secondary,
                modifier = Modifier.size(14.dp).rotate(if (expanded) 0f else 180f))
        }
        AnimatedVisibility(expanded, enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) {
            Column {
                ComposerHairline()
                // The whole queue: Clear all, and Stop turn while one is running.
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 10.dp),
                    horizontalArrangement = Arrangement.End,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (turnRunning) StackChip("Stop turn", onClick = onStopTurn, destructive = true)
                    StackChip("Clear all", onClick = onClearAll, enabled = !busy, destructive = true)
                }
                Column(Modifier.heightIn(max = 240.dp).verticalScroll(rememberScrollState())) {
                    val now = System.currentTimeMillis()
                    state.queuedMessages.forEachIndexed { index, message ->
                        ComposerHairline()
                        Column(Modifier.padding(start = 14.dp, end = 10.dp, top = 6.dp, bottom = 2.dp)) {
                            Text(
                                ComposerRows.queueItemLabel(index, message.queuedAtMillis, now),
                                color = Secondary, fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
                            )
                            Text(message.text?.takeIf { it.isNotBlank() } ?: "(attachments only)", color = Ink, fontSize = 13.sp, maxLines = 4, overflow = TextOverflow.Ellipsis)
                            FlowRowCompat {
                                StackChip("Stop & send", onClick = { action(message.id, "stop_and_send") }, enabled = !busy)
                                if (!message.text.isNullOrBlank() && message.attachmentIds.isEmpty()) {
                                    StackChip("Run in parallel", onClick = { action(message.id, "parallel") }, enabled = !busy)
                                }
                                if (!message.text.isNullOrBlank()) {
                                    StackChip("Copy", icon = Icons.Outlined.ContentCopy, onClick = {
                                        clipboard.setText(AnnotatedString(message.text.orEmpty()))
                                        haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                                    })
                                }
                                StackChip("Remove", onClick = { action(message.id, "remove") }, enabled = !busy, destructive = true)
                            }
                        }
                    }
                }
            }
        }
        ComposerHairline()
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun FlowRowCompat(content: @Composable () -> Unit) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) { content() }
}
