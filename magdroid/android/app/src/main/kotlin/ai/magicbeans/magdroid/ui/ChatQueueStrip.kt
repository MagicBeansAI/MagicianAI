package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ChatUiState
import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material3.Text
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable
internal fun ChatQueueStrip(state: ChatUiState, action: (String, String) -> Unit, topCornerRadius: androidx.compose.ui.unit.Dp = COMPOSER_CORNER_DP.dp) {
    if (state.queueSessionId != state.activeSessionId || state.queuedMessages.isEmpty()) return
    var expanded by remember(state.activeSessionId) { mutableStateOf(false) }
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val hovered by interaction.collectIsHoveredAsState()
    val focused by interaction.collectIsFocusedAsState()
    Column {
        Row(Modifier.fillMaxWidth().height(30.dp)
            .clip(RoundedCornerShape(topStart = topCornerRadius, topEnd = topCornerRadius))
            .background(if (pressed || hovered || focused) Coral.copy(alpha = 0.12f) else Color.Transparent)
            .semantics { contentDescription = "${if (expanded) "Collapse" else "Expand"} queued messages" }
            .clickable(interactionSource = interaction, indication = null, role = Role.Button) { expanded = !expanded }
            .padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("${state.queuedMessages.size} queued · ${state.queuedMessages.first().text.orEmpty()}", color = Secondary,
                style = TextStyle(fontSize = 12.sp, lineHeight = 14.sp, platformStyle = PlatformTextStyle(includeFontPadding = false)),
                maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            Icon(Icons.Outlined.ExpandMore, contentDescription = null, tint = Secondary,
                modifier = Modifier.size(14.dp).rotate(if (expanded) 0f else 180f))
        }
        if (expanded) Column(Modifier.heightIn(max = 210.dp).verticalScroll(rememberScrollState())) {
            state.queuedMessages.forEach { message ->
                HorizontalDivider(thickness = 0.5.dp, color = Muted.copy(alpha = 0.2f))
                Column(Modifier.padding(horizontal = 12.dp, vertical = 4.dp)) {
                    Text(message.text ?: "Attachments", color = Ink, fontSize = 13.sp, maxLines = 3)
                    Row(Modifier.horizontalScroll(rememberScrollState()), verticalAlignment = Alignment.CenterVertically) {
                        Text("Waiting", color = Muted, fontSize = 11.sp)
                        TextButton(colors = ButtonDefaults.textButtonColors(contentColor = Coral, disabledContentColor = Muted.copy(alpha = 0.5f)), enabled = !state.queueMutationInFlight, onClick = { action(message.id, "stop_and_send") }) { Text("Stop & send", fontSize = 12.sp) }
                        if (!message.text.isNullOrBlank() && message.attachmentIds.isEmpty()) {
                            TextButton(colors = ButtonDefaults.textButtonColors(contentColor = Coral, disabledContentColor = Muted.copy(alpha = 0.5f)), enabled = !state.queueMutationInFlight, onClick = { action(message.id, "parallel") }) { Text("Run in parallel", fontSize = 12.sp) }
                        }
                        TextButton(colors = ButtonDefaults.textButtonColors(contentColor = Danger, disabledContentColor = Muted.copy(alpha = 0.5f)), enabled = !state.queueMutationInFlight, onClick = { action(message.id, "remove") }) { Text("Remove", fontSize = 12.sp) }
                    }
                }
            }
        }
        HorizontalDivider(thickness = 0.5.dp, color = Muted.copy(alpha = 0.2f))
    }
}
