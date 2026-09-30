package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.apps.AppEntitySurface
import ai.magicbeans.magdroid.apps.AppIndicatorModel
import ai.magicbeans.magdroid.apps.AppMaterializedIndicator
import ai.magicbeans.magdroid.apps.AppSlotContextualRegion
import ai.magicbeans.magdroid.apps.AppSlotEditorUiState
import ai.magicbeans.magdroid.apps.AppSlotId
import ai.magicbeans.magdroid.apps.AppSlotPickerCandidate
import ai.magicbeans.magdroid.apps.AppSurfacingViewModel
import ai.magicbeans.magdroid.apps.AppWidgetGovernedAction
import ai.magicbeans.magdroid.apps.AppWidgetNativeModel
import ai.magicbeans.magdroid.apps.AppWidgetRenderItem
import ai.magicbeans.magdroid.apps.AppWidgetRenderRow
import ai.magicbeans.magdroid.apps.appWidgetDisplay
import ai.magicbeans.magdroid.apps.orderedForSlot
import ai.magicbeans.magdroid.apps.showsUnavailablePlaceholder
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
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
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowForward
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Apps
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.Badge
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.serialization.json.JsonObject

/** Generic native slot-region wrapper. It never mounts a WebView. */
@Composable
fun AppWidgetSlotRegion(
    page: String,
    region: String,
    viewModel: AppSurfacingViewModel,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val slotId = runCatching { AppSlotId.forPageRegion(page, region) }.getOrNull() ?: return
    val active = state.activePage?.takeIf { it.page == page && it.regions.any { spec -> spec.slotId == slotId } }
        ?: return
    // The last good snapshot stays on screen while a refresh revalidates slot
    // and package authority (stale-while-revalidate); only its controls and
    // actions are held until the binding check returns.
    val visibleSnapshot = active.snapshot
    val revalidating = active.loading && visibleSnapshot != null
    val assignment = visibleSnapshot?.assignments?.get(slotId)
    val target = assignment?.target
    val item = visibleSnapshot?.widgetsBySlot?.get(slotId)
    val model = item?.model
    val fallback = item?.fallback
    val editor = state.slotEditor
    // One editor owns one region at a time, because a settings read advances
    // the host's write fence and would strand any other open one.
    val ownsEditor = editor.region == region
    // An entity route is itself an app surface; offering to open another from
    // inside it would bury the page this one was opened over.
    val entityOpener: ((AppWidgetRenderItem) -> Unit)? = if (state.entitySurface != null) {
        null
    } else {
        { opened ->
            viewModel.openEntityPage(
                opened.installationId,
                opened.title ?: opened.widgetId.replace('_', ' '),
            )
        }
    }

    Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        when {
            active.loading && visibleSnapshot == null -> Surface(
                Modifier.fillMaxWidth(),
                shape = RoundedCornerShape(14.dp),
                color = Panel,
                border = BorderStroke(1.dp, BorderSoft),
            ) {
                Row(
                    Modifier.padding(16.dp),
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = Coral)
                    Text("Loading app widget…", color = Muted, fontSize = 12.sp)
                }
            }
            assignment == null -> Unit
            // Nothing owns this slot — or only a workspace default the host
            // hid for a reason the owner cannot act on — so the only thing to
            // offer is the picker, never a broken-widget card.
            target == null && (!assignment.occupied || assignment.quietlyHiddenDefault) -> AppWidgetSlotEmpty(
                region = region,
                optedOut = assignment.optedOut,
                enabled = !editor.busy && !active.loading,
                onAdd = { viewModel.openSlotPicker(region) },
                onRestoreDefault = { viewModel.restoreWorkspaceDefaultForSlot(region) },
            )
            // Retained as removable layout: an assignment whose package is
            // gone must stay reclaimable rather than silently erasing itself.
            target == null || item == null -> if (assignment.showsUnavailablePlaceholder(item)) AppWidgetUnavailable(
                title = "App widget unavailable",
                detail = active.error,
                onRetry = viewModel::refreshNow,
                modifier = Modifier,
            )
            item.state == "ready" && model != null -> AppNativeWidget(
                item = item,
                model = model,
                actionInFlight = { action -> viewModel.actionInFlight(assignment, item, action.actionId) },
                actionsEnabled = !active.loading,
                onAction = { action -> viewModel.launchEmptyAction(assignment, item, action.actionId) },
                actionMessage = state.actionNotice?.takeIf { notice ->
                    notice.installationId == item.installationId &&
                        model.actions.any { it.actionId == notice.actionId }
                },
                onOpenEntity = entityOpener,
                modifier = Modifier,
            )
            item.state == "unsupported" && fallback?.kind == "hide" -> Unit
            item.state == "unsupported" && fallback?.kind == "message" -> AppWidgetUnavailable(
                title = fallback.title ?: item.title ?: "App widget unavailable",
                detail = fallback.body,
                onRetry = viewModel::refreshNow,
                modifier = Modifier,
            )
            else -> AppWidgetUnavailable(
                title = item.title ?: "App widget unavailable",
                detail = active.error,
                onRetry = viewModel::refreshNow,
                modifier = Modifier,
            )
        }
        if (revalidating) AppWidgetRevalidating()
        if (assignment != null && assignment.occupied && !assignment.quietlyHiddenDefault) AppSlotOccupiedControls(
            region = region,
            userAssigned = assignment.source == "user",
            enabled = !editor.busy && !active.loading,
            onRemove = { viewModel.optOutSlot(region) },
            onRestoreDefault = { viewModel.restoreWorkspaceDefaultForSlot(region) },
        )
        // The picker outlives a page revalidation on purpose: the poll hides
        // cards every cadence, and a chooser that vanished under the owner's
        // finger would be worse than one that briefly refuses a tap.
        if (ownsEditor && editor.pickerOpen) AppSlotPicker(
            slotId = slotId,
            editor = editor,
            enabled = !editor.busy && !active.loading,
            onAssign = viewModel::assignPickerCandidate,
            onLoadMore = viewModel::loadMorePickerCandidates,
            onClose = viewModel::closeSlotPicker,
        )
        if (ownsEditor) editor.error?.let { message ->
            AppSlotEditorNotice(
                message = message,
                retryable = editor.retryable,
                enabled = !editor.mutating,
                onRetry = viewModel::retryPendingSlotMutation,
            )
        }
    }
}

