package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.HttpPlanInspectorTransport
import ai.magicbeans.magdroid.tasks.PlanDecision
import ai.magicbeans.magdroid.tasks.PlanInspectorClient
import ai.magicbeans.magdroid.tasks.PlanInspectorPane
import ai.magicbeans.magdroid.tasks.PlanInspectorRoute
import ai.magicbeans.magdroid.tasks.PlanInspectorRules
import ai.magicbeans.magdroid.tasks.PlanInspectorState
import ai.magicbeans.magdroid.tasks.PlanInspectorTab
import ai.magicbeans.magdroid.tasks.PlanInspectorTransport
import ai.magicbeans.magdroid.tasks.PlanStatus
import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.WarningAmber
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/*
 * The Plan Inspector (design §7 of
 * `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`): a
 * full screen over a task's plan — Graph (default) · Steps · History, with
 * Analysis, Slots, Attempts, Clarifications, the plan document and Replan
 * behind More. Android port of iOS `PlanInspectorScreen.swift`; the rules and
 * requests are the bridge's `PlanInspectorState` / `PlanInspectorClient`.
 *
 * Opening it: wrap a screen in [PlanInspectorHost] and call
 * [LocalOpenPlanInspector] (or hold a route yourself and show
 * [PlanInspectorDialog]). It is a full-screen dialog window, so it covers the
 * app's bars wherever the caller sits.
 */

// Controller

/**
 * Bridges the plain [PlanInspectorState] into Compose: every action runs on
 * the main thread and bumps [revision] when it starts (flags set before the
 * first suspension, like `isSaving`, show at once) and when it ends. Views
 * call [read] so they recompose on every change.
 */
@Stable
class PlanInspectorController(val state: PlanInspectorState, private val scope: CoroutineScope) {
    var revision by mutableIntStateOf(0)
        private set

    fun bump() {
        revision++
    }

    /** The state, subscribing the calling composable to its changes. */
    fun read(): PlanInspectorState {
        @Suppress("UNUSED_VARIABLE") val touch = revision
        return state
    }

    fun run(block: suspend PlanInspectorState.() -> Unit): Job {
        val job = scope.launch(Dispatchers.Main.immediate) {
            try {
                state.block()
            } finally {
                bump()
            }
        }
        bump()
        return job
    }

    fun mutate(block: PlanInspectorState.() -> Unit) {
        state.block()
        bump()
    }
}

/** The app's transport, shared by every inspector (one HTTP client). */
private object PlanInspectorTransports {
    @Volatile private var shared: PlanInspectorTransport? = null

    fun get(context: Context): PlanInspectorTransport =
        shared ?: synchronized(this) {
            shared ?: HttpPlanInspectorTransport.forContext(context.applicationContext).also { shared = it }
        }
}

@Composable
fun rememberPlanInspectorController(
    route: PlanInspectorRoute,
    transport: PlanInspectorTransport? = null,
): PlanInspectorController {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    return remember(route) {
        val client = PlanInspectorClient(route.taskId, transport ?: PlanInspectorTransports.get(context))
        PlanInspectorController(PlanInspectorState(route, client), scope)
    }
}

// Entry

/**
 * Opens the Plan Inspector from anywhere under a [PlanInspectorHost]; null
 * where no host is installed.
 */
val LocalOpenPlanInspector = staticCompositionLocalOf<((PlanInspectorRoute) -> Unit)?> { null }

/**
 * Installs [LocalOpenPlanInspector] for [content] and shows the inspector
 * over it for whichever route was opened. Put it once around a screen tree.
 */
@Composable
fun PlanInspectorHost(content: @Composable () -> Unit) {
    var route by remember { mutableStateOf<PlanInspectorRoute?>(null) }
    val open = remember { { next: PlanInspectorRoute -> route = next } }
    CompositionLocalProvider(LocalOpenPlanInspector provides open) { content() }
    PlanInspectorDebugLauncher(open)
    route?.let { PlanInspectorDialog(it, onClose = { route = null }) }
}

/**
 * The inspector as a full-screen dialog for [route]. [transport] defaults to
 * the app's authorised HTTP transport (tests and fixtures pass their own).
 */
