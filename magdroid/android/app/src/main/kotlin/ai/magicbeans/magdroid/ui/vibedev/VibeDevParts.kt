package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.Lively
import ai.magicbeans.magdroid.ui.MagicanButtonShape
import ai.magicbeans.magdroid.ui.StatusDot
import ai.magicbeans.magdroid.ui.StatusTone
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.ui.magicanButtonColors
import ai.magicbeans.magdroid.vibedev.VibeDevRunState
import ai.magicbeans.magdroid.vibedev.VibeDevStatus
import ai.magicbeans.magdroid.vibedev.isoMillis
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

/** The dot for a run state: the notch dots' mapping (§6 of the plan). */
internal fun VibeDevRunState.tone(): StatusTone = when (this) {
    VibeDevRunState.Working -> StatusTone.Running
    VibeDevRunState.NeedsYou -> StatusTone.Waiting
    VibeDevRunState.Paused -> StatusTone.Partial
    VibeDevRunState.Done -> StatusTone.Done
    VibeDevRunState.Failed -> StatusTone.Failed
    VibeDevRunState.Stopped -> StatusTone.Cancelled
}

/** A crew or lane status in the same dot language. */
internal fun crewTone(status: String): StatusTone = when (status) {
    "running", "ready" -> StatusTone.Running
    "proposed", "choosing", "conflict" -> StatusTone.Waiting
    "integrated", "done" -> StatusTone.Done
    "failed" -> StatusTone.Failed
    "pending" -> StatusTone.NotStarted
    else -> StatusTone.Cancelled
}

@Composable
internal fun RunDot(state: VibeDevRunState, modifier: Modifier = Modifier) =
    StatusDot(state.tone(), modifier.semantics { contentDescription = state.label })

/** A crew or lane dot that TalkBack reads as its status. */
@Composable
internal fun LabeledDot(tone: StatusTone, label: String, modifier: Modifier = Modifier) =
    StatusDot(tone, modifier.semantics { contentDescription = label })

/** The tab's "Updated 2 min ago" clock, ticking twice a minute. */
@Composable
internal fun rememberNow(): Long {
    val now by produceState(System.currentTimeMillis()) {
        while (true) { delay(30_000); value = System.currentTimeMillis() }
    }
    return now
}

internal fun ago(fromMs: Long?, now: Long): String {
    if (fromMs == null || fromMs <= 0) return ""
    val minutes = ((now - fromMs) / 60_000).coerceAtLeast(0)
    return when {
        minutes < 1 -> "just now"
        minutes < 60 -> "${minutes}m"
        minutes < 48 * 60 -> "${minutes / 60}h"
        else -> "${minutes / (24 * 60)}d"
    }
}

/** How long a run has been going, as the mock prints it ("12 min", "2 h 5 min"); null without a start. */
internal fun elapsed(startMs: Long?, now: Long): String? {
    if (startMs == null || startMs <= 0) return null
    val minutes = ((now - startMs) / 60_000).coerceAtLeast(0)
    return when {
        minutes < 60 -> "$minutes min"
        else -> "${minutes / 60} h ${minutes % 60} min"
    }
}

internal fun agoIso(text: String?, now: Long): String = ago(isoMillis(text), now)

internal fun money(value: Double?): String = value?.let { "$" + String.format(java.util.Locale.US, "%.2f", it) } ?: "—"

/** A section heading with an optional count, in the mock's small caps style. */
@Composable
internal fun SectionLabel(text: String, count: Int? = null) {
    Row(
        Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 18.dp, bottom = 8.dp)
            .semantics { heading() },
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(text.uppercase(), color = activePalette.secondaryText, fontSize = 11.sp, fontWeight = FontWeight.Bold,
            letterSpacing = 0.8.sp, fontFamily = LocalMagicanFontFamilies.current.body)
        count?.let {
            Text("$it", color = activePalette.secondaryText, fontSize = 11.sp, fontWeight = FontWeight.Bold,
                fontFamily = LocalMagicanFontFamilies.current.body)
        }
    }
}

@Composable
internal fun VibeCard(
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    Surface(
        modifier = modifier.fillMaxWidth().padding(bottom = 10.dp),
        shape = Lively.cardShape,
        color = activePalette.card,
        border = BorderStroke(1.dp, activePalette.cardBorder),
    ) {
        Column(
            (if (onClick != null) Modifier.clickable(role = Role.Button, onClick = onClick) else Modifier)
                .padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
            content = content,
        )
    }
}

@Composable
internal fun CardTitle(text: String, maxLines: Int = 2) {
    Text(text, color = activePalette.text, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
        fontFamily = LocalMagicanFontFamilies.current.display, maxLines = maxLines, overflow = TextOverflow.Ellipsis)
}

@Composable
internal fun MetaText(text: String, modifier: Modifier = Modifier, mono: Boolean = false, maxLines: Int = 2) {
    val fonts = LocalMagicanFontFamilies.current
    Text(text, modifier = modifier, color = activePalette.secondaryText, fontSize = 12.5.sp,
        fontFamily = if (mono) fonts.mono else fonts.body, maxLines = maxLines, overflow = TextOverflow.Ellipsis)
}

