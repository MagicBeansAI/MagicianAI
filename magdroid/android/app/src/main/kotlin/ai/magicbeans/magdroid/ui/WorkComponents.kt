package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.WorkResource
import ai.magicbeans.magdroid.work.WorkResourceState
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.ui.Alignment
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import kotlinx.coroutines.flow.SharedFlow
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.*

internal val LocalWorkResourceFactory = staticCompositionLocalOf<(android.content.Context) -> WorkResource> { { WorkResource(it) } }

/**
 * The Work view on screen publishes its top-bar Refresh here; every resource
 * mounted under it reloads, so one control refreshes whichever page is open.
 */
internal val LocalWorkRefreshRequests = staticCompositionLocalOf<SharedFlow<Unit>?> { null }

@Composable
internal fun rememberWorkResource(path: String): WorkResource {
    val context = LocalContext.current.applicationContext
    val factory = LocalWorkResourceFactory.current
    val resource = remember(factory) { factory(context) }
    val refreshes = LocalWorkRefreshRequests.current
    LaunchedEffect(path) { resource.load(path) }
    LaunchedEffect(resource, refreshes) { refreshes?.collect { resource.refresh() } }
    DisposableEffect(resource) { onDispose { resource.close() } }
    return resource
}

internal fun workLabel(key: String): String = key.replace('_', ' ').replaceFirstChar { it.uppercase() }
internal fun JsonElement?.workDisplay(): String = when (this) {
    null, JsonNull -> "—"
    is JsonPrimitive -> content
    else -> toString()
}
internal fun JsonElement?.workField(key: String): JsonElement? = (this as? JsonObject)?.get(key)
internal fun JsonElement?.workText(key: String): String? = (workField(key) as? JsonPrimitive)?.contentOrNull
internal fun JsonElement?.workBool(key: String): Boolean = (workField(key) as? JsonPrimitive)?.booleanOrNull == true
internal fun JsonElement?.workRows(key: String): List<JsonElement> = (workField(key) as? JsonArray)?.toList().orEmpty()

/** A padded card for free content (a document, an editor); lists use [WorkGroup]. */
@Composable
internal fun WorkCard(content: @Composable ColumnScope.() -> Unit) {
    Surface(color = activePalette.card, shape = RoundedCornerShape(WORK_CARD_RADIUS_DP.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Column(Modifier.fillMaxWidth().padding(WORK_CARD_PADDING_H_DP.dp), verticalArrangement = Arrangement.spacedBy(10.dp), content = content)
    }
}

/** Whether [WorkResourceStatus] has anything to draw (so a list skips its slot). */
internal val WorkResourceState.hasStatus: Boolean get() = (loading && value == null) || error != null || notice != null

/** A list item for a resource's status, added only when there is one to show. */
internal fun LazyListScope.workStatusItem(state: WorkResourceState, resource: WorkResource, key: String? = null) {
    // Keyed, so a notice appearing above keeps the list where the reader is.
    if (state.hasStatus) item(key = key ?: "status") { WorkResourceStatus(state, resource) }
}

/** Loading, failure and success notices for one resource, drawn the same way on every Work screen. */
@Composable
internal fun WorkResourceStatus(state: WorkResourceState, resource: WorkResource) {
    if (!state.hasStatus) return
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (state.loading && state.value == null) Row(
            Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).semantics(mergeDescendants = true) {},
            horizontalArrangement = Arrangement.Center, verticalAlignment = Alignment.CenterVertically,
        ) {
            CircularProgressIndicator(color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(20.dp))
            Spacer(Modifier.width(10.dp))
            Text("Loading…", color = Muted, fontSize = 15.sp)
        }
        state.error?.let { error -> WorkGroup {
            Row(Modifier.fillMaxWidth().padding(vertical = 12.dp), verticalAlignment = Alignment.Top) {
                Icon(Icons.Filled.Warning, null, tint = Danger, modifier = Modifier.padding(top = 2.dp).size(with(LocalDensity.current) { 16.sp.toDp() }))
                Spacer(Modifier.width(12.dp))
                Text(error, color = Ink, fontSize = 15.sp, lineHeight = 20.sp, modifier = Modifier.weight(1f).semantics { liveRegion = LiveRegionMode.Polite })
            }
            WorkRowDivider()
            WorkActionRow("Retry", Icons.Outlined.Refresh, enabled = !state.busy, onClick = resource::refresh)
        } }
        state.notice?.let { notice -> Row(Modifier.fillMaxWidth().padding(horizontal = WORK_CARD_PADDING_H_DP.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.CheckCircle, null, tint = activePalette.success, modifier = Modifier.size(with(LocalDensity.current) { 14.sp.toDp() }))
            Spacer(Modifier.width(6.dp))
            Text(notice, color = activePalette.success, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
        } }
    }
}

