package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.thinking.OutlineRow
import ai.magicbeans.magdroid.thinking.ThinkingMap
import ai.magicbeans.magdroid.thinking.ThinkingMapSummary
import ai.magicbeans.magdroid.thinking.ThinkingMapViewModel
import ai.magicbeans.magdroid.thinking.ThinkingNodeKind
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.hypot
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

/**
 * Maps the owner has been thinking in.
 *
 * Two ways to read one, as iOS has. Focus walks the map a node at a time —
 * ancestry above, branches below, connections sideways — which is how a map is
 * navigated and how somebody thinks through one. Outline reads the whole
 * structure at once, which is Android's addition and what a phone is good at.
 *
 * There is no canvas to port: iOS's map view is `ScrollView` and cards, its
 * nodes carry no coordinates, and the structure lives entirely in parent, child
 * and edge. Laying one out here would invent a geometry neither client has.
 *
 * The map is live: it follows `ThinkingMapUpdated` on the realtime bus, so an
 * agent writing to it in the background appears here without a refresh. Push is
 * an accelerator rather than the correctness path — a notice carries no payload
 * and the map is always re-read from the server.
 */
@Composable
fun ThinkingMapScreen(
    onClose: () -> Unit,
    /** A thought to start a new map with, from the `@brainstorm` lane. */
    seed: String? = null,
    onSeedTaken: () -> Unit = {},
    /**
     * Whether this screen draws its own way out.
     *
     * True over Observe, where it is an overlay with no other exit. False when
     * the shell hosts it as a drawer destination and has already put a back
     * arrow in the app bar — two controls doing one job, side by side, reads as
     * two different jobs.
     */
    ownsDismiss: Boolean = true,
) {
    val viewModel: ThinkingMapViewModel = viewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = androidx.compose.ui.platform.LocalContext.current

    // Taken once. An empty seed is meaningful — a bare `@brainstorm` opens a
    // fresh map with nothing in it — so the trigger is nullness, not blankness.
    LaunchedEffect(seed) {
        val thought = seed ?: return@LaunchedEffect
        viewModel.startMap(thought)
        onSeedTaken()
    }

    state.open?.let { open ->
        MapReader(
            map = open,
            state = state,
            onBack = viewModel::close,
            onFocus = viewModel::focusNode,
            onReading = viewModel::setReading,
            onCapture = { viewModel.addThought(it) },
            onInterpret = viewModel::interpret,
            onAccept = viewModel::acceptSuggestion,
            onReject = viewModel::rejectSuggestion,
            onDelete = viewModel::deleteNode,
            onRename = viewModel::rename,
            onArchive = {
                viewModel.setLifecycle(ai.magicbeans.magdroid.thinking.MapLifecycle.Archived)
            },
            onConsolidate = viewModel::consolidate,
            onExport = { viewModel.export { markdown -> shareText(context, markdown) } },
            onReplay = viewModel::replay,
            onLeaveReplay = viewModel::leaveReplay,
            onRestore = { viewModel.restoreReplay("") },
            onRetry = viewModel::retryInterpret,
            onDismissFallback = viewModel::dismissFallback,
            onAnswerClarification = viewModel::answerClarification,
            onDeferClarification = viewModel::deferClarification,
            onDecideProposal = viewModel::decideProposal,
            onDuplicate = { viewModel.duplicateMap(open.id) },
            onDeleteMap = { viewModel.deleteMap(open.id) },
            onDisconnect = { other ->
                state.focusedNodeId?.let { viewModel.disconnectNodes(it, other) }
            },
        )
        return
    }

    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Thinking maps", color = Ink, fontSize = 18.sp, fontWeight = FontWeight.Bold)
            Spacer(Modifier.weight(1f))
            if (ownsDismiss) TextButton(onClick = onClose) { Text("Close", color = Coral) }
        }

        MagicianTextField(
            value = state.query,
            onValueChange = viewModel::search,
            singleLine = true,
            placeholder = { Text("Search maps", color = Muted, fontSize = 13.sp) },
            textStyle = LocalTextStyle.current.copy(color = Ink, fontSize = 13.sp),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        )

        when {
            state.loading && state.maps.isEmpty() -> Box(
                Modifier.fillMaxSize(), Alignment.Center,
            ) { CircularProgressIndicator(color = Coral) }

            // A failed read is not an absence of maps. "No maps yet." after a
            // 502 tells the owner their thinking is gone when it is merely
            // unreachable.
            state.maps.isEmpty() && state.failure != null -> FailurePane(
                state.failure!!,
                onRetry = viewModel::refresh,
            )

            state.visible.isEmpty() -> Column(
                Modifier.fillMaxSize().padding(32.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                Text(
                    if (state.query.isBlank()) "No maps yet." else "Nothing matches that.",
                    color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
                )
                Spacer(Modifier.height(6.dp))
                Text(
                    "Maps you think in on the desktop show up here.",
                    color = Muted, fontSize = 12.sp,
                )
            }

            else -> LazyColumn(
                Modifier.fillMaxSize(),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                // Maps already listed stay readable; the bar says the newest
                // ones may be missing, and offers the way to ask again.
                state.failure?.let { problem ->
                    item(key = "maps-failure") {
                        FailureBanner(problem, onRetry = viewModel::refresh)
                    }
                }
                items(state.visible, key = { it.id }) { map ->
                    MapRow(map) { viewModel.open(map) }
                }
                state.error?.let { item { Text(it, color = Coral, fontSize = 12.sp) } }
            }
        }
    }
}