/** A small rounded tag ("trusted", "API lane", "Integrator"). */
@Composable
internal fun Tag(text: String, strong: Boolean = false) {
    Surface(
        shape = Lively.chipShape,
        color = if (strong) activePalette.soft else activePalette.control,
        border = BorderStroke(1.dp, if (strong) activePalette.accent.copy(alpha = 0.45f) else activePalette.controlBorder),
    ) {
        Text(text, Modifier.padding(horizontal = 7.dp, vertical = 2.dp),
            color = if (strong) activePalette.accent else activePalette.secondaryText,
            fontSize = 11.sp, fontWeight = FontWeight.SemiBold, fontFamily = LocalMagicanFontFamilies.current.body)
    }
}

@Composable
internal fun TrustBadge(trusted: Boolean) = Tag(if (trusted) "trusted" else "sandboxed", strong = trusted)

/** Label / value pairs in the header cards. */
@Composable
internal fun KeyValues(rows: List<Pair<String, String>>) {
    val fonts = LocalMagicanFontFamilies.current
    Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
        rows.forEach { (key, value) ->
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(key, Modifier.padding(end = 4.dp), color = activePalette.secondaryText, fontSize = 12.5.sp,
                    fontFamily = fonts.body)
                Text(value, color = activePalette.text, fontSize = 12.5.sp,
                    fontFamily = if (key == "Branch" || key == "Repo") fonts.mono else fonts.body,
                    maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}

enum class VibeButtonKind { Primary, Secondary, Danger }

/** The three button roles: accent fill, outline, danger outline. */
@Composable
internal fun VibeButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    kind: VibeButtonKind = VibeButtonKind.Secondary,
    enabled: Boolean = true,
    busy: Boolean = false,
) {
    val padding = PaddingValues(horizontal = 14.dp, vertical = 8.dp)
    val label: @Composable () -> Unit = {
        if (busy) {
            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp,
                color = if (kind == VibeButtonKind.Primary) activePalette.onAccent else activePalette.accent)
        } else {
            Text(text, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, fontFamily = LocalMagicanFontFamilies.current.body,
                maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
    when (kind) {
        VibeButtonKind.Primary -> Button(
            onClick = onClick, modifier = modifier, enabled = enabled && !busy, shape = MagicanButtonShape,
            colors = magicanButtonColors(), contentPadding = padding,
        ) { label() }
        VibeButtonKind.Secondary, VibeButtonKind.Danger -> {
            val tint = if (kind == VibeButtonKind.Danger) activePalette.danger else activePalette.text
            OutlinedButton(
                onClick = onClick, modifier = modifier, enabled = enabled && !busy, shape = MagicanButtonShape,
                colors = ButtonDefaults.outlinedButtonColors(contentColor = tint),
                border = BorderStroke(1.dp, if (kind == VibeButtonKind.Danger) activePalette.danger.copy(alpha = 0.7f) else activePalette.controlBorder),
                contentPadding = padding,
            ) { label() }
        }
    }
}

/**
 * Last good data stays on screen; a failed re-read says so quietly with the
 * time of that data, and a screen with nothing yet says why with Retry.
 */
@Composable
internal fun StaleLine(status: VibeDevStatus, hasData: Boolean, onRetry: () -> Unit) {
    val now = rememberNow()
    val error = status.error ?: return
    if (hasData) {
        Row(Modifier.fillMaxWidth().padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            MetaText(
                "Updated ${ago(status.updatedAtMs, now).let { if (it == "just now") it else "$it ago" }} · offline",
                Modifier.weight(1f), maxLines = 1,
            )
            TextButton(onClick = onRetry) { Text("Retry", color = activePalette.accent, fontSize = 12.sp) }
        }
    } else {
        VibeCard {
            Text(error, color = activePalette.text, fontSize = 13.sp, fontFamily = LocalMagicanFontFamilies.current.body)
            VibeButton("Retry", onRetry, busy = status.loading)
        }
    }
}

@Composable
internal fun EmptyLine(text: String) {
    Box(Modifier.fillMaxWidth().padding(vertical = 14.dp), contentAlignment = Alignment.Center) {
        Text(text, color = activePalette.secondaryText, fontSize = 13.sp, fontFamily = LocalMagicanFontFamilies.current.body)
    }
}

@Composable
internal fun LoadingLine() {
    Box(Modifier.fillMaxWidth().padding(vertical = 18.dp), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp, color = activePalette.accent)
    }
}

/** A selectable pill (Build / Discuss, engines, attempts). */
@Composable
internal fun Pill(text: String, selected: Boolean, onClick: () -> Unit, enabled: Boolean = true) {
    Surface(
        onClick = onClick,
        enabled = enabled,
        // The chip stays compact; its touch target is still 48 dp.
        modifier = Modifier.minimumInteractiveComponentSize().semantics { this.selected = selected },
        shape = Lively.chipShape,
        color = if (selected) activePalette.accentFill else activePalette.control,
        contentColor = if (selected) activePalette.onAccent else activePalette.text,
        border = BorderStroke(1.dp, if (selected) activePalette.accentFill else activePalette.controlBorder),
    ) {
        Text(text, Modifier.padding(horizontal = 12.dp, vertical = 7.dp), fontSize = 12.5.sp,
            fontWeight = FontWeight.SemiBold, fontFamily = LocalMagicanFontFamilies.current.body, maxLines = 1)
    }
}
