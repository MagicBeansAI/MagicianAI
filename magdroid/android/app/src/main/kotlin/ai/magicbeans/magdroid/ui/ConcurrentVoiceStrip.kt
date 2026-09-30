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
                Column(Modifier.heightIn(max = 220.dp).verticalScroll(rememberScrollState()).padding(horizontal = 12.dp)) {
                    state.focus?.let { row ->
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("Next voice topic: ${row.title}", Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis, fontSize = 11.sp, color = Secondary)
                            TextButton(onClick = { coordinator.select(null) }) { Text("New topic", fontSize = 11.sp) }
                        }
                    }
                    available.forEachIndexed { index, row ->
                        if (index > 0) HorizontalDivider(thickness = 0.5.dp, color = Muted.copy(alpha = 0.2f))
                        Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                            Column(Modifier.weight(1f).clickable { coordinator.select(row) }) {
                                Text(if (state.speaking == row.id) "Speaking" else row.label, fontSize = 11.sp, fontWeight = FontWeight.SemiBold, color = Secondary)
                                Text(row.title, maxLines = 2, overflow = TextOverflow.Ellipsis, fontSize = 12.sp, color = Ink)
                                row.error?.let { Text(it, fontSize = 11.sp, color = Coral) }
                            }
                            VoiceRequestOptions(row, viewModel)
                        }
                    }
                }
            }
            state.error?.let { Text(it, fontSize = 11.sp, color = Coral) }
            HorizontalDivider(color = Muted.copy(alpha = 0.2f))
        }
    } else state.error?.let { Text(it, fontSize = 11.sp, color = Coral) }
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