@Composable
private fun MapRow(map: ThinkingMapSummary, onClick: () -> Unit) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth().clickable { onClick() },
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                map.displayTitle,
                color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Medium,
            )
            // From the server's bounded preview. Absent on a map with no live
            // nodes, and then not invented — repeating the title underneath
            // itself would say nothing.
            map.previewThought?.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Secondary, fontSize = 12.sp, maxLines = 2)
            }
            if (map.previewCount > 0) {
                Text("${map.previewCount} shown", color = Muted, fontSize = 11.sp)
            }
        }
    }
}

/**
 * One map, read either way.
 *
 * Focus walks it a node at a time — ancestry above, branches below, connections
 * sideways — which is how iOS navigates a map and how somebody actually thinks
 * through one. Outline reads the whole structure at once, which a phone is also
 * good for and a canvas is not.
 */
@Composable
private fun MapReader(
    map: ThinkingMap,
    state: ai.magicbeans.magdroid.thinking.ThinkingMapUiState,
    onBack: () -> Unit,
    onFocus: (String) -> Unit,
    onReading: (ai.magicbeans.magdroid.thinking.ReadingMode) -> Unit,
    onCapture: (String) -> Unit,
    onInterpret: (String, ai.magicbeans.magdroid.thinking.FrontierIntent) -> Unit,
    onAccept: (String) -> Unit,
    onReject: (String) -> Unit,
    onDelete: (String) -> Unit,
    onRename: (String) -> Unit,
    onArchive: () -> Unit,
    onConsolidate: () -> Unit,
    onExport: () -> Unit,
    onReplay: (Long) -> Unit,
    onLeaveReplay: () -> Unit,
    onRestore: () -> Unit,
    onRetry: () -> Unit,
    onDismissFallback: () -> Unit,
    onAnswerClarification: (String, String) -> Unit,
    onDeferClarification: (String) -> Unit,
    onDecideProposal: (String, ai.magicbeans.magdroid.thinking.ProposalDecision) -> Unit,
    onDuplicate: () -> Unit,
    onDeleteMap: () -> Unit,
    onDisconnect: (String) -> Unit,
) {
    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(onClick = onBack) { Text("Maps", color = Coral) }
            Text(
                map.title,
                color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f).padding(start = 4.dp),
            )
            if (state.opening || state.saving) {
                CircularProgressIndicator(Modifier.size(14.dp), color = Coral, strokeWidth = 2.dp)
            }
            MapMenu(
                replaying = state.isReplaying,
                onRename = onRename,
                onArchive = onArchive,
                onConsolidate = onConsolidate,
                onExport = onExport,
                onReplay = onReplay,
                onLeaveReplay = onLeaveReplay,
                onRestore = onRestore,
                onDuplicate = onDuplicate,
                onDelete = onDeleteMap,
            )
        }

        // Browsing history is a different thing from reading the map, and the
        // difference has to be visible — a canvas that silently shows an old
        // shape is worse than not offering history at all.
        if (state.isReplaying) {
            Surface(
                color = Coral.copy(alpha = 0.12f),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(
                    "Showing an earlier version. Nothing here can be edited.",
                    color = Coral, fontSize = 11.sp,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
                )
            }
        }

        // Above the tabs on purpose: a question that is holding the map up
        // belongs in front of the reader whichever way they are reading it, not
        // filed behind a mode they might not open.
        if (!state.isReplaying) {
            PendingWork(
                clarifications = map.openClarifications,
                proposals = map.openProposals,
                onAnswer = onAnswerClarification,
                onDefer = onDeferClarification,
                onDecide = onDecideProposal,
            )
        }

        TabRow(
            selectedTabIndex = ai.magicbeans.magdroid.thinking.ReadingMode.entries
                .indexOf(state.reading),
            containerColor = Ground,
            contentColor = Coral,
            divider = {},
        ) {
            ai.magicbeans.magdroid.thinking.ReadingMode.entries.forEach { mode ->
                Tab(
                    selected = mode == state.reading,
                    onClick = { onReading(mode) },
                    text = {
                        Text(
                            mode.label,
                            fontSize = 13.sp,
                            color = if (mode == state.reading) Coral else Secondary,
                        )
                    },
                )
            }
        }

        Box(Modifier.weight(1f)) {
            when (state.reading) {
                ai.magicbeans.magdroid.thinking.ReadingMode.Canvas ->
                    MapCanvas(state.graph, state.showing?.activeNodeId, onFocus)
                ai.magicbeans.magdroid.thinking.ReadingMode.Focus ->
                    MapFocus(state.focus, onFocus, onAccept, onReject, onDelete, onDisconnect)
                ai.magicbeans.magdroid.thinking.ReadingMode.Outline ->
                    OutlineList(state.outline)
            }
        }

        IntelligenceStrip(
            progress = state.progress,
            nodeCount = state.progressNodeCount,
            fallback = state.fallback,
            onRetry = onRetry,
            onDismiss = onDismissFallback,
        )
        state.saveError?.let {
            Text(
                it, color = Coral, fontSize = 11.sp,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
        if (!state.isReplaying) CaptureBar(
            busy = state.saving,
            onCapture = onCapture,
            onInterpret = onInterpret,
        )
    }
}