/** How much a nested record holds: "2 fields", "1 item", "None". */
internal fun workRecordSummary(value: JsonElement): String = when (value) {
    is JsonArray -> if (value.isEmpty()) "None" else if (value.size == 1) "1 item" else "${value.size} items"
    is JsonObject -> if (value.isEmpty()) "Empty" else if (value.size == 1) "1 field" else "${value.size} fields"
    else -> ""
}

/**
 * Operator records read as rows: a caption above each value, hairlines
 * between rows, nested records as expandable rows that say how much they
 * hold. No field is silently dropped; values stay selectable.
 */
@Composable
internal fun WorkRecord(value: JsonElement?) {
    when (value) {
        is JsonObject -> Column(Modifier.fillMaxWidth()) {
            if (value.isEmpty()) WorkRecordEmpty("No details reported.")
            value.toSortedMap().entries.forEachIndexed { index, (key, child) ->
                if (index > 0) WorkRowDivider()
                if (child is JsonObject || child is JsonArray) {
                    var expanded by remember(child) { mutableStateOf(false) }
                    WorkRow(workLabel(key), value = workRecordSummary(child), accessory = if (expanded) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { expanded = !expanded }
                    if (expanded) Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min).padding(bottom = 12.dp)) {
                        Box(Modifier.width(2.dp).fillMaxHeight().background(BorderSoft))
                        Column(Modifier.weight(1f).padding(start = 12.dp)) { WorkRecord(child) }
                    }
                } else Column(Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).padding(vertical = 9.dp), verticalArrangement = Arrangement.spacedBy(3.dp, Alignment.CenterVertically)) {
                    Text(workLabel(key), color = Muted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                    SelectionContainer { Text(child.workDisplay(), color = Ink, fontSize = 15.sp, lineHeight = 20.sp) }
                }
            }
        }
        is JsonArray -> Column(Modifier.fillMaxWidth()) {
            if (value.isEmpty()) WorkRecordEmpty("None")
            value.forEachIndexed { index, row -> if (index > 0) WorkRowDivider(); WorkRecord(row) }
        }
        else -> Box(Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).padding(vertical = 9.dp), contentAlignment = Alignment.CenterStart) {
            SelectionContainer { Text(value.workDisplay(), color = Ink, fontSize = 15.sp, lineHeight = 20.sp) }
        }
    }
}

@Composable
private fun WorkRecordEmpty(text: String) {
    Box(Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).padding(vertical = 9.dp), contentAlignment = Alignment.CenterStart) { Text(text, color = Muted, fontSize = 15.sp) }
}

/** A record as a collapsed row in a card ("Contract, decisions and retained work · 9 fields"). */
@Composable
internal fun WorkRecordRow(label: String, value: JsonElement?, icon: androidx.compose.ui.graphics.vector.ImageVector? = null, initiallyExpanded: Boolean = false) {
    var expanded by remember(value) { mutableStateOf(initiallyExpanded) }
    WorkRow(label, icon = icon, value = value?.let(::workRecordSummary)?.takeIf(String::isNotBlank), accessory = if (expanded) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { expanded = !expanded }
    if (expanded) Column(Modifier.fillMaxWidth().padding(bottom = 8.dp)) { WorkRecord(value) }
}

@Composable
internal fun WorkLink(title: String, onClick: () -> Unit) {
    TextButton(onClick, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
        Text(title, color = Coral, modifier = Modifier.fillMaxWidth())
    }
}