/** A quiet inline cue that the visible card is being revalidated. */
@Composable
private fun AppWidgetRevalidating() {
    Row(
        Modifier.fillMaxWidth().semantics { contentDescription = "Refreshing app widget" },
        horizontalArrangement = Arrangement.End,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CircularProgressIndicator(Modifier.size(10.dp), strokeWidth = 1.5.dp, color = Muted)
    }
}

/**
 * The unassigned state of a slot.
 *
 * Web draws the same affordance: an empty region is where a widget is chosen,
 * not a gap. An opted-out slot also offers the way back to whatever the
 * workspace pinned, which is otherwise unreachable once opted out.
 */
@Composable
private fun AppWidgetSlotEmpty(
    region: String,
    optedOut: Boolean,
    enabled: Boolean,
    onAdd: () -> Unit,
    onRestoreDefault: () -> Unit,
) {
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(14.dp),
        color = Color.Transparent,
        border = BorderStroke(1.dp, BorderSoft),
    ) {
        Row(
            Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(onClick = onAdd, enabled = enabled) {
                Icon(
                    Icons.Outlined.Add,
                    contentDescription = "Add a widget to $region",
                    tint = Coral,
                    modifier = Modifier.size(16.dp),
                )
                Spacer(Modifier.width(6.dp))
                Text("Add a widget", color = Coral, fontSize = 12.sp)
            }
            Spacer(Modifier.weight(1f))
            if (optedOut) TextButton(
                onClick = onRestoreDefault,
                enabled = enabled,
                modifier = Modifier.semantics {
                    contentDescription = "Use the workspace default in $region"
                },
            ) {
                Text("Use the default", color = Muted, fontSize = 11.sp)
            }
        }
    }
}

