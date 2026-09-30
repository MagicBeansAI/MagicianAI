package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.MonitorDetailBundle
import ai.magicbeans.magdroid.tasks.MonitorDraft
import ai.magicbeans.magdroid.tasks.MonitorFinding
import ai.magicbeans.magdroid.tasks.MonitorListItem
import ai.magicbeans.magdroid.tasks.MonitorRun
import ai.magicbeans.magdroid.tasks.MonitorUpdate
import ai.magicbeans.magdroid.tasks.TaskLane
import ai.magicbeans.magdroid.tasks.TaskLoadState
import ai.magicbeans.magdroid.tasks.TaskV3
import ai.magicbeans.magdroid.tasks.TasksUiState
import ai.magicbeans.magdroid.tasks.TasksViewModel
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.ThumbDown
import androidx.compose.material.icons.outlined.ThumbUp
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Divider
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.ZoneId

@Composable
internal fun MonitorLane(state: TasksUiState, viewModel: TasksViewModel, onCreate: () -> Unit) {
    Column(Modifier.fillMaxSize().background(Ground)) {
        LazyRow(
            modifier = Modifier.fillMaxWidth().background(Panel),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 12.dp, vertical = 7.dp),
            horizontalArrangement = Arrangement.spacedBy(7.dp),
        ) {
            items(listOf(null, "active", "paused")) { value ->
                MagicianFilterChip(
                    text = value?.replaceFirstChar(Char::uppercase) ?: "All",
                    selected = state.monitorStateFilter == value,
                    onClick = { viewModel.setMonitorState(value) },
                )
            }
        }
        when {
            state.monitors.isEmpty() && state.monitorLoadState in setOf(TaskLoadState.Idle, TaskLoadState.Loading) ->
                MonitorPlaceholder("Loading monitors…", loading = true)
            state.monitors.isEmpty() && state.monitorLoadState == TaskLoadState.Failed ->
                MonitorPlaceholder(
                    state.activeLoadError?.title ?: "Monitors unavailable",
                    detail = state.activeLoadError?.message ?: "Check the connection and try again.",
                    action = "Try again",
                    onAction = viewModel::refresh,
                )
            state.monitors.isEmpty() ->
                MonitorPlaceholder("No monitors in this view", action = "New monitor", onAction = onCreate)
            else -> LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                state.activeLoadError?.let { error ->
                    item(key = "monitor-load-error") {
                        TaskLoadErrorBanner(error, viewModel::refresh)
                    }
                }
                items(state.monitors, key = MonitorListItem::taskId) { item ->
                    MonitorCard(item) { viewModel.openMonitor(item.taskId) }
                }
                if (state.monitorNextCursor != null) item {
                    OutlinedButton(shape = MagicanButtonShape, onClick = viewModel::loadMore, modifier = Modifier.fillMaxWidth(), enabled = !state.monitorLoadingMore) {
                        if (state.monitorLoadingMore) {
                            CircularProgressIndicator(Modifier.size(15.dp), strokeWidth = 2.dp)
                            Spacer(Modifier.width(7.dp))
                        }
                        Text(state.monitorTotal?.let { "Load more · ${state.monitors.size} of $it" } ?: "Load more")
                    }
                }
            }
        }
    }
}

@Composable
private fun MonitorPlaceholder(
    message: String,
    detail: String? = null,
    loading: Boolean = false,
    action: String? = null,
    onAction: () -> Unit = {},
) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(9.dp)) {
            if (loading) CircularProgressIndicator(color = Coral)
            else Icon(Icons.Outlined.Notifications, null, tint = Muted, modifier = Modifier.size(34.dp))
            Text(message, color = Ink, fontWeight = FontWeight.SemiBold)
            detail?.let { Text(it, color = Muted, fontSize = 12.sp, modifier = Modifier.padding(horizontal = 24.dp)) }
            action?.let { OutlinedButton(shape = MagicanButtonShape, onClick = onAction) { Text(it) } }
        }
    }
}