/**
 * Capture a thought, or ask the interpreter to carry it further.
 *
 * Both take the same text, because they are the same act with different
 * intent: one records what the owner thinks, the other asks Magician to think
 * from there. The frontier verbs are iOS's own — "continue thinking" and
 * "break this open".
 */
@Composable
private fun CaptureBar(
    busy: Boolean,
    onCapture: (String) -> Unit,
    onInterpret: (String, ai.magicbeans.magdroid.thinking.FrontierIntent) -> Unit,
) {
    var text by remember { mutableStateOf("") }
    Column(
        Modifier.fillMaxWidth().padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        MagicianTextField(
            value = text,
            onValueChange = { text = it },
            enabled = !busy,
            placeholder = { Text("Add a thought", color = Muted, fontSize = 13.sp) },
            textStyle = LocalTextStyle.current.copy(color = Ink, fontSize = 13.sp),
            maxLines = 3,
            modifier = Modifier.fillMaxWidth(),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            MapAction("Capture", enabled = !busy && text.isNotBlank()) {
                onCapture(text); text = ""
            }
            // Works with an empty box: continuing from where the map already is
            // needs no new words from the owner.
            MapAction("Continue thinking", enabled = !busy) {
                onInterpret(
                    text.ifBlank { "continue" },
                    ai.magicbeans.magdroid.thinking.FrontierIntent.ContinueThinking,
                )
                text = ""
            }
            MapAction("Break open", enabled = !busy) {
                onInterpret(
                    text.ifBlank { "break this open" },
                    ai.magicbeans.magdroid.thinking.FrontierIntent.BreakOpen,
                )
                text = ""
            }
        }
    }
}

