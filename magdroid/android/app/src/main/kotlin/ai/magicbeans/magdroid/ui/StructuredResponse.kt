package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ActivityRow
import ai.magicbeans.magdroid.chat.StructuredBlock
import ai.magicbeans.magdroid.chat.StructuredResponse
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Tone maps to the palette rather than Material's semantic colours, so a warning
 * here looks like a warning everywhere else in the product.
 */
internal fun structuredToneColor(tone: String?): Color = when (tone?.lowercase()) {
    "success", "ok", "positive" -> ChatSuccess
    "warning", "caution" -> MWarn
    "danger", "error", "critical" -> Danger
    // iOS uses the active accent for an informational structured response.
    "info" -> Coral
    else -> Secondary
}

/**
 * Render a backend-composed answer.
 *
 * Magician builds replies as blocks — a summary, key values, a list, artifacts.
 * Falling back to the plain `text` field would hand the owner a paragraph where
 * they were given a table, so the blocks are drawn as blocks.
 *
 * An unrecognised kind falls through to its text rather than disappearing: a new
 * block type on the backend should degrade to readable, not to blank.
 */
@Composable
fun StructuredResponseView(response: StructuredResponse) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        response.title?.takeIf { it.isNotBlank() }?.let {
            Text(it, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        }
        response.summary?.takeIf { it.isNotBlank() }?.let {
            Text(
                it,
                color = Ink,
                fontSize = CHAT_BUBBLE_FONT_SP.sp,
                lineHeight = CHAT_BUBBLE_LINE_HEIGHT_SP.sp,
            )
        }
        response.blocks.forEach { Block(it) }
    }
}

@Composable
private fun Block(block: StructuredBlock) {
    when (block.kind.lowercase()) {
        "callout" -> Callout(block)
        "key_values" -> KeyValues(block)
        "list" -> BulletList(block)
        "artifacts" -> Artifacts(block)
        "metrics" -> KeyValues(block)
        "copy_text" -> CopyText(block)
        // markdown, text, and anything new: show the words rather than nothing.
        else -> block.text?.takeIf { it.isNotBlank() }?.let {
            Column {
                block.title?.takeIf { t -> t.isNotBlank() }?.let { t ->
                    Text(t, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.height(3.dp))
                }
                Text(it, color = Ink, fontSize = 14.sp, lineHeight = 20.sp)
            }
        }
    }
}

@Composable
private fun Callout(block: StructuredBlock) {
    val tone = structuredToneColor(block.tone)
    Surface(
        color = tone.copy(alpha = 0.10f),
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, tone.copy(alpha = 0.35f)),
    ) {
        Column(Modifier.padding(10.dp)) {
            block.title?.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Ink, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(2.dp))
            }
            block.text?.let { Text(it, color = Secondary, fontSize = 13.sp, lineHeight = 18.sp) }
        }
    }
}

@Composable
private fun KeyValues(block: StructuredBlock) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        block.title?.takeIf { it.isNotBlank() }?.let {
            Text(it, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        }
        block.items.forEach { item ->
            val label = item["label"] ?: item["name"] ?: item["key"]
            val value = item["value"] ?: item["text"]
            if (!label.isNullOrBlank() && !value.isNullOrBlank()) {
                Column {
                    Text(label, color = Muted, fontSize = 11.sp)
                    Text(value, color = Ink, fontSize = 14.sp, lineHeight = 19.sp)
                }
            }
        }
    }
}

@Composable
private fun BulletList(block: StructuredBlock) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        block.title?.takeIf { it.isNotBlank() }?.let {
            Text(it, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(2.dp))
        }
        block.items.forEach { item ->
            val text = item["text"] ?: item["label"] ?: item["value"]
            if (!text.isNullOrBlank()) {
                Row(verticalAlignment = Alignment.Top) {
                    Text("•", color = Coral, fontSize = 14.sp, modifier = Modifier.width(14.dp))
                    Text(text, color = Ink, fontSize = 14.sp, lineHeight = 20.sp)
                }
            }
        }
    }
}

@Composable
private fun Artifacts(block: StructuredBlock) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(
            block.title?.takeIf { it.isNotBlank() } ?: "Files",
            color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
        )
        block.items.forEach { item ->
            val name = item["name"] ?: item["label"] ?: item["path"]
            if (!name.isNullOrBlank()) {
                Surface(
                    color = Ground,
                    shape = RoundedCornerShape(8.dp),
                    border = BorderStroke(1.dp, BorderSoft),
                ) {
                    Row(Modifier.padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text("📄", fontSize = 13.sp)
                        Spacer(Modifier.width(6.dp))
                        Text(name, color = Ink, fontSize = 13.sp)
                    }
                }
            }
        }
    }
}

@Composable
private fun CopyText(block: StructuredBlock) {
    block.text?.takeIf { it.isNotBlank() }?.let { text ->
        Surface(
            color = Ground,
            shape = RoundedCornerShape(8.dp),
            border = BorderStroke(1.dp, BorderSoft),
        ) {
            Text(
                text,
                color = Ink,
                fontSize = 13.sp,
                fontFamily = LocalMagicanFontFamilies.current.mono,
                lineHeight = 19.sp,
                modifier = Modifier.padding(10.dp),
            )
        }
    }
}