@Composable
fun PlanInspectorDialog(
    route: PlanInspectorRoute,
    onClose: () -> Unit,
    transport: PlanInspectorTransport? = null,
) {
    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(
            dismissOnBackPress = false,
            dismissOnClickOutside = false,
            usePlatformDefaultWidth = false,
            decorFitsSystemWindows = false,
        ),
    ) {
        val window = (LocalView.current.parent as? DialogWindowProvider)?.window
        val dark = activePalette.isDark
        // The dialog window owns its own bars: match the theme as the activity
        // does (`BindWindowTheme`). Applied after composition and again once
        // the window is showing, since showing it re-applies the dialog theme.
        SideEffect { window?.let { applyInspectorBars(it, dark) } }
        LaunchedEffect(window, dark) {
            delay(120)
            window?.let { applyInspectorBars(it, dark) }
        }
        val controller = rememberPlanInspectorController(route, transport ?: PlanInspectorFixtures.transportFor(route.taskId))
        PlanInspectorScreen(controller, onClose)
    }
}

private fun applyInspectorBars(window: android.view.Window, dark: Boolean) {
    // A dim-behind window makes the system draw light bar icons for the scrim.
    window.clearFlags(android.view.WindowManager.LayoutParams.FLAG_DIM_BEHIND)
    window.setDimAmount(0f)
    @Suppress("DEPRECATION")
    window.clearFlags(android.view.WindowManager.LayoutParams.FLAG_TRANSLUCENT_STATUS)
    window.addFlags(android.view.WindowManager.LayoutParams.FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS)
    @Suppress("DEPRECATION")
    window.statusBarColor = android.graphics.Color.TRANSPARENT
    @Suppress("DEPRECATION")
    window.navigationBarColor = android.graphics.Color.TRANSPARENT
    if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.Q) {
        window.isStatusBarContrastEnforced = false
        window.isNavigationBarContrastEnforced = false
    }
    WindowCompat.getInsetsController(window, window.decorView).apply {
        isAppearanceLightStatusBars = !dark
        isAppearanceLightNavigationBars = !dark
    }
}

// Screen