@Composable
private fun MapAction(label: String, enabled: Boolean, onClick: () -> Unit) {
    Surface(
        color = if (enabled) Coral.copy(alpha = 0.14f) else Ground,
        shape = RoundedCornerShape(6.dp),
        modifier = Modifier.then(if (enabled) Modifier.clickable { onClick() } else Modifier),
    ) {
        Text(
            label,
            color = if (enabled) Coral else Muted,
            fontSize = 12.sp, fontWeight = FontWeight.Medium,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 7.dp),
        )
    }
}

/**
 * The focused node, with the ways out of it.
 *
 * Headings say what the reader is looking at rather than naming the data:
 * "you are exploring" and "choose where to go next" are iOS's, and they are
 * what make a graph feel navigable instead of listed.
 */
/**
 * The map drawn.
 *
 * Depth runs left to right, position within a depth runs top to bottom, and
 * branch edges are drawn as beziers through the midpoint — the layout iOS
 * computes, ported so the same map is the same shape on both.
 *
 * Tapping a node focuses it, which is what makes the drawing useful on a phone
 * rather than decorative: you see the shape, then go to the part of it you
 * meant.
 */
/**
 * Everything that acts on the map rather than on one node.
 *
 * A menu because these are rare and destructive-ish — archiving and
 * consolidating both change what the library shows — and putting them beside
 * Capture would make the common action share a row with the ones worth
 * hesitating over.
 */
@OptIn(ExperimentalMaterial3Api::class)
/**
 * What the facilitator is doing, or why it produced nothing.
 *
 * Absent while idle and unfailed, so it never occupies a strip of a small
 * screen to say "ready". Retry appears only when the failure was one worth
 * repeating — a rejected request will be rejected identically, and a button
 * that cannot work is the promise this client keeps taking out.
 */