@Composable
private fun MonitorCard(item: MonitorListItem, onOpen: () -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onOpen),
        colors = CardDefaults.cardColors(containerColor = Panel),
        border = BorderStroke(1.dp, BorderSoft),
        shape = RoundedCornerShape(14.dp),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Row(verticalAlignment = Alignment.Top) {
                Text(
                    item.title.ifBlank { "Untitled monitor" }, color = Ink, fontSize = 16.sp,
                    fontWeight = FontWeight.SemiBold, maxLines = 2, overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                MonitorPill(if (item.state == "active") "Active" else "Paused", if (item.state == "active") MSuccess else MWarning)
            }
            if (item.objective.isNotBlank()) Text(item.objective, color = Muted, fontSize = 13.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                MonitorPill(item.cadenceSummary, MInfo)
                if (item.health != "ok") MonitorPill(healthLabel(item.health), MWarning)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                MonitorPill(runStatusLabel(item.lastRunStatus), monitorStatusTint(item.lastRunStatus))
                relativeMonitorTime(item.lastRunAt)?.let { Text("· $it", color = Muted, fontSize = 11.sp) }
            }
        }
    }
}

@Composable
internal fun MonitorDetailScreen(state: TasksUiState, viewModel: TasksViewModel) {
    val detailState = state.monitorDetail
    var tab by remember(detailState.taskId, detailState.highlightUpdateId) {
        mutableIntStateOf(if (detailState.highlightUpdateId == null) 0 else 1)
    }
    var showEdit by remember { mutableStateOf(false) }
    var confirmDelete by remember { mutableStateOf(false) }
    val detailError = detailState.error
    val loadedBundle = detailState.bundle
    Column(Modifier.fillMaxSize().background(Ground)) {
        Row(
            Modifier.fillMaxWidth().background(Panel).padding(horizontal = 6.dp, vertical = 5.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = viewModel::closeMonitorDetail) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back", tint = Ink) }
            Text("Monitor", color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            IconButton(onClick = { detailState.taskId?.let(viewModel::openMonitor) }) { Icon(Icons.Outlined.Refresh, "Refresh", tint = Coral) }
        }
        when {
            detailState.loading -> MonitorPlaceholder("Loading monitor…", loading = true)
            detailError != null -> MonitorPlaceholder(detailError, action = "Try again") {
                detailState.taskId?.let(viewModel::openMonitor)
            }
            loadedBundle != null -> {
                val bundle = loadedBundle
                MonitorHeader(
                    bundle,
                    viewModel,
                    busy = state.mutatingTaskId != null,
                    onEdit = { showEdit = true },
                    onDelete = { confirmDelete = true },
                )
                LazyRow(
                    modifier = Modifier.fillMaxWidth().background(Panel),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 12.dp, vertical = 7.dp),
                    horizontalArrangement = Arrangement.spacedBy(7.dp),
                ) {
                    items(listOf("Latest", "Updates", "Runs", "Settings")) { label ->
                        val index = listOf("Latest", "Updates", "Runs", "Settings").indexOf(label)
                        MagicianFilterChip(text = label, selected = tab == index, onClick = { tab = index })
                    }
                }
                when (tab) {
                    0 -> MonitorLatest(bundle, detailState, viewModel, onEdit = { showEdit = true })
                    1 -> MonitorUpdates(bundle, detailState, viewModel, onEdit = { showEdit = true })
                    2 -> MonitorRuns(bundle)
                    else -> MonitorSettings(bundle)
                }
            }
        }
    }
    detailState.bundle?.let { bundle ->
        if (showEdit) MonitorComposer(
            title = "Edit Monitor",
            initial = bundle.toDraft(),
            onDismiss = { showEdit = false },
            onSave = { draft -> viewModel.updateMonitor(bundle.detail.taskId, draft) { if (it) showEdit = false } },
            busy = state.mutatingTaskId != null,
            isEdit = true,
        )
        if (confirmDelete) AlertDialog(
            onDismissRequest = { confirmDelete = false },
            title = { Text("Delete this monitor?") },
            text = { Text("It will be archived and disappear from monitor surfaces. Existing task history stays unless files are explicitly removed.") },
            dismissButton = { TextButton(onClick = { confirmDelete = false }) { Text("Keep monitor") } },
            confirmButton = { Button(shape = MagicanButtonShape,
                onClick = { viewModel.deleteMonitor(bundle.detail.taskId, false) { if (it) confirmDelete = false } },
                colors = ButtonDefaults.buttonColors(containerColor = MDanger),
            ) { Text("Delete monitor") } },
        )
    }
}