@Composable
fun PlanInspectorScreen(controller: PlanInspectorController, onClose: () -> Unit) {
    val state = controller.read()
    val palette = activePalette
    var selectedStep by remember { mutableStateOf<String?>(null) }
    var pane by remember { mutableStateOf<PlanInspectorPane?>(null) }
    var showDocument by remember { mutableStateOf(false) }
    var askLabel by remember { mutableStateOf(false) }
    var confirmDiscard by remember { mutableStateOf(false) }
    var confirmReject by remember { mutableStateOf(false) }
    var confirmReplan by remember { mutableStateOf(false) }

    // First load: the plan and its history, then the run's marks once.
    LaunchedEffect(controller) {
        val history = controller.run { loadVersions() }
        controller.run { load() }.join()
        controller.run { refreshLiveMarks() }
        history.join()
        PlanInspectorFixtures.launchState(state.route.taskId)?.let { launch ->
            when (launch) {
                "steps" -> controller.mutate { tab = PlanInspectorTab.Steps }
                "history" -> controller.mutate { tab = PlanInspectorTab.History }
                "history-preview" -> {
                    controller.mutate { tab = PlanInspectorTab.History }
                    state.versions.firstOrNull()?.let { first -> controller.run { previewVersion(first) } }
                }
                "steps-edit" -> controller.mutate { tab = PlanInspectorTab.Steps; beginEditing() }
                "step-sheet" -> {
                    controller.mutate { tab = PlanInspectorTab.Steps; beginEditing() }
                    selectedStep = state.graph?.steps?.getOrNull(2)?.id
                }
                "step-sheet-read" -> selectedStep = state.graph?.steps?.getOrNull(2)?.id
                "clarifications" -> pane = PlanInspectorPane.Clarifications
                "slots" -> pane = PlanInspectorPane.Slots
                "attempts" -> pane = PlanInspectorPane.Attempts
                "analysis" -> pane = PlanInspectorPane.Analysis
            }
        }
    }

    // Live run marks: every 4 s while the run is active and the screen resumed.
    val runActive = state.runActive
    val lifecycleOwner = LocalLifecycleOwner.current
    LaunchedEffect(runActive, lifecycleOwner) {
        if (!runActive) return@LaunchedEffect
        lifecycleOwner.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                delay(PlanInspectorRules.LIVE_POLL_SECONDS * 1000L)
                controller.run { refreshLiveMarks() }.join()
            }
        }
    }

    val notice = state.notice
    LaunchedEffect(notice) {
        if (notice == null) return@LaunchedEffect
        delay(3_000)
        if (state.notice == notice) controller.mutate { this.notice = null }
    }

    BackHandler {
        when {
            showDocument -> showDocument = false
            pane != null -> pane = null
            state.isEditing -> if (state.isDirty) confirmDiscard = true else controller.mutate { discardEdits() }
            state.preview != null -> controller.mutate { clearPreview() }
            else -> onClose()
        }
    }

    Box(Modifier.fillMaxSize()) {
    Column(Modifier.fillMaxSize().themeWash().navigationBarsPadding()) {
        InspectorTopBar(
            controller = controller,
            onClose = onClose,
            onDone = { if (state.isDirty) confirmDiscard = true else controller.mutate { discardEdits() } },
            onSave = { askLabel = true },
            onPane = { pane = it },
            onDocument = { showDocument = true },
            onReplan = { confirmReplan = true },
        )
        InspectorHeader(controller)
        PlanSegmented(
            selected = state.tab,
            onSelect = { tab -> controller.mutate { this.tab = tab } },
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        InspectorBanners(controller)
        Box(Modifier.weight(1f).fillMaxWidth()) {
            InspectorContent(
                controller = controller,
                onSelectStep = { selectedStep = it },
                onAnswer = { pane = PlanInspectorPane.Clarifications },
            )
        }
        if (state.showsApprovalBar && state.preview == null) {
            ApprovalBar(controller, onReject = { confirmReject = true })
        }
    }

    selectedStep?.let { id -> PlanStepSheet(controller, id, onDismiss = { selectedStep = null }) }

    pane?.let { current ->
        PlanPaneScreen(controller, current, onBack = { pane = null })
    }
    if (showDocument) {
        PlanDocumentScreen(state.route.planDocument.orEmpty(), onBack = { showDocument = false })
    }
    }

    if (askLabel) SaveLabelDialog(
        saving = state.isSaving,
        onCancel = { askLabel = false },
        onSave = { label ->
            askLabel = false
            controller.run { save(label) }
        },
    )
    if (confirmDiscard) ConfirmDialog(
        title = "Discard your changes?",
        message = "Your edits to this plan have not been saved.",
        confirm = "Discard changes",
        dismiss = "Keep editing",
        onConfirm = { confirmDiscard = false; controller.mutate { discardEdits() } },
        onDismiss = { confirmDiscard = false },
    )
    if (confirmReject) ConfirmDialog(
        title = "Reject this plan?",
        message = "A rejected plan will not run.",
        confirm = "Reject plan",
        onConfirm = { confirmReject = false; controller.run { decide(PlanDecision.Reject) } },
        onDismiss = { confirmReject = false },
    )
    if (confirmReplan) ConfirmDialog(
        title = "Plan this task again?",
        message = "The planner starts over; the current plan stays in History.",
        confirm = "Replan",
        onConfirm = { confirmReplan = false; controller.run { replan() } },
        onDismiss = { confirmReplan = false },
    )
}