/** Removal and restore for a slot some authority currently owns. */
@Composable
private fun AppSlotOccupiedControls(
    region: String,
    userAssigned: Boolean,
    enabled: Boolean,
    onRemove: () -> Unit,
    onRestoreDefault: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.End,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (userAssigned) TextButton(
            onClick = onRestoreDefault,
            enabled = enabled,
            modifier = Modifier.semantics {
                contentDescription = "Use the workspace default in $region"
            },
        ) {
            Text("Use the default", color = Muted, fontSize = 11.sp)
        }
        TextButton(
            onClick = onRemove,
            enabled = enabled,
            modifier = Modifier.semantics {
                contentDescription = "Remove the widget from $region"
            },
        ) {
            Text("Remove", color = Muted, fontSize = 11.sp)
        }
    }
}

/**
 * The bounded picker, inline rather than modal.
 *
 * It is drawn where the slot is, so the choice is made against the layout it
 * changes; the list is height-capped and scrolls inside itself so a long
 * inventory cannot take over the page it is being fitted into.
 */
@Composable
private fun AppSlotPicker(
    slotId: AppSlotId,
    editor: AppSlotEditorUiState,
    enabled: Boolean,
    onAssign: (AppSlotPickerCandidate) -> Unit,
    onLoadMore: () -> Unit,
    onClose: () -> Unit,
) {
    val settings = editor.settings
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(14.dp),
        color = Panel,
        border = BorderStroke(1.dp, BorderSoft),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Choose a widget",
                    color = Ink,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onClose, enabled = !editor.mutating) {
                    Text("Close", color = Coral, fontSize = 12.sp)
                }
            }
            when {
                settings == null -> Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp, color = Coral)
                    Text("Loading widgets…", color = Muted, fontSize = 12.sp)
                }
                settings.picker.isEmpty() ->
                    Text("No installed app widgets are available.", color = Muted, fontSize = 12.sp)
                else -> Column(
                    Modifier.heightIn(max = 300.dp).verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    settings.picker.orderedForSlot(slotId).forEach { candidate ->
                        AppSlotPickerRow(
                            candidate = candidate,
                            suggested = slotId.value in candidate.suggestedSlotIds,
                            enabled = enabled,
                            onAssign = { onAssign(candidate) },
                        )
                    }
                }
            }
            if (settings?.pickerTruncated == true) TextButton(
                onClick = onLoadMore,
                enabled = enabled,
            ) {
                Text("Load more widgets", color = Coral, fontSize = 12.sp)
            }
        }
    }
}

