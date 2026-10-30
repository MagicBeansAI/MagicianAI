package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.attention.ApprovalPreview
import ai.magicbeans.magdroid.ui.vibedev.UnifiedDiffView
import ai.magicbeans.magdroid.vibedev.DiffRow
import ai.magicbeans.magdroid.vibedev.UnifiedDiffParser
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.WarningAmber
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The tallest the diff gets inside an answer form; longer diffs scroll in place. */
private val PREVIEW_DIFF_MAX_HEIGHT = 360.dp

/**
 * A confirmation's structured approval card (`input_schema.approval_preview`),
 * the native counterpart of the web's `ApprovalPreviewCard.svelte`: who wants
 * to change what, warnings first, the agent's stated reason under its label,
 * the change as a line diff, the evidence offered and any notes. The raw call
 * stays one tap away for auditing. Everything is plain text — `why`, the
 * evidence and the diff lines are the agent's own words, never markup.
 *
 * Display only: the Approve / Reject controls below it are unchanged.
 */
@Composable
internal fun ApprovalPreviewCard(
    preview: ApprovalPreview,
    rawCall: String?,
    modifier: Modifier = Modifier.fillMaxWidth(),
) {
    Column(modifier.testTag("approval-preview"), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        if (preview.warnings.isNotEmpty()) {
            Surface(
                color = MWarn.copy(alpha = 0.12f),
                shape = RoundedCornerShape(8.dp),
                border = BorderStroke(1.dp, MWarn.copy(alpha = 0.6f)),
                modifier = Modifier.fillMaxWidth().testTag("approval-preview-warnings"),
            ) {
                Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    preview.warnings.forEach { warning ->
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.Top) {
                            Icon(Icons.Outlined.WarningAmber, contentDescription = "Warning", tint = MWarn, modifier = Modifier.size(18.dp))
                            Text(warning.message, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                        }
                    }
                }
            }
        }

        preview.why?.let { why ->
            Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                PreviewLabel(preview.whyHeading)
                SelectionContainer { Text(why, color = Ink, fontSize = 14.sp, lineHeight = 20.sp) }
            }
        }

        preview.diff?.let { diff ->
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PreviewLabel("Changes")
                Text(
                    diff.changeLabel,
                    color = Secondary,
                    fontSize = 12.sp,
                    fontFamily = LocalMagicanFontFamilies.current.mono,
                    modifier = Modifier.testTag("approval-preview-stats"),
                )
            }
            Text(diff.path, color = Secondary, fontSize = 12.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
            val unified = remember(diff) { diff.toUnifiedDiff() }
            if (!unified.isEmpty) {
                var collapsed by remember(diff) { mutableStateOf(emptySet<Int>()) }
                var unfolded by remember(diff) { mutableStateOf(emptySet<String>()) }
                Surface(
                    color = activePalette.card,
                    shape = RoundedCornerShape(8.dp),
                    border = BorderStroke(1.dp, BorderSoft),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    // A lazy list needs a bounded height inside the scrolling sheet:
                    // the diff's own height, capped, and longer diffs scroll in place.
                    UnifiedDiffView(
                        diff = unified,
                        wrap = false,
                        collapsed = collapsed,
                        unfolded = unfolded,
                        onToggleHunk = { index -> collapsed = if (index in collapsed) collapsed - index else collapsed + index },
                        onUnfold = { key -> unfolded = unfolded + key },
                        modifier = Modifier.fillMaxWidth()
                            .height(minOf(previewDiffHeight(unified, collapsed, unfolded), PREVIEW_DIFF_MAX_HEIGHT))
                            .testTag("approval-preview-diff"),
                    )
                }
            }
            diff.omittedLabel?.let { Text(it, color = Secondary, fontSize = 12.sp) }
        }

        preview.notes.forEach { note -> Text(note, color = Secondary, fontSize = 12.sp) }

        if (preview.evidence.isNotEmpty()) {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                PreviewLabel("Evidence")
                SelectionContainer {
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        preview.evidence.forEach { entry ->
                            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                Text("•", color = Secondary, fontSize = 13.sp)
                                Text(entry, color = Ink, fontSize = 13.sp, modifier = Modifier.weight(1f))
                            }
                        }
                    }
                }
            }
        }

        rawCall?.takeIf { it.isNotBlank() }?.let { raw ->
            var open by remember(raw) { mutableStateOf(false) }
            TextButton(
                onClick = { open = !open },
                contentPadding = PaddingValues(horizontal = 0.dp),
                modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)
                    .semantics { stateDescription = if (open) "Expanded" else "Collapsed" }
                    .testTag("approval-preview-raw-toggle"),
            ) {
                Text((if (open) "▾ " else "▸ ") + "Raw call", color = Coral, fontSize = 13.sp, modifier = Modifier.fillMaxWidth())
            }
            if (open) Surface(color = Panel, shape = RoundedCornerShape(8.dp), modifier = Modifier.fillMaxWidth()) {
                SelectionContainer(Modifier.heightIn(max = 220.dp).verticalScroll(rememberScrollState())) {
                    Text(
                        raw,
                        color = Ink,
                        fontSize = 12.sp,
                        lineHeight = 17.sp,
                        fontFamily = LocalMagicanFontFamilies.current.mono,
                        modifier = Modifier.padding(10.dp).testTag("approval-preview-raw"),
                    )
                }
            }
        }
    }
}

@Composable
private fun PreviewLabel(text: String) {
    Text(
        text.uppercase(),
        color = Secondary,
        fontSize = 11.sp,
        fontWeight = FontWeight.Bold,
        letterSpacing = 0.5.sp,
        modifier = Modifier.semantics { heading() },
    )
}

/**
 * The height the diff view needs for what it shows: a 48 dp header per hunk,
 * one unwrapped line per row (grown with the font scale), 48 dp per folded run.
 */
@Composable
private fun previewDiffHeight(
    diff: ai.magicbeans.magdroid.vibedev.UnifiedDiff,
    collapsed: Set<Int>,
    unfolded: Set<String>,
): Dp {
    val line = 18.dp * LocalDensity.current.fontScale
    var total = 0.dp
    diff.hunks.forEachIndexed { index, hunk ->
        total += 48.dp
        if (index in collapsed) return@forEachIndexed
        UnifiedDiffParser.rows(hunk).forEachIndexed { row, item ->
            total += when (item) {
                is DiffRow.Line -> line
                is DiffRow.Fold -> if ("$index:$row" in unfolded) line * item.lines.size else 48.dp
            }
        }
    }
    return total + 2.dp
}
