@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.MuijComponent
import ai.magicbeans.magdroid.today.MuijDocument
import ai.magicbeans.magdroid.today.MuijGraphModel
import ai.magicbeans.magdroid.today.MuijGraphNode
import ai.magicbeans.magdroid.today.MuijGraphLayout
import ai.magicbeans.magdroid.today.MuijParseResult
import ai.magicbeans.magdroid.today.compactDisplay
import ai.magicbeans.magdroid.today.presentationRows
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Warning
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.roundToInt
import kotlin.math.sin

/** Native, bounded and read-only MUIJ renderer for published surfaces. */
@Composable
internal fun MuijDocumentRenderer(raw: JsonElement, modifier: Modifier = Modifier) {
    when (val parsed = remember(raw) { MuijDocument.parse(raw) }) {
        is MuijParseResult.Valid -> Column(
            modifier.semantics { contentDescription = "Dashboard" },
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            parsed.document.layout.forEach { component ->
                key(component.id) { MuijComponentRenderer(component, 0) }
            }
        }
        is MuijParseResult.Invalid -> Surface(
            modifier.fillMaxWidth(), shape = RoundedCornerShape(11.dp), color = Control,
            border = BorderStroke(1.dp, ControlBorder),
        ) {
            Row(Modifier.padding(12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Icon(Icons.Outlined.Warning, null, tint = Danger)
                Text(parsed.reason.message, color = Secondary, fontSize = 13.sp)
            }
        }
    }
}

@Composable
internal fun MuijJsonContentRenderer(raw: JsonElement, modifier: Modifier = Modifier) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        raw.presentationRows().forEachIndexed { index, row ->
            Surface(
                Modifier.fillMaxWidth(), shape = RoundedCornerShape(9.dp), color = Control,
                border = BorderStroke(1.dp, ControlBorder),
            ) {
                Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                    if (row.key.isNotBlank()) Text(row.key, color = Secondary, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
                    Text(row.value, color = Ink, fontSize = 13.sp)
                }
            }
        }
    }
}

@Composable
private fun MuijComponentRenderer(component: MuijComponent, depth: Int) {
    if (depth >= MuijDocument.MaximumDepth) return
    when (component.type) {
        "Stack", "Container", "ScrollArea", "SplitPanel" -> MuijChildren(component, depth)
        "Grid" -> FlowRow(
            Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(9.dp),
            verticalArrangement = Arrangement.spacedBy(9.dp), maxItemsInEachRow = component.number("columns")?.toInt()?.coerceIn(1, 3) ?: 2,
        ) { component.children.forEach { child -> key(child.id) { Box(Modifier.widthIn(min = 140.dp).weight(1f)) { MuijComponentRenderer(child, depth + 1) } } } }
        "Card", "Panel" -> MuijCard(component, depth)
        "Text" -> MuijText(component)
        "Markdown" -> MuijMarkdown(component)
        "MetricCard", "Gauge" -> MuijMetric(component)
        "Table", "EntityGrid" -> MuijTable(component)
        "DataList" -> MuijDataList(component)
        "Progress", "ProgressBar" -> MuijProgress(component)
        "Badge", "Tag" -> MuijBadge(component)
        "ActivityFeed" -> MuijActivity(component)
        "Alert", "Toast", "Notification" -> MuijNotice(component)
        "EmptyState" -> MuijEmpty(component)
        "Divider" -> HorizontalDivider(color = ControlBorder)
        "CodeBlock", "DiffViewer", "TerminalTransient" -> MuijCode(component)
        "PieChart", "BarChart", "LineChart", "AreaChart", "ScatterChart", "TrendChart", "Sparkline", "Heatmap" -> MuijChart(component)
        "Tree", "TreeNode" -> MuijTree(component, depth)
        "Graph" -> MuijGraph(component)
        "Image", "Video", "Audio", "QRCode" -> MuijMedia(component)
        "Button", "TextField", "Select", "Form", "TextArea", "NumberField", "Slider", "Checkbox", "RadioGroup", "MultiSelect", "DatePicker", "Toggle", "SearchInput", "ActionBus", "ApprovalFlow", "ConfirmDialog" -> MuijReadOnlyControl(component)
        else -> MuijFallback(component, depth)
    }
}