@Composable
private fun MonitorHeader(
    bundle: MonitorDetailBundle,
    viewModel: TasksViewModel,
    busy: Boolean,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
) {
    val detail = bundle.detail
    Column(Modifier.fillMaxWidth().background(Panel).padding(14.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
        Row(verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f)) {
                Text(detail.title.ifBlank { "Untitled monitor" }, color = Ink, fontSize = 19.sp, fontWeight = FontWeight.Bold)
                Text(detail.spec.objective, color = Muted, fontSize = 13.sp, maxLines = 3, overflow = TextOverflow.Ellipsis)
            }
            MonitorPill(if (detail.state.status == "paused") "Paused" else "Active", if (detail.state.status == "paused") MWarning else MSuccess)
        }
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
            Button(shape = MagicanButtonShape, onClick = { viewModel.monitorAction(detail.taskId, "run") }, enabled = !busy, colors = ButtonDefaults.buttonColors(containerColor = Coral)) {
                Icon(Icons.Outlined.PlayArrow, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(4.dp)); Text("Run now")
            }
            OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.monitorAction(detail.taskId, if (detail.state.status == "paused") "resume" else "pause") }, enabled = !busy) {
                Icon(if (detail.state.status == "paused") Icons.Outlined.PlayArrow else Icons.Outlined.Pause, null, modifier = Modifier.size(16.dp))
                Spacer(Modifier.width(4.dp)); Text(if (detail.state.status == "paused") "Resume" else "Pause")
            }
            OutlinedButton(shape = MagicanButtonShape, onClick = onEdit, enabled = !busy) { Icon(Icons.Outlined.Edit, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(4.dp)); Text("Edit") }
            OutlinedButton(shape = MagicanButtonShape, onClick = {
                viewModel.openTask(TaskV3(
                    id = detail.taskId, title = detail.title, description = detail.spec.objective,
                    status = if (detail.state.status == "paused") "paused" else "ready",
                    monitorRevision = detail.monitorRevision, createdAt = detail.createdAt, updatedAt = detail.updatedAt,
                ))
            }) { Icon(Icons.Outlined.Link, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(4.dp)); Text("Task detail") }
            TextButton(onClick = onDelete) { Icon(Icons.Outlined.Delete, null, tint = MDanger, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(4.dp)); Text("Delete", color = MDanger) }
        }
    }
}

@Composable
private fun MonitorLatest(
    bundle: MonitorDetailBundle,
    detailState: ai.magicbeans.magdroid.tasks.MonitorDetailState,
    viewModel: TasksViewModel,
    onEdit: () -> Unit,
) {
    val latest = bundle.updates.firstOrNull()
    if (latest == null) {
        MonitorPlaceholder("No updates yet. Run the monitor to establish its baseline.")
    } else LazyColumn(
        Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) { item { MonitorUpdateCard(
        latest, bundle.detail.taskId, viewModel,
        selectedVerdict = bundle.feedbackByUpdate[latest.updateId],
        feedbackBusy = latest.updateId in detailState.feedbackInFlight,
        highlighted = latest.updateId == detailState.highlightUpdateId,
        onEdit = onEdit,
        canPause = bundle.detail.schedule != null && bundle.detail.state.status != "paused",
    ) } }
}

@Composable
private fun MonitorUpdates(
    bundle: MonitorDetailBundle,
    detailState: ai.magicbeans.magdroid.tasks.MonitorDetailState,
    viewModel: TasksViewModel,
    onEdit: () -> Unit,
) {
    if (bundle.updates.isEmpty()) return MonitorPlaceholder("No durable updates yet.")
    LazyColumn(
        Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) { items(bundle.updates, key = MonitorUpdate::updateId) { update -> MonitorUpdateCard(
        update, bundle.detail.taskId, viewModel,
        selectedVerdict = bundle.feedbackByUpdate[update.updateId],
        feedbackBusy = update.updateId in detailState.feedbackInFlight,
        highlighted = update.updateId == detailState.highlightUpdateId,
        onEdit = onEdit,
        canPause = bundle.detail.schedule != null && bundle.detail.state.status != "paused",
    ) } }
}

