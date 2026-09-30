@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.TODAY_WIRE_TICKER_MS
import ai.magicbeans.magdroid.today.TodayActivityFilter
import ai.magicbeans.magdroid.today.TodayActivityItem
import ai.magicbeans.magdroid.today.TodayUiState
import ai.magicbeans.magdroid.today.TodayWireFilter
import ai.magicbeans.magdroid.today.TodayWireItem
import ai.magicbeans.magdroid.today.TodayWireKind
import ai.magicbeans.magdroid.today.TodayWireSeverity
import ai.magicbeans.magdroid.today.filterWireItems
import ai.magicbeans.magdroid.today.formatCompact24h
import ai.magicbeans.magdroid.today.todayRelativeTime
import ai.magicbeans.magdroid.today.wireFilterCount
import ai.magicbeans.magdroid.today.wireTimeAgo
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccessTime
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.History
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

internal fun wireKindColor(kind: TodayWireKind): Color = when (kind) {
    TodayWireKind.Event -> TodayInfo
    TodayWireKind.Insight -> TodayDiscovery
    TodayWireKind.Activity -> Coral
}

/**
 * The live wire: a one-line ticker that cycles every 4.5s while collapsed and
 * a drawer with the latest five lines plus the way into Activity.
 */
@Composable
internal fun TodayRealtimeWire(
    state: TodayUiState,
    onOpenItem: (TodayWireItem) -> Unit,
    onOpenActivity: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var expanded by rememberSaveable { mutableStateOf(false) }
    var filter by rememberSaveable { mutableStateOf(TodayWireFilter.All) }
    var tickerIndex by rememberSaveable { mutableIntStateOf(0) }
    var now by androidx.compose.runtime.remember { mutableLongStateOf(System.currentTimeMillis()) }
    val visible = filterWireItems(state.wireItems, filter)
    val newestId = state.wireItems.firstOrNull()?.id

    // A new dispatch snaps the ticker to the newest line.
    LaunchedEffect(newestId) { if (!expanded) tickerIndex = 0 }
    LaunchedEffect(expanded, visible.size) {
        while (true) {
            delay(TODAY_WIRE_TICKER_MS)
            now = System.currentTimeMillis()
            if (!expanded && visible.isNotEmpty()) tickerIndex = (tickerIndex + 1) % visible.size
        }
    }
    val current = visible.takeIf { it.isNotEmpty() }?.let { it[tickerIndex % it.size] }

    Column(
        modifier.fillMaxWidth()
            .background(Panel, RoundedCornerShape(10.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(10.dp)),
    ) {
        Row(
            Modifier.fillMaxWidth().clickable { expanded = !expanded }.padding(horizontal = 10.dp, vertical = 9.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            LiveDot()
            Box(Modifier.weight(1f)) {
                Crossfade(targetState = current, label = "wire-ticker") { item ->
                    if (item == null) {
                        Text("Awaiting incoming transmissions…", color = Muted, fontSize = 12.sp,
                            fontStyle = androidx.compose.ui.text.font.FontStyle.Italic, maxLines = 1)
                    } else {
                        WireTickerLine(item, now)
                    }
                }
            }
            Text(
                formatCompact24h(state.eventCount24h),
                modifier = Modifier
                    .background(Soft, RoundedCornerShape(5.dp))
                    .padding(horizontal = 6.dp, vertical = 2.dp)
                    .semantics { contentDescription = "${state.eventCount24h} events in the last 24 hours" },
                color = Secondary, fontFamily = newsMono(), fontSize = 10.sp, fontWeight = FontWeight.Bold,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(if (expanded) "Hide" else "Latest 5", color = Coral, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
                Icon(if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, null, tint = Coral, modifier = Modifier.size(16.dp))
            }
        }
        AnimatedVisibility(expanded) {
            Column(Modifier.fillMaxWidth().padding(start = 10.dp, end = 10.dp, bottom = 10.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
                Hairline()
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    TodayWireFilter.entries.forEach { option ->
                        FilterChip(
                            selected = filter == option,
                            onClick = { filter = option; tickerIndex = 0 },
                            label = { Text("${option.label} ${wireFilterCount(state.wireItems, option)}", fontSize = 11.sp) },
                        )
                    }
                }
                if (visible.isEmpty()) {
                    Text("Awaiting incoming transmissions…", color = Muted, fontSize = 12.sp, modifier = Modifier.padding(vertical = 6.dp))
                }
                visible.forEach { item -> WireCard(item, now) { onOpenItem(item) } }
                OutlinedButton(onClick = onOpenActivity, shape = MagicanButtonShape, modifier = Modifier.fillMaxWidth()) {
                    Icon(Icons.Outlined.History, null, Modifier.size(16.dp)); Text("  Activity")
                }
            }
        }
    }
}

@Composable
private fun LiveDot() {
    val transition = rememberInfiniteTransition(label = "live-dot")
    val pulse by transition.animateFloat(
        initialValue = 1f, targetValue = .35f,
        animationSpec = infiniteRepeatable(tween(900), RepeatMode.Reverse), label = "live-dot-alpha",
    )
    Box(
        Modifier.size(8.dp).alpha(pulse).background(TodaySuccess, CircleShape)
            .semantics { contentDescription = "Live stream active" },
    )
}

@Composable
private fun WireKindTag(kind: TodayWireKind) {
    val color = wireKindColor(kind)
    Text(
        kind.label,
        modifier = Modifier.background(color.copy(alpha = .12f), RoundedCornerShape(4.dp)).padding(horizontal = 5.dp, vertical = 1.dp),
        color = color, fontFamily = newsMono(), fontSize = 9.sp, fontWeight = FontWeight.Bold, maxLines = 1,
    )
}

@Composable
private fun WireTickerLine(item: TodayWireItem, now: Long) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        WireKindTag(item.kind)
        Text(
            buildAnnotatedString {
                withStyle(SpanStyle(fontWeight = FontWeight.Bold, color = Ink)) { append(item.title) }
                if (item.summary.isNotBlank()) {
                    withStyle(SpanStyle(color = Muted)) { append("  ·  ") }
                    withStyle(SpanStyle(color = Secondary)) { append(item.summary) }
                }
                withStyle(SpanStyle(color = Muted)) { append("  (${wireTimeAgo(item.timestamp, now)})") }
            },
            fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun WireCard(item: TodayWireItem, now: Long, onClick: () -> Unit) {
    val navigable = item.taskId != null || item.threadId != null
    Column(
        Modifier.fillMaxWidth()
            .background(Ground, RoundedCornerShape(8.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(8.dp))
            .clickable(enabled = navigable, onClick = onClick)
            .padding(9.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            WireKindTag(item.kind)
            if (item.badge.isNotBlank()) Text(item.badge, color = Muted, fontSize = 10.sp, maxLines = 1, modifier = Modifier.weight(1f, fill = false))
            when (item.severity) {
                TodayWireSeverity.Error -> Text("error", color = Danger, fontSize = 10.sp, fontWeight = FontWeight.Bold)
                TodayWireSeverity.Success -> Text("completed", color = TodaySuccess, fontSize = 10.sp, fontWeight = FontWeight.Bold)
                else -> Unit
            }
            Spacer(Modifier.weight(1f))
            Icon(Icons.Outlined.AccessTime, null, tint = Muted, modifier = Modifier.size(11.dp))
            Text(wireTimeAgo(item.timestamp, now), color = Muted, fontSize = 10.sp)
        }
        Text(item.title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, maxLines = 2, overflow = TextOverflow.Ellipsis)
        if (item.summary.isNotBlank()) Text(item.summary, color = Secondary, fontSize = 11.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
    }
}

/** The former Activity section: search, filters, remove and clear, as a sheet. */
@Composable
internal fun TodayActivitySheet(
    state: TodayUiState,
    onQuery: (String) -> Unit,
    onFilter: (TodayActivityFilter) -> Unit,
    onOpen: (TodayActivityItem) -> Unit,
    onRemove: (TodayActivityItem) -> Unit,
    onClear: () -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = Ground) {
        Column(
            Modifier.fillMaxWidth().heightIn(max = 640.dp).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text("Activity", style = newsSerif(22.sp, FontWeight.Bold))
            state.sectionErrors["activity"]?.let { TodayInlineError(it) }
            MagicianTextField(
                value = state.activityQuery, onValueChange = onQuery, modifier = Modifier.fillMaxWidth(), singleLine = true,
                leadingIcon = { Icon(Icons.Outlined.Search, null) }, placeholder = { Text("Search activity") },
            )
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                TodayActivityFilter.entries.forEach { filter ->
                    FilterChip(selected = state.activityFilter == filter, onClick = { onFilter(filter) }, label = { Text(filter.title) })
                }
            }
            val rows = state.filteredActivity()
            if (rows.isEmpty()) Text("No activity matches.", color = Muted, fontSize = 12.sp, modifier = Modifier.padding(vertical = 8.dp))
            rows.forEach { item -> ActivityRow(item, onOpen = { onOpen(item) }, onRemove = { onRemove(item) }) }
            if (state.activityItems.isNotEmpty()) {
                TextButton(onClick = onClear, modifier = Modifier.align(Alignment.End)) {
                    Icon(Icons.Outlined.Delete, null, Modifier.size(16.dp), tint = Danger); Text(" Clear activity", color = Danger)
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun ActivityRow(item: TodayActivityItem, onOpen: () -> Unit, onRemove: () -> Unit) {
    Card(colors = CardDefaults.cardColors(containerColor = Panel), border = BorderStroke(1.dp, BorderSoft)) {
        Row(Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(11.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(todaySourceIcon(item.itemType), null, tint = todayStatusColor(item.status), modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(9.dp))
            Column(Modifier.weight(1f)) {
                Text(item.title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                item.summary?.let { Text(it, color = Muted, fontSize = 11.sp, maxLines = 2) }
                Text(todayRelativeTime(item.updatedAt), color = Muted, fontSize = 9.sp)
            }
            IconButton(onClick = onRemove, modifier = Modifier.size(32.dp)) { Icon(Icons.Outlined.Close, "Remove", tint = Muted, modifier = Modifier.size(16.dp)) }
        }
    }
}