@Composable
private fun IntelligenceStrip(
    progress: ai.magicbeans.magdroid.thinking.ThinkingProgress,
    nodeCount: Int?,
    fallback: ai.magicbeans.magdroid.thinking.ThinkingFallback?,
    onRetry: () -> Unit,
    onDismiss: () -> Unit,
) {
    // Any working stage shows, not only Facilitating: the server narrates
    // preparing → loading_context → facilitating → parsing → shaping, and a
    // strip that only knew one of them would sit silent for the rest.
    val thinking = progress != ai.magicbeans.magdroid.thinking.ThinkingProgress.Idle
    if (!thinking && fallback == null) return

    Surface(
        color = if (fallback != null) Danger.copy(alpha = 0.10f) else Coral.copy(alpha = 0.10f),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (thinking) {
                CircularProgressIndicator(Modifier.size(13.dp), color = Coral, strokeWidth = 2.dp)
            }
            Column(Modifier.weight(1f)) {
                Text(
                    if (fallback != null) fallback.statusLabel else progress.label,
                    color = if (fallback != null) Danger else Coral,
                    fontSize = 10.sp, fontWeight = FontWeight.SemiBold,
                )
                Text(
                    fallback?.message ?: progress.detail(nodeCount),
                    color = Secondary, fontSize = 11.sp, lineHeight = 15.sp,
                )
            }
            if (fallback != null) {
                if (fallback.canRetry) {
                    TextButton(onClick = onRetry) { Text("Retry", color = Coral, fontSize = 12.sp) }
                }
                TextButton(onClick = onDismiss) { Text("Dismiss", color = Muted, fontSize = 12.sp) }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MapMenu(
    replaying: Boolean,
    onRename: (String) -> Unit,
    onArchive: () -> Unit,
    onConsolidate: () -> Unit,
    onExport: () -> Unit,
    onReplay: (Long) -> Unit,
    onLeaveReplay: () -> Unit,
    onRestore: () -> Unit,
    onDuplicate: () -> Unit,
    onDelete: () -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var newTitle by remember { mutableStateOf("") }
    var confirmingDelete by remember { mutableStateOf(false) }

    Box {
        TextButton(onClick = { open = true }) { Text("⋯", color = Coral, fontSize = 18.sp) }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            if (replaying) {
                // While reading history the only sensible verbs are about
                // history: everything else would edit a version that is not
                // the current one.
                DropdownMenuItem(
                    text = { Text("Back to now") },
                    onClick = { open = false; onLeaveReplay() },
                )
                DropdownMenuItem(
                    text = { Text("Restore as a new map") },
                    onClick = { open = false; onRestore() },
                )
            } else {
                DropdownMenuItem(
                    text = { Text("Rename") },
                    onClick = { open = false; renaming = true },
                )
                DropdownMenuItem(
                    text = { Text("Tidy up") },
                    onClick = { open = false; onConsolidate() },
                )
                DropdownMenuItem(
                    text = { Text("Export as markdown") },
                    onClick = { open = false; onExport() },
                )
                DropdownMenuItem(
                    text = { Text("Earlier versions") },
                    // Sequence 0 is the beginning; the reader steps forward
                    // from there rather than being asked for a number.
                    onClick = { open = false; onReplay(0) },
                )
                DropdownMenuItem(
                    text = { Text("Duplicate") },
                    onClick = { open = false; onDuplicate() },
                )
                DropdownMenuItem(
                    text = { Text("Archive") },
                    onClick = { open = false; onArchive() },
                )
                // Archive is reversible and comes first for that reason; this
                // is the server's permanent delete, and its endpoint went
                // uncalled by this client — a map could be put away here but
                // never actually got rid of.
                DropdownMenuItem(
                    text = { Text("Delete", color = Coral) },
                    onClick = { open = false; confirmingDelete = true },
                )
            }
        }
    }

    if (confirmingDelete) {
        AlertDialog(
            onDismissRequest = { confirmingDelete = false },
            title = { Text("Delete this map?", color = Ink) },
            text = {
                Text(
                    "The map and everything thought in it go for good. " +
                        "Archive keeps it and takes it out of the way.",
                    color = Secondary,
                )
            },
            confirmButton = {
                TextButton(onClick = { confirmingDelete = false; onDelete() }) {
                    Text("Delete", color = Coral)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmingDelete = false }) {
                    Text("Keep", color = Secondary)
                }
            },
        )
    }

    if (renaming) {
        AlertDialog(
            onDismissRequest = { renaming = false },
            title = { Text("Rename map", color = Ink) },
            text = {
                MagicianTextField(
                    value = newTitle,
                    onValueChange = { newTitle = it },
                    singleLine = true,
                    textStyle = LocalTextStyle.current.copy(color = Ink),
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    renaming = false
                    onRename(newTitle)
                    newTitle = ""
                }) { Text("Rename", color = Coral) }
            },
            dismissButton = {
                TextButton(onClick = { renaming = false }) { Text("Cancel", color = Muted) }
            },
            containerColor = Panel,
        )
    }
}