@Composable
private fun MuijChildren(component: MuijComponent, depth: Int) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(9.dp)) {
        component.children.forEach { child -> key(child.id) { MuijComponentRenderer(child, depth + 1) } }
    }
}

@Composable
private fun MuijCard(component: MuijComponent, depth: Int) {
    Card(
        Modifier.fillMaxWidth(), shape = RoundedCornerShape(12.dp),
        colors = CardDefaults.cardColors(containerColor = Control),
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Column(Modifier.padding(13.dp), verticalArrangement = Arrangement.spacedBy(9.dp)) {
            if (component.displayLabel.isNotBlank()) Text(component.displayLabel, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            MuijChildren(component, depth)
        }
    }
}

@Composable
private fun MuijText(component: MuijComponent) {
    val title = component.string("variant") == "title"
    Text(
        component.string("children", component.string("text", component.label)),
        color = Ink, fontSize = if (title) 22.sp else 14.sp,
        fontWeight = if (title) FontWeight.Bold else FontWeight.Normal,
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun MuijMarkdown(component: MuijComponent) {
    // Markdown is intentionally kept as readable text without interpreting raw
    // HTML. This avoids turning a published artifact into an executable view.
    Text(component.string("content", component.label), color = Ink, fontSize = 14.sp, modifier = Modifier.fillMaxWidth())
}

@Composable
private fun MuijMetric(component: MuijComponent) {
    Surface(
        Modifier.fillMaxWidth().widthIn(min = 140.dp), shape = RoundedCornerShape(11.dp), color = Panel,
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(component.metricValue, color = Ink, fontSize = 23.sp, fontWeight = FontWeight.Bold)
            Text(component.displayLabel.ifBlank { "Metric" }, color = Secondary, fontSize = 11.sp, fontWeight = FontWeight.Medium)
            val trend = component.string("trend")
            val trendLabel = component.string("trendLabel")
            if (trend.isNotBlank() || trendLabel.isNotBlank()) {
                Text(
                    when (trend) { "up" -> "↑ ${trendLabel.ifBlank { "Up" }}"; "down" -> "↓ ${trendLabel.ifBlank { "Down" }}"; else -> "— ${trendLabel.ifBlank { "Flat" }}" },
                    color = if (trend == "down") Danger else Coral, fontSize = 10.sp, fontWeight = FontWeight.SemiBold,
                )
            }
        }
    }
}

@Composable
private fun MuijTable(component: MuijComponent) {
    val columns = component.tableColumns
    val rows = component.tableRows
    Surface(
        Modifier.fillMaxWidth(), shape = RoundedCornerShape(10.dp), color = Panel,
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Column(Modifier.padding(11.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            if (component.displayLabel.isNotBlank()) Text(component.displayLabel, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            if (columns.isEmpty() || rows.isEmpty()) Text("No data", color = Secondary, fontSize = 12.sp)
            else Column(Modifier.horizontalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                    columns.forEach { Text(it.label, color = Secondary, fontSize = 11.sp, fontWeight = FontWeight.Bold, modifier = Modifier.width(116.dp), maxLines = 2) }
                }
                HorizontalDivider(color = ControlBorder)
                rows.forEachIndexed { rowIndex, row ->
                    key(rowIndex) {
                        Row(horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                            columns.forEach { column -> Text(row[column.key].compactDisplay(), color = Ink, fontSize = 12.sp, modifier = Modifier.width(116.dp), maxLines = 4, overflow = TextOverflow.Ellipsis) }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun MuijDataList(component: MuijComponent) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(7.dp)) {
        component.arrayProp("items").take(200).forEachIndexed { index, raw ->
            val item = raw as? JsonObject ?: return@forEachIndexed
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.Top) {
                Text(item["key"].compactDisplay(), color = Secondary, fontSize = 11.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                Text(item["value"].compactDisplay(), color = Ink, fontSize = 12.sp, modifier = Modifier.weight(1.5f))
            }
        }
    }
}

@Composable
private fun MuijProgress(component: MuijComponent) {
    val percent = (component.number("percent") ?: 0.0).coerceIn(0.0, 100.0)
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(5.dp)) {
        if (component.displayLabel.isNotBlank()) Text(component.displayLabel, color = Ink, fontSize = 12.sp, fontWeight = FontWeight.Medium)
        LinearProgressIndicator(progress = { (percent / 100.0).toFloat() }, color = Coral, trackColor = Control, modifier = Modifier.fillMaxWidth())
        if (component.boolean("showPercent", true)) Text("${percent.toInt()}%", color = Secondary, fontSize = 10.sp)
    }
}

@Composable
private fun MuijBadge(component: MuijComponent) {
    Surface(shape = RoundedCornerShape(50), color = Coral.copy(alpha = .12f)) {
        Text(component.string("text", component.displayLabel), color = Coral, fontSize = 11.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(horizontal = 9.dp, vertical = 5.dp))
    }
}

@Composable
private fun MuijActivity(component: MuijComponent) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(9.dp)) {
        component.arrayProp("items").take(100).forEachIndexed { index, raw ->
            val item = raw as? JsonObject ?: return@forEachIndexed
            Row(horizontalArrangement = Arrangement.spacedBy(9.dp), verticalAlignment = Alignment.Top) {
                Box(Modifier.padding(top = 5.dp).width(7.dp).height(7.dp).clip(RoundedCornerShape(50)).background(Coral))
                Column {
                    Text(listOf(item["actor"], item["action"], item["target"]).map { it.compactDisplay() }.filter { it != "—" && it.isNotBlank() }.joinToString(" "), color = Ink, fontSize = 12.sp)
                    item["timestamp"]?.let { Text(it.compactDisplay(), color = Secondary, fontSize = 9.sp) }
                }
            }
        }
    }
}

@Composable
private fun MuijNotice(component: MuijComponent) {
    Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(10.dp), color = Coral.copy(alpha = .10f)) {
        Row(Modifier.padding(11.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Icon(Icons.Outlined.Info, null, tint = Coral)
            Text(component.string("message", component.string("body", component.displayLabel)), color = Ink, fontSize = 12.sp)
        }
    }
}

@Composable
private fun MuijEmpty(component: MuijComponent) {
    Column(Modifier.fillMaxWidth().padding(18.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(5.dp)) {
        Text(component.string("title", component.displayLabel.ifBlank { "No results" }), color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        component.string("description").takeIf(String::isNotBlank)?.let { Text(it, color = Secondary, fontSize = 12.sp) }
    }
}

@Composable
private fun MuijCode(component: MuijComponent) {
    Text(
        component.string("code", component.string("content", component.string("text", component.label))),
        color = Ink, fontSize = 11.sp, fontFamily = LocalMagicanFontFamilies.current.mono,
        modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).background(Panel, RoundedCornerShape(9.dp)).padding(10.dp),
    )
}

@Composable
private fun MuijChart(component: MuijComponent) {
    val data = component.chartData
    val maximum = data.maxOfOrNull { abs(it.value) }?.coerceAtLeast(.000001) ?: 1.0
    Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(10.dp), color = Panel, border = BorderStroke(1.dp, ControlBorder)) {
        Column(Modifier.padding(11.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (component.displayLabel.isNotBlank()) Text(component.displayLabel, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            if (data.isEmpty()) Text("No chart data", color = Secondary, fontSize = 12.sp)
            data.forEach { point ->
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(point.label, color = Secondary, fontSize = 10.sp, modifier = Modifier.width(82.dp), maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Box(Modifier.weight(1f).height(8.dp).clip(RoundedCornerShape(3.dp)).background(Control)) {
                        Box(Modifier.fillMaxWidth((abs(point.value) / maximum).toFloat()).height(8.dp).background(Coral.copy(alpha = .75f)))
                    }
                    Text(point.value.toString(), color = Ink, fontSize = 10.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
                }
            }
        }
    }
}

@Composable
private fun MuijTree(component: MuijComponent, depth: Int) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(5.dp)) {
        if (component.displayLabel.isNotBlank()) Text("• ${component.displayLabel}", color = Ink, fontSize = 12.sp)
        Box(Modifier.padding(start = if (component.children.isEmpty()) 0.dp else 12.dp)) { MuijChildren(component, depth) }
    }
}

/**
 * Graph family (plan 1.5): deterministic layered/radial/list layouts with
 * local-only select/expand. Reveal order renders statically on Android; the
 * renderer keeps its read-only contract (no dispatched controls).
 */
@Composable
private fun MuijGraph(component: MuijComponent) {
    val model = remember(component) { component.graphModel }
    var selectedNodeId by remember(component.id) { mutableStateOf(model.focusNodeId ?: "") }
    val selected = model.nodes.firstOrNull { it.id == selectedNodeId }
    Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(10.dp), color = Panel, border = BorderStroke(1.dp, ControlBorder)) {
        Column(Modifier.padding(11.dp), verticalArrangement = Arrangement.spacedBy(9.dp)) {
            if (component.displayLabel.isNotBlank()) Text(component.displayLabel, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            if (model.nodes.isEmpty()) Text("No items", color = Secondary, fontSize = 12.sp)
            else when (model.layout) {
                MuijGraphLayout.LAYERED -> MuijGraphTiers(model, selectedNodeId) { selectedNodeId = it }
                MuijGraphLayout.RADIAL -> MuijGraphRadial(model, selectedNodeId) { selectedNodeId = it }
                MuijGraphLayout.LIST -> MuijGraphList(model, selectedNodeId) { selectedNodeId = it }
            }
            if (selected != null) {
                MuijGraphNodeDetail(
                    selected,
                    incoming = model.edges.count { it.to == selected.id },
                    outgoing = model.edges.count { it.from == selected.id },
                )
            }
        }
    }
}

@Composable
private fun MuijGraphTiers(model: MuijGraphModel, selectedNodeId: String, onSelect: (String) -> Unit) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(9.dp)) {
        model.nodes.groupBy { it.tier }.toSortedMap().forEach { (_, tierNodes) ->
            FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                tierNodes.forEach { node -> key(node.id) { MuijGraphNodeChip(node, node.id == selectedNodeId, onSelect) } }
            }
        }
    }
}

@Composable
private fun MuijGraphRadial(model: MuijGraphModel, selectedNodeId: String, onSelect: (String) -> Unit) {
    BoxWithConstraints(Modifier.fillMaxWidth().height(320.dp), contentAlignment = Alignment.Center) {
        val density = LocalDensity.current
        val widthPx = with(density) { maxWidth.toPx() }
        val heightPx = with(density) { maxHeight.toPx() }
        val radius = with(density) { (minOf(maxWidth, maxHeight).toPx() / 2f) - 44.dp.toPx() }
        fun nodePosition(id: String): Offset? {
            val index = model.nodes.indexOfFirst { it.id == id }
            if (index < 0) return null
            val angle = 2.0 * PI * index / model.nodes.size.coerceAtLeast(1) - PI / 2
            return Offset(
                widthPx / 2f + (radius * cos(angle)).toFloat(),
                heightPx / 2f + (radius * sin(angle)).toFloat(),
            )
        }
        Canvas(Modifier.matchParentSize()) {
            for (edge in model.edges) {
                val from = nodePosition(edge.from) ?: continue
                val to = nodePosition(edge.to) ?: continue
                drawLine(color = ControlBorder, start = from, end = to, strokeWidth = 1.dp.toPx())
            }
        }
        model.nodes.forEachIndexed { index, node ->
            val angle = 2.0 * PI * index / model.nodes.size.coerceAtLeast(1) - PI / 2
            key(node.id) {
                MuijGraphNodeChip(
                    node,
                    node.id == selectedNodeId,
                    onSelect,
                    Modifier.offset {
                        IntOffset((radius * cos(angle)).roundToInt(), (radius * sin(angle)).roundToInt())
                    },
                )
            }
        }
    }
}

@Composable
private fun MuijGraphList(model: MuijGraphModel, selectedNodeId: String, onSelect: (String) -> Unit) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(5.dp)) {
        model.nodes.forEach { node ->
            val targets = model.edges.filter { it.from == node.id }
                .mapNotNull { edge -> model.nodes.firstOrNull { it.id == edge.to }?.label }
            key(node.id) {
                Row(
                    Modifier.fillMaxWidth().clip(RoundedCornerShape(7.dp))
                        .background(if (node.id == selectedNodeId) Coral.copy(alpha = .14f) else Control)
                        .border(BorderStroke(1.dp, if (node.id == selectedNodeId) Coral else ControlBorder), RoundedCornerShape(7.dp))
                        .clickable { onSelect(node.id) }
                        .padding(horizontal = 8.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(node.label, color = Ink, fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        node.kind?.let { Text(it, color = Secondary, fontSize = 9.sp, maxLines = 1) }
                    }
                    Text(
                        if (targets.isEmpty()) "—" else "→ ${targets.take(3).joinToString(", ")}",
                        color = Secondary, fontSize = 10.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

@Composable
private fun MuijGraphNodeChip(node: MuijGraphNode, selected: Boolean, onSelect: (String) -> Unit, modifier: Modifier = Modifier) {
    Column(
        modifier
            .clip(RoundedCornerShape(7.dp))
            .background(if (selected) Coral.copy(alpha = .14f) else Control)
            .border(BorderStroke(1.dp, if (selected) Coral else ControlBorder), RoundedCornerShape(7.dp))
            .clickable { onSelect(node.id) }
            .padding(horizontal = 8.dp, vertical = 4.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(node.label, color = Ink, fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        node.kind?.let { Text(it, color = Secondary, fontSize = 9.sp, maxLines = 1) }
    }
}

@Composable
private fun MuijGraphNodeDetail(node: MuijGraphNode, incoming: Int, outgoing: Int) {
    Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(9.dp), color = Control, border = BorderStroke(1.dp, ControlBorder)) {
        Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(node.label, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            node.kind?.let { Text(it, color = Secondary, fontSize = 10.sp) }
            node.metadata.forEach { entry ->
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(entry.key, color = Secondary, fontSize = 11.sp, modifier = Modifier.weight(1f))
                    Text(entry.value, color = Ink, fontSize = 11.sp, modifier = Modifier.weight(1f))
                }
            }
            Text("→ $outgoing   ← $incoming", color = Muted, fontSize = 10.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
        }
    }
}

@Composable
private fun MuijMedia(component: MuijComponent) {
    val uriHandler = LocalUriHandler.current
    val raw = component.string("src", component.string("url"))
    val safe = raw.takeIf { it.startsWith("https://") || it.startsWith("http://") }
    Surface(
        modifier = Modifier.fillMaxWidth(), shape = RoundedCornerShape(9.dp), color = Control,
        border = BorderStroke(1.dp, ControlBorder), onClick = { safe?.let(uriHandler::openUri) }, enabled = safe != null,
    ) {
        Row(Modifier.padding(10.dp), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.AutoMirrored.Outlined.OpenInNew, null, tint = if (safe == null) Muted else Coral)
            Text(component.displayLabel.ifBlank { if (safe == null) "Unavailable media" else "Open media" }, color = if (safe == null) Muted else Ink, fontSize = 12.sp)
        }
    }
}

@Composable
private fun MuijReadOnlyControl(component: MuijComponent) {
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(9.dp)).background(Control).padding(10.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(7.dp),
    ) {
        Icon(Icons.Outlined.Lock, null, tint = Muted)
        Text(component.displayLabel.ifBlank { component.type }, color = Secondary, fontSize = 11.sp, modifier = Modifier.weight(1f))
        Text("Read only", color = Muted, fontSize = 10.sp)
    }
}

@Composable
private fun MuijFallback(component: MuijComponent, depth: Int) {
    Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(9.dp), color = Control, border = BorderStroke(1.dp, ControlBorder)) {
        Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                Text(component.type, color = Muted, fontSize = 10.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
                if (component.displayLabel.isNotBlank()) Text(component.displayLabel, color = Secondary, fontSize = 12.sp)
            }
            MuijChildren(component, depth)
        }
    }
}