@Composable
private fun AppSlotPickerRow(
    candidate: AppSlotPickerCandidate,
    suggested: Boolean,
    enabled: Boolean,
    onAssign: () -> Unit,
) {
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(10.dp),
        color = Control,
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Row(Modifier.padding(2.dp), verticalAlignment = Alignment.CenterVertically) {
            TextButton(onClick = onAssign, enabled = enabled, modifier = Modifier.weight(1f)) {
                Column(Modifier.fillMaxWidth()) {
                    Text(
                        candidate.title,
                        color = Ink,
                        fontSize = 13.sp,
                        fontWeight = FontWeight.Medium,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        candidate.installationId,
                        color = Muted,
                        fontSize = 10.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            if (suggested) Text(
                "Suggested",
                color = Coral,
                fontSize = 9.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.padding(end = 10.dp),
            )
        }
    }
}

/**
 * The one place a slot change reports itself.
 *
 * An ambiguous write keeps its retry here rather than being restated as a new
 * change; retrying replays the same fence and mutation id.
 */
@Composable
private fun AppSlotEditorNotice(
    message: String,
    retryable: Boolean,
    enabled: Boolean,
    onRetry: () -> Unit,
) {
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(10.dp),
        color = Control,
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Row(
            Modifier.padding(start = 10.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(message, color = Danger, fontSize = 11.sp, modifier = Modifier.weight(1f))
            if (retryable) TextButton(onClick = onRetry, enabled = enabled) {
                Text("Retry", color = Coral, fontSize = 11.sp)
            }
        }
    }
}

/**
 * The contextual fitting for one app's canonical entity route.
 *
 * Rendered natively like every other app surface on this client: no frame, no
 * WebView, no degraded fallback — the same slot region the shell pages use,
 * resolved for the app's own page. It is hosted by the shell rather than by a
 * widget card because opening the route retires the card's own region.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AppEntitySurfaceSheet(
    surface: AppEntitySurface,
    viewModel: AppSurfacingViewModel,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Ground,
    ) {
        Column(
            Modifier.fillMaxWidth().padding(horizontal = 18.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(
                        surface.title,
                        color = Ink,
                        fontSize = 18.sp,
                        fontWeight = FontWeight.Bold,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        surface.installationId,
                        color = Muted,
                        fontSize = 11.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                TextButton(onClick = onDismiss) { Text("Close", color = Coral) }
            }
            AppWidgetSlotRegion(
                page = surface.page,
                region = AppSlotContextualRegion,
                viewModel = viewModel,
            )
            Spacer(Modifier.height(28.dp))
        }
    }
}

@Composable
private fun AppNativeWidget(
    item: AppWidgetRenderItem,
    model: AppWidgetNativeModel,
    actionInFlight: (AppWidgetGovernedAction) -> Boolean,
    actionsEnabled: Boolean,
    onAction: (AppWidgetGovernedAction) -> Unit,
    actionMessage: ai.magicbeans.magdroid.apps.AppActionNotice?,
    onOpenEntity: ((AppWidgetRenderItem) -> Unit)?,
    modifier: Modifier,
) {
    val title = item.title ?: item.widgetId.replace('_', ' ')
    Card(
        modifier.fillMaxWidth(),
        shape = RoundedCornerShape(17.dp),
        colors = CardDefaults.cardColors(containerColor = Control.copy(alpha = .82f)),
        border = BorderStroke(1.dp, BorderSoft),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.Apps, null, tint = Coral, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(8.dp))
                Text(
                    title,
                    color = Ink,
                    fontSize = 19.sp,
                    fontWeight = FontWeight.Bold,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                // The card is the only place this client knows an installation
                // by identity, so it is where its own entity route is reached.
                onOpenEntity?.let { open ->
                    IconButton(onClick = { open(item) }) {
                        Icon(
                            Icons.AutoMirrored.Outlined.ArrowForward,
                            contentDescription = "Open $title",
                            tint = Muted,
                            modifier = Modifier.size(18.dp),
                        )
                    }
                }
            }
            when (model.model) {
                "detail" -> model.row?.let { AppWidgetDetail(it) }
                    ?: AppWidgetEmpty()
                "list" -> AppWidgetList(model.rows.orEmpty(), model)
                "table" -> AppWidgetTable(model.rows.orEmpty(), model.columns.orEmpty())
                "timeline" -> AppWidgetTimeline(model.rows.orEmpty(), model)
                "tree" -> AppWidgetTree(model.rows.orEmpty(), model)
                "graph" -> AppWidgetGraph(model.rows.orEmpty(), model)
                else -> AppWidgetEmpty("Unsupported native model")
            }
            if (model.actions.isNotEmpty()) {
                HorizontalDivider(color = BorderSoft)
                Row(
                    Modifier.horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    model.actions.forEachIndexed { index, action ->
                        val busy = actionInFlight(action)
                        if (index == 0) Button(
                            shape = MagicanButtonShape,
                            onClick = { onAction(action) },
                            enabled = !busy && actionsEnabled,
                            colors = ButtonDefaults.buttonColors(containerColor = Coral.copy(alpha = .14f), contentColor = Coral),
                        ) { Text(if (busy) "Starting…" else action.label, fontSize = 13.sp, maxLines = 1) }
                        else OutlinedButton(
                            shape = MagicanButtonShape,
                            onClick = { onAction(action) },
                            enabled = !busy && actionsEnabled,
                            border = BorderStroke(1.dp, BorderSoft),
                        ) { Text(if (busy) "Starting…" else action.label, fontSize = 13.sp, maxLines = 1, color = Ink) }
                    }
                }
            }
            actionMessage?.let { notice ->
                Text(
                    notice.message,
                    color = if (notice.succeeded) Coral else Danger,
                    fontSize = 11.sp,
                )
            }
        }
    }
}

@Composable
private fun AppWidgetDetail(row: AppWidgetRenderRow) {
    MuijJsonContentRenderer(JsonObject(row.fields))
}

@Composable
private fun AppWidgetList(rows: List<AppWidgetRenderRow>, model: AppWidgetNativeModel) {
    if (rows.isEmpty()) return AppWidgetEmpty()
    Column(verticalArrangement = Arrangement.spacedBy(7.dp)) {
        rows.forEach { row -> AppWidgetRow(row, model) }
    }
}

@Composable
private fun AppWidgetRow(row: AppWidgetRenderRow, model: AppWidgetNativeModel, leading: String? = null) {
    val displayKey = model.hints.displayField?.takeIf(row.fields::containsKey)
        ?: row.fields.keys.sorted().firstOrNull()
    val secondary = row.fields.entries
        .filter { it.key != displayKey }
        .take(2)
        .joinToString(" · ") { (key, value) -> "$key ${value.appWidgetDisplay()}" }
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(10.dp),
        color = Control,
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Row(Modifier.padding(10.dp), verticalAlignment = Alignment.Top) {
            leading?.let { Text(it, color = Coral, fontFamily = LocalMagicanFontFamilies.current.mono, fontSize = 11.sp); Spacer(Modifier.width(7.dp)) }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    displayKey?.let(row.fields::get).appWidgetDisplay().ifBlank { row.recordId },
                    color = Ink,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                )
                if (secondary.isNotBlank()) Text(
                    secondary,
                    color = Muted,
                    fontSize = 10.sp,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

@Composable
private fun AppWidgetTable(rows: List<AppWidgetRenderRow>, columns: List<String>) {
    if (rows.isEmpty() || columns.isEmpty()) return AppWidgetEmpty()
    Column(Modifier.horizontalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            columns.forEach { column ->
                Text(column, color = Muted, fontSize = 10.sp, fontWeight = FontWeight.Bold, modifier = Modifier.width(112.dp))
            }
        }
        HorizontalDivider(color = BorderSoft)
        rows.forEach { row ->
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                columns.forEach { column ->
                    Text(
                        row.fields[column].appWidgetDisplay(),
                        color = Ink,
                        fontSize = 11.sp,
                        modifier = Modifier.width(112.dp),
                        maxLines = 3,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

@Composable
private fun AppWidgetTimeline(rows: List<AppWidgetRenderRow>, model: AppWidgetNativeModel) {
    if (rows.isEmpty()) return AppWidgetEmpty()
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        rows.forEach { row ->
            Row(horizontalArrangement = Arrangement.spacedBy(9.dp), verticalAlignment = Alignment.Top) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Box(Modifier.size(8.dp), contentAlignment = Alignment.Center) {
                        Surface(Modifier.size(8.dp), shape = CircleShape, color = Coral) {}
                    }
                    Spacer(Modifier.height(4.dp))
                    Spacer(Modifier.width(1.dp).height(34.dp).background(BorderSoft))
                }
                Column(Modifier.weight(1f)) {
                    model.hints.timestampField?.let { timestamp ->
                        Text(row.fields[timestamp].appWidgetDisplay(), color = Muted, fontSize = 9.sp)
                    }
                    AppWidgetRow(row, model)
                }
            }
        }
    }
}

@Composable
private fun AppWidgetTree(rows: List<AppWidgetRenderRow>, model: AppWidgetNativeModel) {
    if (rows.isEmpty()) return AppWidgetEmpty()
    val depths = widgetTreeDepths(rows, model.hints.parentField)
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        rows.forEach { row ->
            Row(Modifier.padding(start = (depths[row.recordId] ?: 0).coerceAtMost(8).times(12).dp)) {
                AppWidgetRow(row, model, leading = "└")
            }
        }
    }
}

@Composable
private fun AppWidgetGraph(rows: List<AppWidgetRenderRow>, model: AppWidgetNativeModel) {
    if (rows.isEmpty()) return AppWidgetEmpty()
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(10.dp),
        color = Control,
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.AccountTree, null, tint = Coral, modifier = Modifier.size(16.dp))
                Spacer(Modifier.width(6.dp))
                Text("${rows.size} linked items", color = Muted, fontSize = 10.sp)
            }
            rows.forEach { row -> AppWidgetRow(row, model) }
        }
    }
}

@Composable
private fun AppWidgetEmpty(message: String = "No items") {
    Text(message, color = Muted, fontSize = 12.sp, modifier = Modifier.padding(vertical = 6.dp))
}

@Composable
private fun AppWidgetUnavailable(
    title: String,
    detail: String?,
    onRetry: () -> Unit,
    modifier: Modifier,
) {
    Surface(
        modifier.fillMaxWidth(),
        shape = RoundedCornerShape(14.dp),
        color = Panel,
        border = BorderStroke(1.dp, BorderSoft),
    ) {
        Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                detail?.takeIf(String::isNotBlank)?.let {
                    Text(it, color = Muted, fontSize = 10.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
            }
            androidx.compose.material3.IconButton(onClick = onRetry) {
                Icon(Icons.Outlined.Refresh, "Retry app widget", tint = Coral)
            }
        }
    }
}

/** Compact ambient chips/badges for the existing shell top bar. */
@Composable
fun AppIndicatorChrome(indicators: List<AppMaterializedIndicator>, modifier: Modifier = Modifier) {
    if (indicators.isEmpty()) return
    Row(modifier, horizontalArrangement = Arrangement.spacedBy(5.dp), verticalAlignment = Alignment.CenterVertically) {
        indicators.take(2).forEach { indicator -> AppIndicatorChromeItem(indicator.model, indicator.title) }
        if (indicators.size > 2) Badge(containerColor = Control, contentColor = Ink) {
            Text("+${indicators.size - 2}", fontSize = 9.sp)
        }
    }
}

@Composable
private fun AppIndicatorChromeItem(model: AppIndicatorModel, title: String) {
    when (model.kind) {
        "badge" -> Badge(containerColor = Coral, contentColor = Color.White) {
            Text(model.count?.let { if (it > 999) "999+" else it.toString() }.orEmpty(), fontSize = 9.sp)
        }
        "chip", "state" -> Surface(shape = RoundedCornerShape(50), color = Coral.copy(alpha = .12f)) {
            Text(
                model.text ?: model.label ?: title,
                color = Coral,
                fontSize = 9.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 88.dp).padding(horizontal = 7.dp, vertical = 4.dp),
            )
        }
    }
}

/** Bounded cycle-safe hierarchy used by the native tree renderer. */
internal fun widgetTreeDepths(rows: List<AppWidgetRenderRow>, parentField: String?): Map<String, Int> {
    if (parentField == null) return rows.associate { it.recordId to 0 }
    val parents = rows.associate { row -> row.recordId to row.fields[parentField].appWidgetDisplay().takeIf { it != "—" } }
    return rows.associate { row ->
        val seen = mutableSetOf(row.recordId)
        var depth = 0
        var cursor = parents[row.recordId]
        while (cursor != null && cursor in parents && seen.add(cursor) && depth < 8) {
            depth += 1
            cursor = parents[cursor]
        }
        row.recordId to depth
    }
}
