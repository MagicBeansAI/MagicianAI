package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.VibeDevTimelineCard
import ai.magicbeans.magdroid.vibedev.VibeDevTimelineState
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.Lightbulb
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * One turn's timeline, folded on the server into cards: the agent's messages,
 * collapsed reasoning, one line per tool step with its status, errors, and the
 * final summary.
 */
@Composable
internal fun VibeDevTurnTimeline(state: VibeDevTimelineState?, onRetry: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(start = 18.dp, top = 4.dp, bottom = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        val timeline = state?.timeline
        when {
            timeline == null && state?.error != null -> {
                MetaText(state.error.orEmpty())
                VibeButton("Retry", onRetry)
            }
            timeline == null -> LoadingLine()
            timeline.cards.isEmpty() -> MetaText("Nothing recorded for this turn yet.")
            else -> {
                if (timeline.truncated) MetaText("Earlier steps are on the desktop.")
                timeline.cards.forEach { card -> TimelineCard(card) }
            }
        }
    }
}

@Composable
private fun TimelineCard(card: VibeDevTimelineCard) {
    val fonts = LocalMagicanFontFamilies.current
    var open by rememberSaveable(card.index) { mutableStateOf(false) }
    when (card.kind) {
        "reasoning" -> Column(
            Modifier.fillMaxWidth().heightIn(min = 48.dp)
                .clickable(role = Role.Button, onClickLabel = if (open) "Hide thinking" else "Show thinking") { open = !open },
        ) {
            CardLine(Icons.Outlined.Lightbulb, activePalette.secondaryText, if (open) "Thinking" else "Thinking…", muted = true)
            if (open) card.text?.let {
                Text(it, Modifier.padding(start = 24.dp), color = activePalette.secondaryText, fontSize = 12.5.sp,
                    fontStyle = FontStyle.Italic, fontFamily = fonts.body)
            }
        }
        "action" -> {
            val (icon, tint) = when (card.status) {
                "error" -> Icons.Outlined.ErrorOutline to activePalette.danger
                "ok" -> Icons.Outlined.CheckCircle to activePalette.success
                else -> Icons.Outlined.PlayArrow to activePalette.running
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (card.status == "running") {
                    CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp, color = activePalette.running)
                } else {
                    Icon(icon, contentDescription = card.status, tint = tint, modifier = Modifier.size(16.dp))
                }
                Text(card.title, color = activePalette.text, fontSize = 12.5.sp, fontFamily = fonts.mono,
                    maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
            if (card.status == "error") card.text?.let {
                Text(it, Modifier.padding(start = 24.dp), color = activePalette.danger, fontSize = 12.sp, fontFamily = fonts.mono, maxLines = 4,
                    overflow = TextOverflow.Ellipsis)
            }
        }
        "error" -> {
            CardLine(Icons.Outlined.ErrorOutline, activePalette.danger, card.title)
            card.text?.let { Text(it, Modifier.padding(start = 24.dp), color = activePalette.danger, fontSize = 12.5.sp, fontFamily = fonts.body) }
        }
        "completed" -> {
            CardLine(Icons.Outlined.CheckCircle, activePalette.success, card.title, bold = true)
            card.text?.let { Text(it, Modifier.padding(start = 24.dp), color = activePalette.text, fontSize = 13.sp, fontFamily = fonts.body) }
        }
        "message" -> Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Icon(Icons.Outlined.ChatBubbleOutline, contentDescription = null, tint = activePalette.accent, modifier = Modifier.size(16.dp))
            Text(card.text ?: card.title, color = activePalette.text, fontSize = 13.sp, fontFamily = fonts.body,
                maxLines = if (open) Int.MAX_VALUE else 8, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.clickable { open = !open })
        }
        else -> CardLine(Icons.Outlined.Info, activePalette.secondaryText, card.text ?: card.title, muted = true)
    }
}

@Composable
private fun CardLine(icon: ImageVector, tint: androidx.compose.ui.graphics.Color, text: String, muted: Boolean = false, bold: Boolean = false) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Icon(icon, contentDescription = null, tint = tint, modifier = Modifier.size(16.dp))
        Text(text, color = if (muted) activePalette.secondaryText else activePalette.text, fontSize = 13.sp,
            fontWeight = if (bold) FontWeight.SemiBold else FontWeight.Normal,
            fontFamily = LocalMagicanFontFamilies.current.body, maxLines = 3, overflow = TextOverflow.Ellipsis)
    }
}