@Composable
private fun InspectorTopBar(
    controller: PlanInspectorController,
    onClose: () -> Unit,
    onDone: () -> Unit,
    onSave: () -> Unit,
    onPane: (PlanInspectorPane) -> Unit,
    onDocument: () -> Unit,
    onReplan: () -> Unit,
) {
    val state = controller.read()
    val palette = activePalette
    var menu by remember { mutableStateOf(false) }
    Row(
        Modifier
            .fillMaxWidth()
            .background(palette.background)
            .statusBarsPadding()
            .heightIn(min = 52.dp)
            .padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (state.isEditing) {
            BarTextButton("Done", palette.accent, onClick = onDone)
            Text(
                "Editing plan", style = planBodyStyle(17.sp, FontWeight.SemiBold),
                textAlign = TextAlign.Center, modifier = Modifier.weight(1f),
            )
            BarTextButton(
                if (state.isSaving) "Saving…" else "Save",
                palette.accent, enabled = state.isDirty && !state.isSaving, bold = true, onClick = onSave,
            )
        } else {
            BarTextButton("Close", palette.accent, onClick = onClose)
            Text(
                "Plan", style = planBodyStyle(17.sp, FontWeight.SemiBold),
                modifier = Modifier.weight(1f).padding(start = 4.dp),
            )
            IconButton(onClick = { controller.run { refresh() } }, enabled = !state.isLoading) {
                Icon(Icons.Outlined.Refresh, "Refresh", tint = palette.accent)
            }
            if (state.canEdit && state.tab != PlanInspectorTab.History) {
                BarTextButton("Edit", palette.accent) { controller.mutate { beginEditing() } }
            }
            Box {
                IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreHoriz, "More", tint = palette.accent) }
                DropdownMenu(menu, onDismissRequest = { menu = false }) {
                    for (item in PlanInspectorPane.entries) {
                        val count = state.pendingQuestions.size
                        val label = if (item == PlanInspectorPane.Clarifications && count > 0) "${item.title} ($count)" else item.title
                        DropdownMenuItem(text = { Text(label) }, onClick = { menu = false; onPane(item) })
                    }
                    if (!state.route.planDocument.isNullOrEmpty()) {
                        DropdownMenuItem(text = { Text("Plan document") }, onClick = { menu = false; onDocument() })
                    }
                    if (state.canReplan) {
                        HorizontalDivider()
                        DropdownMenuItem(
                            text = { Text("Replan", color = palette.danger) },
                            onClick = { menu = false; onReplan() },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun BarTextButton(text: String, color: Color, enabled: Boolean = true, bold: Boolean = false, onClick: () -> Unit) {
    Box(
        Modifier
            .heightIn(min = Lively.tapMin)
            .widthIn(min = Lively.tapMin)
            .clip(RoundedCornerShape(10.dp))
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 12.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text,
            style = planBodyStyle(16.sp, if (bold) FontWeight.SemiBold else FontWeight.Medium, if (enabled) color else activePalette.secondaryText.copy(alpha = 0.6f)),
        )
    }
}

@Composable
private fun InspectorHeader(controller: PlanInspectorController) {
    val state = controller.read()
    val palette = activePalette
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        state.route.title?.takeIf { it.isNotBlank() }?.let {
            Text(it, style = planBodyStyle(15.sp, FontWeight.SemiBold), maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        val summary = state.shownGraph?.summary
        if (summary != null) {
            Row(
                Modifier.horizontalScroll(rememberScrollState()).semantics(mergeDescendants = true) {},
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                state.status?.let { PlanTag(it.label, statusColor(it)) }
                PlanTag(PlanStyle.plural(summary.steps, "step"), palette.info)
                PlanTag(PlanStyle.plural(summary.edges, "edge"))
                PlanTag("${summary.unresolved} unresolved", if (summary.unresolved > 0) palette.warning else palette.success)
                PlanTag("Confidence ${PlanStyle.percent(summary.confidence)}", mono = true)
                summary.strategy?.let { PlanTag(it) }
            }
        } else if (state.status != null) {
            PlanTag(state.status!!.label, statusColor(state.status!!))
        }
    }
}

internal fun statusColor(status: PlanStatus, palette: Palette = activePalette): Color = when (status) {
    PlanStatus.Approved -> palette.success
    PlanStatus.Draft, PlanStatus.Eliciting -> palette.warning
    PlanStatus.Planning -> palette.running
    PlanStatus.Rejected, PlanStatus.Failed -> palette.danger
}

/** Graph · Steps · History: compact rounded-square segments in a 48 dp hit row. */
@Composable
internal fun PlanSegmented(selected: PlanInspectorTab, onSelect: (PlanInspectorTab) -> Unit, modifier: Modifier = Modifier) {
    val palette = activePalette
    Box(modifier.fillMaxWidth().height(Lively.tapMin), contentAlignment = Alignment.Center) {
        Box(
            Modifier
                .fillMaxWidth()
                .height(36.dp)
                .background(palette.soft, RoundedCornerShape(10.dp)),
        )
        Row(Modifier.fillMaxWidth().padding(horizontal = 2.dp)) {
            for (tab in PlanInspectorTab.entries) {
                val on = tab == selected
                Box(
                    Modifier
                        .weight(1f)
                        .height(Lively.tapMin)
                        .clickable(role = Role.Tab, onClickLabel = tab.title) { onSelect(tab) }
                        .semantics { this.selected = on },
                    contentAlignment = Alignment.Center,
                ) {
                    Box(
                        Modifier
                            .fillMaxWidth()
                            .height(32.dp)
                            .then(
                                if (on) Modifier
                                    .background(palette.card, RoundedCornerShape(8.dp))
                                    .border(0.5.dp, palette.cardBorder, RoundedCornerShape(8.dp))
                                else Modifier,
                            ),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            tab.title,
                            style = planBodyStyle(13.sp, if (on) FontWeight.SemiBold else FontWeight.Medium, if (on) palette.text else palette.secondaryText),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun InspectorBanners(controller: PlanInspectorController) {
    val state = controller.read()
    val palette = activePalette
    val error = state.errorMessage
    if (error != null && state.record != null) Banner(error, palette.danger, Icons.Outlined.WarningAmber)
    state.notice?.let { Banner(it, palette.success, Icons.Outlined.CheckCircle) }
    if (state.isEditing) {
        Banner(
            if (state.isDirty) "Unsaved changes — tap Save to keep them." else "Editing. Tap a step to change it.",
            palette.accent, Icons.Outlined.Edit,
        )
    }
}

@Composable
private fun Banner(text: String, color: Color, icon: ImageVector) {
    Row(
        Modifier
            .padding(horizontal = 16.dp, vertical = 3.dp)
            .fillMaxWidth()
            .background(color.copy(alpha = 0.1f), RoundedCornerShape(Lively.chipRadius + 2.dp))
            .padding(horizontal = 12.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Icon(icon, null, tint = color, modifier = Modifier.size(16.dp))
        Text(text, style = planBodyStyle(12.sp, FontWeight.Medium, color))
    }
}

@Composable
private fun InspectorContent(controller: PlanInspectorController, onSelectStep: (String) -> Unit, onAnswer: () -> Unit) {
    val state = controller.read()
    val graph = state.shownGraph
    when {
        state.tab == PlanInspectorTab.History -> PlanHistoryView(controller)
        graph != null && (graph.steps.isNotEmpty() || state.isEditing) -> when (state.tab) {
            PlanInspectorTab.Graph -> Column(Modifier.fillMaxSize()) {
                PlanGraphCanvas(graph, state.runMarks, onSelectStep, Modifier.weight(1f))
                PlanGraphLegend()
            }
            else -> if (state.isEditing) {
                PlanEditList(
                    graph = graph,
                    onSelect = onSelectStep,
                    onMove = { from, to -> controller.mutate { editDraft { it.moveSteps(setOf(from), to) } } },
                    onDelete = { id -> controller.mutate { editDraft { it.removeStep(id) } } },
                    onAdd = {
                        var id: String? = null
                        controller.mutate { id = addStep() }
                        id?.let(onSelectStep)
                    },
                )
            } else {
                PlanWaterfall(graph, state.runMarks, onSelectStep)
            }
        }
        !state.hasLoaded || (state.isLoading && state.record == null) -> Column(
            Modifier.fillMaxSize(), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            CircularProgressIndicator(color = activePalette.accent)
            Spacer(Modifier.height(10.dp))
            Text("Loading plan…", style = planBodyStyle(13.sp, color = activePalette.secondaryText))
        }
        state.errorMessage != null && state.record == null ->
            Unavailable(controller, "Plan unavailable", state.errorMessage.orEmpty(), onAnswer)
        else -> Unavailable(controller, "No graph-backed plan yet", emptyDetail(state), onAnswer)
    }
}

private fun emptyDetail(state: PlanInspectorState): String = when (state.status) {
    PlanStatus.Planning -> "The planner is still working. This fills in when the plan is drafted."
    PlanStatus.Eliciting -> "The planner is waiting on answers — see Clarifications."
    PlanStatus.Failed -> state.record?.error ?: "Planning failed."
    else -> "This task has no plan graph to inspect."
}

@Composable
private fun Unavailable(controller: PlanInspectorController, title: String, detail: String, onAnswer: () -> Unit) {
    val state = controller.read()
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(title, style = planBodyStyle(17.sp, FontWeight.SemiBold), textAlign = TextAlign.Center)
        Spacer(Modifier.height(6.dp))
        Text(detail, style = planBodyStyle(13.sp, color = activePalette.secondaryText), textAlign = TextAlign.Center)
        Spacer(Modifier.height(14.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            val count = state.pendingQuestions.size
            if (count > 0) {
                LivelyChip(onClick = onAnswer, prominent = true) {
                    Text("Answer ${PlanStyle.plural(count, "question")}", style = livelyChipTextStyle(prominent = true))
                }
            }
            LivelyChip(onClick = { controller.run { refresh() } }) { Text("Refresh", style = livelyChipTextStyle()) }
        }
    }
}

/** Reject (confirmed) · Approve · Approve & Run: rounded-square buttons. */
@Composable
private fun ApprovalBar(controller: PlanInspectorController, onReject: () -> Unit) {
    val state = controller.read()
    val palette = activePalette
    val busy = state.deciding != null
    Row(
        Modifier
            .fillMaxWidth()
            .background(palette.background.copy(alpha = 0.92f))
            .padding(horizontal = 16.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        DecisionButton("Reject", palette.danger, filled = false, enabled = !busy, modifier = Modifier.weight(0.85f), onClick = onReject)
        DecisionButton(
            if (state.deciding == PlanDecision.Approve) "Approving…" else "Approve",
            palette.accent, filled = false, enabled = !busy, modifier = Modifier.weight(0.95f),
        ) { controller.run { decide(PlanDecision.Approve) } }
        DecisionButton(
            if (state.deciding == PlanDecision.ApproveAndRun) "Starting…" else "Approve & Run",
            palette.accent, filled = true, enabled = !busy, modifier = Modifier.weight(1.3f),
        ) { controller.run { decide(PlanDecision.ApproveAndRun) } }
    }
}

@Composable
internal fun DecisionButton(
    text: String,
    color: Color,
    filled: Boolean,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    val palette = activePalette
    val shape = RoundedCornerShape(10.dp)
    val foreground = if (filled) palette.foregroundFor(color) else color
    Box(
        modifier
            .heightIn(min = Lively.tapMin)
            .clip(shape)
            .background(if (filled) palette.fillFor(color) else color.copy(alpha = 0.14f))
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 6.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text,
            style = planBodyStyle(13.5.sp, FontWeight.SemiBold, foreground)
                .copy(color = foreground.copy(alpha = if (enabled) 1f else 0.5f)),
            maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun SaveLabelDialog(saving: Boolean, onCancel: () -> Unit, onSave: (String) -> Unit) {
    var label by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text("Save plan") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(
                    "Saving keeps the current plan as a restorable version, then writes your changes.",
                    style = planBodyStyle(13.sp, color = activePalette.secondaryText),
                )
                MagicianTextField(
                    value = label, onValueChange = { label = it }, singleLine = true,
                    label = { Text("Label (optional)") }, modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = { TextButton(onClick = { onSave(label) }, enabled = !saving) { Text("Save") } },
        dismissButton = { TextButton(onClick = onCancel) { Text("Cancel") } },
        shape = RoundedCornerShape(16.dp),
    )
}

@Composable
internal fun ConfirmDialog(
    title: String,
    message: String,
    confirm: String,
    dismiss: String = "Cancel",
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = { Text(message) },
        confirmButton = { TextButton(onClick = onConfirm) { Text(confirm, color = activePalette.danger) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text(dismiss) } },
        shape = RoundedCornerShape(16.dp),
    )
}

/** A full-screen sub-page inside the inspector (panes, the plan document). */
@Composable
internal fun PlanSubPage(title: String, onBack: () -> Unit, content: @Composable () -> Unit) {
    val palette = activePalette
    BackHandler(onBack = onBack)
    Column(Modifier.fillMaxSize().themeWash().navigationBarsPadding()) {
        Row(
            Modifier
                .fillMaxWidth()
                .background(palette.background)
                .statusBarsPadding()
                .heightIn(min = 52.dp)
                .padding(horizontal = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back", tint = palette.text)
            }
            Text(title, style = planBodyStyle(17.sp, FontWeight.SemiBold), modifier = Modifier.weight(1f))
        }
        Box(Modifier.weight(1f).fillMaxWidth()) { content() }
    }
}