/** Hand markdown to whatever the owner shares with. */
private fun shareText(context: android.content.Context, text: String) {
    runCatching {
        context.startActivity(
            android.content.Intent.createChooser(
                android.content.Intent(android.content.Intent.ACTION_SEND).apply {
                    type = "text/plain"
                    putExtra(android.content.Intent.EXTRA_TEXT, text)
                },
                "Share map",
            ).addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }
}

@Composable
private fun MapCanvas(
    layout: ai.magicbeans.magdroid.thinking.GraphLayout,
    activeNodeId: String?,
    onFocus: (String) -> Unit,
) {
    if (layout.points.isEmpty()) {
        Box(Modifier.fillMaxSize().padding(32.dp), Alignment.Center) {
            Text("This map is empty.", color = Muted, fontSize = 13.sp)
        }
        return
    }

    val edgeColor = Secondary.copy(alpha = 0.24f)
    val accent = Coral
    val plain = Secondary

    BoxWithConstraints(Modifier.fillMaxSize().padding(20.dp)) {
        val widthPx = with(LocalDensity.current) { maxWidth.toPx() }
        val heightPx = with(LocalDensity.current) { maxHeight.toPx() }

        Canvas(
            Modifier
                .fillMaxSize()
                .pointerInput(layout) {
                    detectTapGestures { tap ->
                        // Nearest node within a finger's reach. A canvas that
                        // only responds to an exact hit is a canvas nobody can
                        // use on a phone.
                        layout.points
                            .map { point ->
                                point to hypot(
                                    point.x * widthPx - tap.x,
                                    point.y * heightPx - tap.y,
                                )
                            }
                            .minByOrNull { it.second }
                            ?.takeIf { it.second < 64f }
                            ?.let { onFocus(it.first.node.id) }
                    }
                },
        ) {
            layout.links.forEach { link ->
                val from = Offset(link.from.x * size.width, link.from.y * size.height)
                val to = Offset(link.to.x * size.width, link.to.y * size.height)
                val mid = (from.x + to.x) / 2f
                drawPath(
                    Path().apply {
                        moveTo(from.x, from.y)
                        cubicTo(mid, from.y, mid, to.y, to.x, to.y)
                    },
                    color = edgeColor,
                    style = Stroke(width = 1.dp.toPx()),
                )
            }

            layout.points.forEach { point ->
                val centre = Offset(point.x * size.width, point.y * size.height)
                val isActive = point.node.id == activeNodeId
                val radius = if (isActive) 4.5.dp.toPx() else 3.dp.toPx()
                val colour = if (point.node.suggested) plain else accent
                drawCircle(colour, radius, centre)
                // The focused node wears a ring, so it is findable in a shape
                // where every node is otherwise a dot.
                if (isActive) {
                    drawCircle(
                        colour.copy(alpha = 0.35f),
                        radius + 3.dp.toPx(),
                        centre,
                        style = Stroke(width = 2.dp.toPx()),
                    )
                }
            }
        }
    }
}

@Composable
private fun MapFocus(
    focus: ai.magicbeans.magdroid.thinking.ThinkingFocus,
    onFocus: (String) -> Unit,
    onAccept: (String) -> Unit,
    onReject: (String) -> Unit,
    onDelete: (String) -> Unit,
    onDisconnect: (String) -> Unit,
) {
    val node = focus.node
    if (node == null) {
        Box(Modifier.fillMaxSize().padding(32.dp), Alignment.Center) {
            Text("This map is empty.", color = Muted, fontSize = 13.sp)
        }
        return
    }

    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        // The path back, tappable. Without it a reader who follows three
        // branches has no way back up except closing the map.
        if (focus.ancestry.size > 1) {
            item {
                Row(
                    Modifier.horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    focus.ancestry.dropLast(1).forEach { crumb ->
                        Text(
                            crumb.title,
                            color = Coral, fontSize = 11.sp,
                            modifier = Modifier.clickable { onFocus(crumb.id) },
                        )
                        Text("›", color = Muted, fontSize = 11.sp)
                    }
                }
            }
        }

        item {
            SectionLabelSmall(
                if (node.suggested) "A DIRECTION TO EXPLORE" else "YOU ARE EXPLORING",
            )
        }
        item {
            Surface(
                color = Panel,
                shape = RoundedCornerShape(12.dp),
                border = BorderStroke(1.dp, Coral.copy(alpha = 0.45f)),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    KindChip(node.nodeKind)
                    Text(node.title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.Medium)
                    node.detail.takeIf { it.isNotBlank() }?.let {
                        Text(it, color = Secondary, fontSize = 13.sp, lineHeight = 18.sp)
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        // Accepting makes a suggestion the owner's own;
                        // rejecting records that they said no, rather than
                        // deleting it as though it had never been proposed.
                        if (node.suggested) {
                            MapAction("Keep", enabled = true) { onAccept(node.id) }
                            MapAction("Not this", enabled = true) { onReject(node.id) }
                        }
                        MapAction("Remove", enabled = true) { onDelete(node.id) }
                    }
                }
            }
        }

        if (focus.branches.isNotEmpty()) {
            item {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    SectionLabelSmall("CHOOSE WHERE TO GO NEXT", Modifier.weight(1f))
                    Text("${focus.branches.size} branches", color = Muted, fontSize = 11.sp)
                }
            }
            items(focus.branches, key = { "b-${it.id}" }) { branch ->
                NodeCard(node = branch, onClick = { onFocus(branch.id) })
            }
        } else {
            item {
                Text(
                    "Nothing branches from here yet.",
                    color = Muted, fontSize = 12.sp,
                )
            }
        }

        if (focus.related.isNotEmpty()) {
            item { SectionLabelSmall("CONNECTED ELSEWHERE") }
            items(focus.related, key = { "r-${it.id}" }) { other ->
                // Long press removes the link. `connect` shipped without its
                // inverse, so a wrong connection could only be undone by
                // deleting a node that was not the problem.
                NodeCard(
                    node = other,
                    onClick = { onFocus(other.id) },
                    onLongClick = { onDisconnect(other.id) },
                )
            }
        }
    }
}

@OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
@Composable
private fun NodeCard(
    node: ai.magicbeans.magdroid.thinking.ThinkingNode,
    onClick: () -> Unit,
    onLongClick: (() -> Unit)? = null,
) {
    Surface(
        color = if (node.suggested) Ground else Panel,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(
            1.dp,
            if (node.suggested) Coral.copy(alpha = 0.3f) else BorderSoft,
        ),
        modifier = Modifier.fillMaxWidth().then(
            if (onLongClick == null) {
                Modifier.clickable { onClick() }
            } else {
                Modifier.combinedClickable(onClick = onClick, onLongClick = onLongClick)
            },
        ),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                KindChip(node.nodeKind)
                if (node.suggested) {
                    Spacer(Modifier.width(6.dp))
                    Text("Suggested", color = Coral, fontSize = 10.sp)
                }
            }
            Text(node.title, color = Ink, fontSize = 13.sp)
            node.detail.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Secondary, fontSize = 11.sp, maxLines = 2)
            }
        }
    }
}

/**
 * What the map is waiting on the owner for.
 *
 * Both of these rode on every map payload and neither was read, so the two ways
 * the agent can ask for a decision were invisible here: a clarification left the
 * map looking idle when it was actually blocked, and a restructure proposal
 * could not be answered at all — `decideProposal` existed with no way to learn
 * a proposal id.
 */
@Composable
private fun PendingWork(
    clarifications: List<ai.magicbeans.magdroid.thinking.ThinkingClarification>,
    proposals: List<ai.magicbeans.magdroid.thinking.RestructureProposal>,
    onAnswer: (String, String) -> Unit,
    onDefer: (String) -> Unit,
    onDecide: (String, ai.magicbeans.magdroid.thinking.ProposalDecision) -> Unit,
) {
    if (clarifications.isEmpty() && proposals.isEmpty()) return

    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        clarifications.forEach { clarification ->
            key(clarification.id) {
                ClarificationCard(
                    question = clarification.question,
                    onAnswer = { onAnswer(clarification.id, it) },
                    onDefer = { onDefer(clarification.id) },
                )
            }
        }
        proposals.forEach { proposal ->
            key(proposal.id) {
                Surface(
                    color = Coral.copy(alpha = 0.10f),
                    shape = RoundedCornerShape(10.dp),
                    modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
                ) {
                    Column(Modifier.padding(12.dp)) {
                        SectionLabelSmall("SUGGESTED RESTRUCTURE")
                        Text(
                            proposal.rationale,
                            color = Ink, fontSize = 13.sp,
                            modifier = Modifier.padding(top = 4.dp),
                        )
                        if (proposal.affectedNodeIds.isNotEmpty()) {
                            Text(
                                "${proposal.affectedNodeIds.size} thoughts move",
                                color = Secondary, fontSize = 11.sp,
                                modifier = Modifier.padding(top = 2.dp),
                            )
                        }
                        Row(Modifier.padding(top = 6.dp)) {
                            TextButton(onClick = {
                                onDecide(
                                    proposal.id,
                                    ai.magicbeans.magdroid.thinking.ProposalDecision.Confirm,
                                )
                            }) { Text("Accept", color = Coral, fontSize = 13.sp) }
                            TextButton(onClick = {
                                onDecide(
                                    proposal.id,
                                    ai.magicbeans.magdroid.thinking.ProposalDecision.Reject,
                                )
                            }) { Text("No", color = Secondary, fontSize = 13.sp) }
                        }
                    }
                }
            }
        }
    }
}