@Composable
private fun MonitorUpdateCard(
    update: MonitorUpdate,
    taskId: String,
    viewModel: TasksViewModel,
    selectedVerdict: String?,
    feedbackBusy: Boolean,
    highlighted: Boolean,
    onEdit: () -> Unit,
    canPause: Boolean,
) {
    Card(
        colors = CardDefaults.cardColors(containerColor = if (highlighted) Coral.copy(alpha = .06f) else Panel),
        border = BorderStroke(if (highlighted) 2.dp else 1.dp, if (highlighted) Coral else BorderSoft),
    ) {
        Column(Modifier.padding(13.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.Top) {
                Column(Modifier.weight(1f)) {
                    Text(update.headline.ifBlank { runStatusLabel(update.status) }, color = Ink, fontWeight = FontWeight.Bold)
                    Text(update.occurredAt, color = Muted, fontSize = 11.sp)
                }
                MonitorPill(runStatusLabel(update.status), monitorStatusTint(update.status))
            }
            if (update.summary.isNotBlank()) Text(update.summary, color = Ink, fontSize = 13.sp)
            update.findings.forEach { MonitorFindingRow(it) }
            Text(
                if (update.notification.emitted) "Notified · ${update.notification.channel}" else "Not notified",
                color = Muted,
                fontSize = 11.sp,
            )
            if (update.status == "changed") Row(
                Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(7.dp),
            ) {
                OutlinedButton(shape = MagicanButtonShape,
                    onClick = { viewModel.monitorFeedback(taskId, update.updateId, "useful") },
                    enabled = !feedbackBusy,
                    colors = if (selectedVerdict == "useful") ButtonDefaults.outlinedButtonColors(containerColor = MSuccess.copy(alpha = .12f)) else ButtonDefaults.outlinedButtonColors(),
                ) {
                    Icon(Icons.Outlined.ThumbUp, null, tint = MSuccess, modifier = Modifier.size(15.dp)); Spacer(Modifier.width(4.dp)); Text("Useful", fontSize = 11.sp)
                }
                OutlinedButton(shape = MagicanButtonShape,
                    onClick = { viewModel.monitorFeedback(taskId, update.updateId, "not_relevant") },
                    enabled = !feedbackBusy,
                    colors = if (selectedVerdict == "not_relevant") ButtonDefaults.outlinedButtonColors(containerColor = MWarning.copy(alpha = .12f)) else ButtonDefaults.outlinedButtonColors(),
                ) {
                    Icon(Icons.Outlined.ThumbDown, null, tint = MWarning, modifier = Modifier.size(15.dp)); Spacer(Modifier.width(4.dp)); Text("Not relevant", fontSize = 11.sp)
                }
                OutlinedButton(shape = MagicanButtonShape, onClick = onEdit) {
                    Icon(Icons.Outlined.Edit, null, tint = MInfo, modifier = Modifier.size(15.dp))
                    Spacer(Modifier.width(4.dp)); Text("Edit monitor", fontSize = 11.sp)
                }
                if (canPause) OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.monitorAction(taskId, "pause") }) {
                    Icon(Icons.Outlined.Pause, null, tint = MWarning, modifier = Modifier.size(15.dp))
                    Spacer(Modifier.width(4.dp)); Text("Pause monitor", fontSize = 11.sp)
                }
            }
        }
    }
}

