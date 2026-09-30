package ai.magicbeans.magdroid.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Hub
import androidx.compose.material.icons.automirrored.outlined.MenuBook
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Sensors
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Kicker, display title, live status line, and the one Refresh action. */
@Composable
internal fun ObserveCommandHeader(
    statusLine: String,
    live: Boolean,
    refreshing: Boolean,
    onRefresh: () -> Unit,
) {
    // The top bar already says "Observe"; the header is just the live status
    // and Refresh, one compact row above the KPI grid.
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier.size(8.dp).background(if (live) Danger else Muted.copy(alpha = 0.5f), CircleShape),
            )
            Spacer(Modifier.width(8.dp))
            Text(
                statusLine, color = if (live) Ink else Secondary, fontSize = 14.sp,
                fontWeight = if (live) FontWeight.SemiBold else FontWeight.Medium,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        }
        Surface(
            color = Panel,
            shape = CircleShape,
            border = BorderStroke(1.dp, BorderSoft),
            modifier = Modifier
                .size(38.dp)
                .clip(CircleShape)
                .clickable(enabled = !refreshing, role = Role.Button, onClick = onRefresh)
                .semantics { contentDescription = if (refreshing) "Refreshing" else "Refresh Observe" },
        ) {
            Box(contentAlignment = Alignment.Center) {
                if (refreshing) {
                    CircularProgressIndicator(color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(18.dp))
                } else {
                    Icon(Icons.Outlined.Refresh, null, tint = Coral, modifier = Modifier.size(20.dp))
                }
            }
        }
    }
}

/** One KPI card's content. */
internal data class ObserveKpi(
    val pane: ObservePane,
    val label: String,
    val value: String,
    val sub: String,
    val footer: String,
    val icon: ImageVector,
    val tint: Color,
    val live: Boolean = false,
)

internal fun observeKpis(
    activeCaptures: Int,
    liveMeetings: Int,
    sourcesOn: String,
    audioValue: String,
    notesValue: String,
): List<ObserveKpi> {
    val palette = activePalette
    return listOf(
        ObserveKpi(
            ObservePane.Now, "Now & Live", activeCaptures.toString(),
            nowKpiSub(activeCaptures, liveMeetings), "Open live deck →",
            Icons.Outlined.Sensors, palette.success, live = activeCaptures > 0,
        ),
        ObserveKpi(
            ObservePane.Sources, "Sources on", sourcesOn, "Channels, tabs & feeds", "View sources →",
            Icons.Outlined.Hub, palette.info,
        ),
        ObserveKpi(
            ObservePane.Audio, "Audio Profiles", audioValue, "Meeting & listening STT", "Configure audio →",
            Icons.Outlined.GraphicEq, palette.discovery,
        ),
        ObserveKpi(
            ObservePane.Notes, "Notes & Recents", notesValue, "Observations & journals", "Browse notes →",
            Icons.AutoMirrored.Outlined.MenuBook, palette.warning,
        ),
    )
}

/** The 2×2 grid; the ONLY view switcher. */
@Composable
internal fun ObserveKpiGrid(
    kpis: List<ObserveKpi>,
    selected: ObservePane,
    onSelect: (ObservePane) -> Unit,
) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(7.dp)) {
        kpis.chunked(2).forEach { row ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                row.forEach { kpi ->
                    ObserveKpiCard(
                        kpi = kpi,
                        selected = kpi.pane == selected,
                        onClick = { onSelect(kpi.pane) },
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

@Composable
private fun ObserveKpiCard(
    kpi: ObserveKpi,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val fonts = LocalMagicanFontFamilies.current
    val shape = RoundedCornerShape(10.dp)
    val badgeTint = if (kpi.live) Danger else kpi.tint
    Surface(
        color = if (selected) Coral.copy(alpha = 0.05f).compositeOver(Panel) else Panel,
        shape = shape,
        // Selected: accent border plus a 1px ring (2dp total), per the web.
        border = BorderStroke(if (selected) 2.dp else 1.dp, if (selected) Coral else BorderSoft),
        modifier = modifier
            .clip(shape)
            .clickable(role = Role.Tab, onClick = onClick)
            .semantics {
                this.selected = selected
                contentDescription = kpiAccessibilityLabel(kpi.label, kpi.value, kpi.sub, selected)
            },
    ) {
        Column {
            Box(Modifier.fillMaxWidth().height(3.dp).background(if (selected) Coral else Color.Transparent))
            Column(
                Modifier.padding(start = 10.dp, end = 10.dp, top = 5.dp, bottom = 7.dp),
                verticalArrangement = Arrangement.spacedBy(3.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    DeckIconBadge(kpi.icon, badgeTint, size = 20, iconSize = 13)
                    Spacer(Modifier.width(6.dp))
                    Text(
                        kpi.label.uppercase(), color = Secondary, fontSize = 9.5.sp,
                        fontWeight = FontWeight.Bold, letterSpacing = 0.5.sp,
                        maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
                    )
                    if (kpi.live) DeckPill("LIVE", Danger)
                }
                Row(verticalAlignment = Alignment.Bottom) {
                    Text(
                        kpi.value, color = Ink, fontSize = 20.sp, lineHeight = 22.sp,
                        fontWeight = FontWeight.ExtraBold, fontFamily = fonts.mono, maxLines = 1,
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        kpi.sub, color = Muted, fontSize = 10.5.sp, lineHeight = 13.sp,
                        maxLines = 2, overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f).padding(bottom = 2.dp),
                    )
                }
                Text(
                    kpi.footer, color = if (selected) Coral else Muted, fontSize = 10.5.sp,
                    fontWeight = if (selected) FontWeight.Bold else FontWeight.SemiBold,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}