/**
 * One question, with room to answer it.
 *
 * The draft is held per card so two open questions cannot type into each
 * other's box — they are keyed by clarification id above for the same reason.
 */
@Composable
private fun ClarificationCard(question: String, onAnswer: (String) -> Unit, onDefer: () -> Unit) {
    var draft by remember { mutableStateOf("") }
    Surface(
        color = Panel,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, Coral.copy(alpha = 0.35f)),
        modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
    ) {
        Column(Modifier.padding(12.dp)) {
            SectionLabelSmall("MAGICIAN ASKS")
            Text(
                question,
                color = Ink, fontSize = 13.sp,
                modifier = Modifier.padding(top = 4.dp),
            )
            MagicianTextField(
                value = draft,
                onValueChange = { draft = it },
                placeholder = { Text("Answer…", color = Secondary, fontSize = 13.sp) },
                singleLine = false,
                textStyle = LocalTextStyle.current.copy(color = Ink, fontSize = 13.sp),
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            )
            Row(Modifier.padding(top = 4.dp)) {
                TextButton(
                    onClick = {
                        onAnswer(draft)
                        draft = ""
                    },
                    enabled = draft.isNotBlank(),
                ) { Text("Answer", color = Coral, fontSize = 13.sp) }
                // Deferred, not dismissed: "not now" is a different answer from
                // "this does not apply", and the map keeps the question either
                // way.
                TextButton(onClick = onDefer) { Text("Later", color = Secondary, fontSize = 13.sp) }
            }
        }
    }
}

@Composable
private fun SectionLabelSmall(text: String, modifier: Modifier = Modifier) {
    Text(text, color = Muted, fontSize = 10.sp, fontWeight = FontWeight.SemiBold, modifier = modifier)
}

@Composable
private fun OutlineList(rows: List<OutlineRow>) {
    if (rows.isEmpty()) {
        Box(Modifier.fillMaxSize().padding(32.dp), Alignment.Center) {
            Text("This map is empty.", color = Muted, fontSize = 13.sp)
        }
        return
    }
    Column(Modifier.fillMaxSize()) {
        LazyColumn(
            Modifier.fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            items(rows, key = { it.node.id }) { row ->
                Row(
                    // Depth is capped: past four levels the indent costs more
                    // width than the nesting is worth on a phone.
                    Modifier.padding(start = (row.depth.coerceAtMost(4) * 14).dp),
                ) {
                    Surface(
                        color = if (row.node.suggested) Ground else Panel,
                        shape = RoundedCornerShape(8.dp),
                        border = BorderStroke(
                            1.dp,
                            if (row.node.suggested) Coral.copy(alpha = 0.3f) else BorderSoft,
                        ),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(
                            Modifier.padding(horizontal = 10.dp, vertical = 8.dp),
                            verticalArrangement = Arrangement.spacedBy(2.dp),
                        ) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                KindChip(row.node.nodeKind)
                                if (row.node.suggested) {
                                    Spacer(Modifier.width(6.dp))
                                    // Marked because it is the agent's thought,
                                    // not the owner's, and a map that blurs the
                                    // two misrepresents what they decided.
                                    Text("Suggested", color = Coral, fontSize = 10.sp)
                                }
                            }
                            Text(row.node.title, color = Ink, fontSize = 13.sp)
                            row.node.detail.takeIf { it.isNotBlank() }?.let {
                                Text(it, color = Secondary, fontSize = 11.sp, maxLines = 4)
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun KindChip(kind: ThinkingNodeKind) {
    Surface(color = Coral.copy(alpha = 0.12f), shape = RoundedCornerShape(4.dp)) {
        Text(
            kind.label,
            color = Coral, fontSize = 10.sp, fontWeight = FontWeight.SemiBold,
            modifier = Modifier.padding(horizontal = 5.dp, vertical = 1.dp),
        )
    }
}