/**
 * The chat turn's Steps disclosure, matching iOS.
 *
 * The steps matter when an answer is surprising and are noise the rest of the
 * time, so settled turns start collapsed. A live turn starts open, including an
 * empty `Steps 0` state, so waiting for the first activity event never looks
 * like a frozen answer.
 */
@Composable
fun ActivitySection(
    rows: List<ActivityRow>,
    onOpenAttention: () -> Unit,
    isLive: Boolean = false,
    onOpenCompleteResult: (ActivityRow) -> Unit = {},
) {
    if (!shouldShowActivitySection(rows.size, isLive)) return

    var expanded by remember { mutableStateOf(isLive) }
    var userToggledExpand by remember { mutableStateOf(false) }
    LaunchedEffect(isLive) {
        if (!userToggledExpand) expanded = isLive
    }

    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 6.dp),
        // The response bubble owns the theme's tinted surface. Steps is quiet
        // supporting context, so its container stays unfilled and relies on a
        // subtle edge instead of competing with the answer.
        color = ACTIVITY_SECTION_BACKGROUND,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Column {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(
                        onClickLabel = activitySectionToggleLabel(expanded),
                        role = Role.Button,
                    ) {
                        userToggledExpand = true
                        expanded = !expanded
                    }
                    .padding(
                        horizontal = ACTIVITY_SECTION_HORIZONTAL_PADDING_DP.dp,
                        vertical = ACTIVITY_SECTION_VERTICAL_PADDING_DP.dp,
                    ),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Icon(
                    imageVector = if (expanded) Icons.Outlined.ExpandMore else Icons.Outlined.ChevronRight,
                    contentDescription = null,
                    tint = Secondary,
                    modifier = Modifier.size(14.dp),
                )
                Text(
                    text = "Steps",
                    color = Secondary,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(text = rows.size.toString(), color = Muted, fontSize = 11.sp)
                if (isLive) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(12.dp),
                        color = Secondary,
                        strokeWidth = 1.5.dp,
                    )
                }
            }

            if (expanded) {
                Column(
                    modifier = Modifier.padding(
                        start = ACTIVITY_SECTION_HORIZONTAL_PADDING_DP.dp,
                        end = ACTIVITY_SECTION_HORIZONTAL_PADDING_DP.dp,
                        bottom = ACTIVITY_SECTION_EXPANDED_BOTTOM_PADDING_DP.dp,
                    ),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    if (rows.isEmpty() && isLive) {
                        Text(
                            text = "Waiting for the first activity update…",
                            color = Muted,
                            fontSize = 11.sp,
                        )
                    }
                    rows.forEach { row ->
                        Row(verticalAlignment = Alignment.Top) {
                            Text(
                                when (row.status?.lowercase()) {
                                    "failed", "error" -> "✕"
                                    // "waiting" is a HITL pause — unfinished, so it
                                    // must not draw as a checkmark.
                                    "running", "in_progress", "waiting" -> "•"
                                    else -> "✓"
                                },
                                color = if (row.status?.lowercase() in setOf("failed", "error")) {
                                    structuredToneColor("danger")
                                } else {
                                    Muted
                                },
                                fontSize = 11.sp,
                                modifier = Modifier.width(16.dp),
                            )
                            Column {
                                Text(row.label, color = Secondary, fontSize = 12.sp)
                                row.detail?.takeIf { it.isNotBlank() }?.let {
                                    Text(it, color = Muted, fontSize = 11.sp, lineHeight = 15.sp)
                                }
                                if (row.status == "waiting") {
                                    Text(
                                        "Open in Attention",
                                        color = Coral,
                                        fontSize = 11.sp,
                                        fontWeight = FontWeight.SemiBold,
                                        modifier = Modifier.clickable(role = Role.Button, onClick = onOpenAttention)
                                            .padding(horizontal = 8.dp, vertical = 5.dp),
                                    )
                                }
                                if (!row.resultRef.isNullOrBlank()) {
                                    Text(
                                        text = "Open complete result",
                                        color = Coral,
                                        fontSize = 11.sp,
                                        fontWeight = FontWeight.SemiBold,
                                        modifier = Modifier
                                            .padding(top = 4.dp)
                                            .clickable(
                                                role = Role.Button,
                                                onClickLabel = "Open complete result",
                                            ) { onOpenCompleteResult(row) }
                                            .padding(horizontal = 8.dp, vertical = 5.dp),
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

internal fun shouldShowActivitySection(rowCount: Int, isLive: Boolean): Boolean =
    rowCount > 0 || isLive

internal fun activitySectionToggleLabel(expanded: Boolean): String =
    if (expanded) "Hide steps" else "Show steps"

internal const val ACTIVITY_SECTION_HORIZONTAL_PADDING_DP = 10
internal const val ACTIVITY_SECTION_VERTICAL_PADDING_DP = 4
internal const val ACTIVITY_SECTION_EXPANDED_BOTTOM_PADDING_DP = 6
internal val ACTIVITY_SECTION_BACKGROUND = Color.Transparent