@Composable
private fun MonitorFindingRow(finding: MonitorFinding) {
    Surface(color = Ground, shape = RoundedCornerShape(10.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row {
                Text(finding.title.ifBlank { "Finding" }, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                MonitorPill(finding.classification.replace('_', ' '), monitorStatusTint(finding.classification))
            }
            if (finding.summary.isNotBlank()) Text(finding.summary, color = Muted, fontSize = 12.sp)
            if (finding.whyItMatters.isNotBlank()) Text("Why it matters · ${finding.whyItMatters}", color = Ink, fontSize = 12.sp)
            if (finding.source.isNotBlank()) Text(finding.source, color = MInfo, fontSize = 11.sp)
        }
    }
}

@Composable
private fun MonitorRuns(bundle: MonitorDetailBundle) {
    if (bundle.runs.isEmpty()) return MonitorPlaceholder("No finalized runs yet.")
    LazyColumn(
        Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) { items(bundle.runs, key = MonitorRun::executionId) { MonitorRunCard(it) } }
}

@Composable
private fun MonitorRunCard(run: MonitorRun) {
    Card(colors = CardDefaults.cardColors(containerColor = Panel), border = BorderStroke(1.dp, BorderSoft)) {
        Column(Modifier.padding(13.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Row {
                Column(Modifier.weight(1f)) {
                    Text(run.executionId, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(run.completedAt.ifBlank { run.startedAt }, color = Muted, fontSize = 11.sp)
                }
                MonitorPill(runStatusLabel(run.status), monitorStatusTint(run.status))
            }
            Text(
                "Scanned ${run.counts.scanned} · ${run.counts.new} new · ${run.counts.updated} updated · ${run.counts.unchanged} unchanged",
                color = Muted, fontSize = 12.sp,
            )
            if (!run.completeScan) Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.ErrorOutline, null, tint = MWarning, modifier = Modifier.size(15.dp)); Spacer(Modifier.width(5.dp)); Text("Partial scan", color = MWarning, fontSize = 12.sp)
            }
            run.sourceOutcomes.forEach { outcome ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        if (outcome.status == "ok") Icons.Outlined.CheckCircle else Icons.Outlined.ErrorOutline,
                        null,
                        tint = if (outcome.status == "ok") MSuccess else MWarning,
                        modifier = Modifier.size(14.dp),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(outcome.source, color = Muted, fontSize = 11.sp, modifier = Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(
                        if (outcome.status == "ok") "${outcome.itemsScanned} items" else runStatusLabel(outcome.status),
                        color = if (outcome.status == "ok") Muted else MWarning,
                        fontSize = 11.sp,
                    )
                }
            }
            run.accessProblem?.let { problem ->
                Row(
                    Modifier.fillMaxWidth().background(MWarning.copy(alpha = .12f), RoundedCornerShape(9.dp)).padding(8.dp),
                    verticalAlignment = Alignment.Top,
                ) {
                    Icon(Icons.Outlined.ErrorOutline, null, tint = MWarning, modifier = Modifier.size(16.dp))
                    Spacer(Modifier.width(6.dp))
                    Text(problem.message.ifBlank { "${problem.source} needs attention." }, color = MWarning, fontSize = 12.sp)
                }
            }
        }
    }
}

@Composable
private fun MonitorSettings(bundle: MonitorDetailBundle) {
    val detail = bundle.detail
    LazyColumn(
        Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(9.dp),
    ) {
        item { MonitorSetting("Objective", detail.spec.objective) }
        item { MonitorSetting("Match mode", detail.spec.matchMode) }
        item { MonitorSetting("Notification", detail.spec.notificationPolicy.replace('_', ' ')) }
        // Editable in the composer and invisible here, so a monitor set to
        // announce its own baseline looked identical to one that stays quiet
        // until something changes.
        item {
            MonitorSetting(
                "First baseline",
                if (detail.spec.notifyInitialBaseline) "Notified" else "Not notified",
            )
        }
        item { MonitorSetting("Cadence", monitorCadence(detail.schedule)) }
        // Three different things, listed separately as iOS lists them. Merged
        // into one blob a reader could not tell a domain from a search phrase,
        // which are not the same instruction at all.
        item { MonitorSetting("URLs", detail.spec.sources.urls.joinToString("\n").ifBlank { "None" }) }
        item { MonitorSetting("Domains", detail.spec.sources.domains.joinToString("\n").ifBlank { "None" }) }
        item { MonitorSetting("Search phrases", detail.spec.querySeeds.joinToString("\n").ifBlank { "None" }) }
        item {
            MonitorSetting(
                "Signed-in sources",
                detail.spec.sources.authenticatedSources.joinToString("\n").ifBlank { "None" },
            )
        }
        item { MonitorSetting("Include rules", detail.spec.includeRules.joinToString("\n").ifBlank { "None" }) }
        item { MonitorSetting("Exclude rules", detail.spec.excludeRules.joinToString("\n").ifBlank { "None" }) }
        item { MonitorSetting("Revision", detail.monitorRevision.toString()) }
        item { MonitorSetting("Scheduled fires", detail.state.scheduleFireCount.toString()) }
        item { MonitorSetting("Task id", detail.taskId) }
    }
}

@Composable
private fun MonitorSetting(label: String, value: String) {
    Surface(color = Panel, shape = RoundedCornerShape(12.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Column(Modifier.fillMaxWidth().padding(12.dp)) {
            Text(label.uppercase(), color = Muted, fontSize = 10.sp, fontWeight = FontWeight.Bold)
            Text(value, color = Ink, fontSize = 13.sp)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MonitorComposer(
    title: String,
    initial: MonitorDraft?,
    onDismiss: () -> Unit,
    onSave: (MonitorDraft) -> Unit,
    busy: Boolean,
    keptSchedule: String? = null,
    /**
     * Editing an existing monitor rather than writing a new one.
     *
     * Only the cadence list reads this, and for a reason worth stating: a PATCH
     * omits `schedule` when no cron is set, so choosing "On demand only" while
     * editing changes nothing on the server. The monitor keeps its old cadence
     * while the form says it does not.
     */
    isEdit: Boolean = false,
) {
    var draft by remember(initial) { mutableStateOf(initial ?: MonitorDraft()) }
    var review by remember { mutableStateOf(false) }
    // Collapsed by default, as on web and iOS: the four fields above it are
    // what a monitor actually needs, and eleven at once reads as a form to be
    // filled in rather than a thing to be asked for.
    var showAdvanced by remember { mutableStateOf(false) }
    val error = draft.validationError()
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Ground,
    ) {
        LazyColumn(
            Modifier.fillMaxWidth().padding(horizontal = 18.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(bottom = 30.dp),
        ) {
            item {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Text(title, color = Ink, fontSize = 19.sp, fontWeight = FontWeight.Bold, modifier = Modifier.weight(1f))
                    TextButton(onClick = onDismiss) { Text("Cancel", color = Coral) }
                }
            }
            keptSchedule?.let { item { Text("The existing task schedule is kept: $it", color = MInfo, fontSize = 12.sp) } }
            if (!review) {
                item { MagicianTextField(draft.title, { draft = draft.copy(title = it) }, Modifier.fillMaxWidth(), label = { Text("Title · optional") }) }
                item { MagicianTextField(draft.objective, { draft = draft.copy(objective = it) }, Modifier.fillMaxWidth(), label = { Text("What should Magician monitor?") }, minLines = 3) }
                item { MonitorMultiline("URLs · one per line", draft.urls) { draft = draft.copy(urls = it) } }
                if (keptSchedule == null) {
                    item {
                        MonitorChoice(
                            "Cadence",
                            monitorPreset(draft.cron),
                            cadenceChoices(isEdit, monitorPreset(draft.cron)),
                        ) { choice -> draft = draft.copy(cron = presetCron(choice, draft.cron)) }
                    }
                    if (monitorPreset(draft.cron) == "custom") item {
                        MagicianTextField(draft.cron.orEmpty(), { draft = draft.copy(cron = it) }, Modifier.fillMaxWidth(), label = { Text("Five-field cron") })
                    }
                    if (draft.cron != null) item { MagicianTextField(draft.timezone, { draft = draft.copy(timezone = it) }, Modifier.fillMaxWidth(), label = { Text("Timezone") }) }
                }
                item { MonitorChoice("Notify", draft.notificationPolicy, listOf("material_changes", "every_run", "never")) { draft = draft.copy(notificationPolicy = it) } }

                // Everything a monitor can be narrowed with, rather than
                // everything it needs. Web and iOS draw the same line here, and
                // the fields behind it are the ones that only matter once the
                // simple form has produced something worth refining.
                item {
                    TextButton(onClick = { showAdvanced = !showAdvanced }) {
                        Text(
                            if (showAdvanced) "Fewer options" else "More options",
                            color = Coral, fontSize = 13.sp,
                        )
                    }
                }
                if (showAdvanced) {
                    item { MonitorMultiline("Domains · one per line", draft.domains) { draft = draft.copy(domains = it) } }
                    item { MonitorMultiline("Search phrases · one per line", draft.querySeeds) { draft = draft.copy(querySeeds = it) } }
                    item { MonitorMultiline("Include rules · one per line", draft.includeRules) { draft = draft.copy(includeRules = it) } }
                    item { MonitorMultiline("Exclude rules · one per line", draft.excludeRules) { draft = draft.copy(excludeRules = it) } }
                    item { MonitorChoice("Match mode", draft.matchMode, listOf("strict", "balanced", "broad")) { draft = draft.copy(matchMode = it) } }
                    item { MonitorMultiline("Signed-in sources · one per line", draft.authenticatedSources) { draft = draft.copy(authenticatedSources = it) } }
                    item {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(draft.notifyInitialBaseline, { draft = draft.copy(notifyInitialBaseline = it) })
                            Text("Notify on the first baseline", color = Ink, fontSize = 13.sp)
                        }
                    }
                }
                error?.let { item { Text(it, color = MDanger, fontSize = 12.sp) } }
                item { Button(shape = MagicanButtonShape, onClick = { review = true }, enabled = error == null, modifier = Modifier.fillMaxWidth()) { Text("Review") } }
            } else {
                item { MonitorReview("Title", draft.title.ifBlank { "Derived from objective" }) }
                item { MonitorReview("Objective", draft.objective) }
                item { MonitorReview("Sources", (draft.urls + draft.domains + draft.querySeeds).joinToString("\n")) }
                item { MonitorReview("Matching", draft.matchMode) }
                item { MonitorReview("Notifications", draft.notificationPolicy.replace('_', ' ')) }
                item { MonitorReview("Cadence", keptSchedule ?: draft.cron ?: "Run on demand") }
                item {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedButton(shape = MagicanButtonShape, onClick = { review = false }, modifier = Modifier.weight(1f)) { Text("Back") }
                        Button(shape = MagicanButtonShape, onClick = { onSave(draft) }, enabled = !busy, modifier = Modifier.weight(1f), colors = ButtonDefaults.buttonColors(containerColor = Coral)) {
                            if (busy) CircularProgressIndicator(Modifier.size(16.dp), color = Color.White, strokeWidth = 2.dp) else Text("Save")
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun MonitorMultiline(label: String, values: List<String>, onChange: (List<String>) -> Unit) {
    MagicianTextField(
        value = values.joinToString("\n"),
        onValueChange = { onChange(it.lines()) },
        modifier = Modifier.fillMaxWidth(), label = { Text(label) }, minLines = 2, maxLines = 5,
    )
}

@Composable
private fun MonitorChoice(label: String, value: String, choices: List<String>, onChange: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        MagicianTextField(
            value = value.replace('_', ' '), onValueChange = {}, readOnly = true,
            modifier = Modifier.fillMaxWidth().clickable { expanded = true }, label = { Text(label) },
        )
        DropdownMenu(expanded, { expanded = false }) {
            choices.forEach { choice -> DropdownMenuItem(
                text = { Text(choice.replace('_', ' ')) },
                onClick = { onChange(choice); expanded = false },
            ) }
        }
    }
}

@Composable
private fun MonitorReview(label: String, value: String) {
    Column(Modifier.fillMaxWidth()) {
        Text(label.uppercase(), color = Muted, fontSize = 10.sp, fontWeight = FontWeight.Bold)
        Text(value.ifBlank { "None" }, color = Ink, fontSize = 13.sp)
    }
}

private val MSuccess = Color(0xFF2E9B72)
private val MWarning = Color(0xFFE6952F)
private val MDanger = Color(0xFFD9544D)
private val MInfo = Color(0xFF3978C5)

@Composable
private fun MonitorPill(label: String, tint: Color) {
    Text(label, color = tint, fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
        modifier = Modifier.background(tint.copy(alpha = .12f), CircleShape).padding(horizontal = 8.dp, vertical = 3.dp))
}

private fun runStatusLabel(status: String): String = when (status) {
    "never_ran" -> "Never ran"; "possibly_removed" -> "Possibly removed"
    else -> status.replace('_', ' ').replaceFirstChar(Char::uppercase)
}

private fun monitorStatusTint(status: String): Color = when (status) {
    "changed", "new", "updated" -> MInfo
    "unchanged", "baseline", "useful" -> MSuccess
    "degraded", "possibly_removed", "not_relevant" -> MWarning
    "failed" -> MDanger
    else -> Muted
}

private fun healthLabel(health: String): String = when (health) {
    "needs_attention" -> "Needs attention"; "failing" -> "Failing"; else -> health.replace('_', ' ')
}

private fun relativeMonitorTime(value: String?): String? {
    val instant = runCatching { java.time.Instant.parse(value) }.getOrNull() ?: return null
    val seconds = ((System.currentTimeMillis() - instant.toEpochMilli()) / 1000).coerceAtLeast(0)
    return when {
        seconds < 60 -> "now"; seconds < 3_600 -> "${seconds / 60}m ago"
        seconds < 86_400 -> "${seconds / 3_600}h ago"; else -> "${seconds / 86_400}d ago"
    }
}

private fun monitorCadence(schedule: kotlinx.serialization.json.JsonObject?): String {
    val kind = schedule?.get("kind") as? kotlinx.serialization.json.JsonObject
    val cron = kind?.get("Cron") as? kotlinx.serialization.json.JsonObject
    val expression = cron?.get("expression")?.let { (it as? kotlinx.serialization.json.JsonPrimitive)?.content }
    return expression?.let { "Cron $it" } ?: "Run on demand"
}

private fun MonitorDetailBundle.toDraft(): MonitorDraft = MonitorDraft(
    title = detail.title,
    objective = detail.spec.objective,
    urls = detail.spec.sources.urls,
    domains = detail.spec.sources.domains,
    authenticatedSources = detail.spec.sources.authenticatedSources,
    querySeeds = detail.spec.querySeeds,
    includeRules = detail.spec.includeRules,
    excludeRules = detail.spec.excludeRules,
    matchMode = detail.spec.matchMode,
    notificationPolicy = detail.spec.notificationPolicy,
    notifyInitialBaseline = detail.spec.notifyInitialBaseline,
    cron = (detail.schedule?.get("kind") as? kotlinx.serialization.json.JsonObject)
        ?.get("Cron")?.let { it as? kotlinx.serialization.json.JsonObject }
        ?.get("expression")?.let { it as? kotlinx.serialization.json.JsonPrimitive }?.content,
    timezone = detail.schedule?.get("timezone")?.let { it as? kotlinx.serialization.json.JsonPrimitive }?.content
        ?: ZoneId.systemDefault().id,
)

private fun monitorPreset(cron: String?): String = when (cron) {
    null -> "none"; "0 * * * *" -> "hourly"; "0 9 * * *" -> "daily-9"; "0 18 * * *" -> "daily-18"
    "0 9 * * 1-5" -> "weekdays-9"; "0 9 * * 1" -> "weekly-mon-9"; "0 9 1 * *" -> "monthly-1-9"; else -> "custom"
}

/**
 * The cadence options to offer, given whether this is an edit.
 *
 * "On demand only" is withheld while editing because choosing it does nothing:
 * a PATCH omits `schedule` when no cron is set, so the monitor keeps running on
 * the cadence it already had while the form claims otherwise. It stays on the
 * list when it is already the selection — a monitor with no schedule, or one on
 * an interval this form cannot express, opens there and must not silently
 * appear to be on a cron.
 *
 * Web and iOS apply the same rule; this client offered the dead option.
 */
internal fun cadenceChoices(isEdit: Boolean, current: String): List<String> {
    val scheduled = listOf("hourly", "daily-9", "daily-18", "weekdays-9", "weekly-mon-9", "monthly-1-9", "custom")
    return if (!isEdit || current == "none") listOf("none") + scheduled else scheduled
}

private fun presetCron(preset: String, existing: String?): String? = when (preset) {
    "none" -> null; "hourly" -> "0 * * * *"; "daily-9" -> "0 9 * * *"; "daily-18" -> "0 18 * * *"
    "weekdays-9" -> "0 9 * * 1-5"; "weekly-mon-9" -> "0 9 * * 1"; "monthly-1-9" -> "0 9 1 * *"
    else -> existing ?: ""
}
