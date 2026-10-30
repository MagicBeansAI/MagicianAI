package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.AppShortcutLinks
import ai.magicbeans.magdroid.ui.AppShortcutTarget
import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.VibeDevRunData
import ai.magicbeans.magdroid.vibedev.VibeDevRunState
import ai.magicbeans.magdroid.vibedev.VibeDevRunTurn
import ai.magicbeans.magdroid.vibedev.VibeDevRunViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevStatus
import ai.magicbeans.magdroid.vibedev.WorkItemIds
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.TabRowDefaults
import androidx.compose.material3.TabRowDefaults.tabIndicatorOffset
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/** One run: header, Turns (each expands to its timeline) and Changes. */
@Composable
internal fun VibeDevRunScreen(
    chrome: VibeDevChrome,
    route: VibeDevRoute.Run,
    model: VibeDevRunViewModel,
    composer: ai.magicbeans.magdroid.vibedev.VibeDevComposerViewModel,
    now: ai.magicbeans.magdroid.vibedev.VibeDevNowViewModel,
    snackbar: SnackbarHostState,
) {
    AttachWhileVisible(model, chrome)
    VibeDevMessages(model, snackbar)
    val data by model.data.collectAsStateWithLifecycle()
    val status by model.loadStatus.collectAsStateWithLifecycle()
    var tab by rememberSaveable(route.runId) { mutableIntStateOf(if (route.openChanges) 1 else 0) }
    val turns = data.turns
    androidx.compose.runtime.LaunchedEffect(turns?.title) { turns?.title?.let { chrome.retitle(route, it) } }

    Column(Modifier.fillMaxSize().background(activePalette.background)) {
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 16.dp),
        ) {
            item(key = "stale") { StaleLine(status, turns != null, onRetry = { model.refresh() }) }
            if (turns == null) {
                if (status.loading || status.error == null) item(key = "loading") { LoadingLine() }
                return@LazyColumn
            }
            item(key = "header") { RunHeader(data) }
            item(key = "tabs") {
                TabRow(
                    selectedTabIndex = tab,
                    containerColor = activePalette.background,
                    contentColor = activePalette.accent,
                    indicator = { positions ->
                        TabRowDefaults.SecondaryIndicator(Modifier.tabIndicatorOffset(positions[tab]), height = 3.dp, color = activePalette.accent)
                    },
                    divider = { HorizontalDivider(color = activePalette.cardBorder) },
                    modifier = Modifier.padding(bottom = 8.dp),
                ) {
                    listOf("Turns · ${turns.turns.size}", "Changes · ${data.changes?.files?.size ?: 0}").forEachIndexed { index, label ->
                        Tab(
                            selected = tab == index,
                            onClick = { tab = index },
                            selectedContentColor = activePalette.accent,
                            unselectedContentColor = activePalette.secondaryText,
                            text = { Text(label, fontWeight = FontWeight.SemiBold, fontFamily = LocalMagicanFontFamilies.current.body, fontSize = 14.sp) },
                        )
                    }
                }
            }
            if (tab == 0) turnItems(data, model) else changeItems(chrome, data, model)
        }
        val home by now.data.collectAsStateWithLifecycle()
        VibeDevAskDock(turns?.turns.orEmpty().map { it.taskId }.toSet() + route.runId,
            home.openProjects.firstOrNull { it.projectId == turns?.projectId }?.chatSessionId)
        RunActions(chrome, data, status, model)
        if (turns != null && data.state.isEnded) {
            androidx.compose.runtime.key(route.runId) {
                val previous = turns.turns.lastOrNull()
                VibeDevComposer(VibeDevSheet.NewRun(turns.projectId, turns.liveTaskId, turns.title,
                    codingRouteId = turns.turns.lastOrNull { it.codingRouteId != null }?.codingRouteId,
                    engine = turns.engine, mode = previous?.mode ?: "build"),
                    home.openProjects, composer, onDismiss = {}, embedded = true, showPendingAsks = false, workLocation = turns.workLocation ?: if (turns.trusted) "trusted" else "sandboxed")
            }
        }
    }
}

@Composable
private fun RunHeader(data: VibeDevRunData) {
    val turns = data.turns ?: return
    val state = data.state
    VibeCard {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            RunDot(state, data.isPartlyDone && !data.pendingStop)
            MetaText(if (data.pendingStop) "Stopping" else data.statusLabel, maxLines = 1)
            turns.lane?.let { Tag("$it lane", strong = true) }
            // While it runs: its own ongoing notification until it ends.
            if (state.isLive) {
                Spacer(Modifier.weight(1f))
                WorkFollowButton(WorkItemIds.run(turns.runId))
            }
        }
        CardTitle(turns.title.removePrefix("VibeDev · "), maxLines = 3)
        data.openItemsLine?.let {
            Text(it, color = activePalette.warning, fontSize = 13.sp, fontFamily = LocalMagicanFontFamilies.current.body,
                maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        KeyValues(buildList {
            add("Project" to turns.projectName.ifBlank { "—" })
            turns.engine?.let { add("Engine" to it) }
            add("Cost" to money(turns.costUsd))
            turns.workLocation?.let { add("Work location" to when (it) { "original" -> "Original folder"; "trusted" -> "Isolated branch"; else -> "Isolated workspace" }) }
            if (turns.trusted) add("Branch" to (turns.branch ?: "—"))
        })
    }
}

private fun LazyListScope.turnItems(data: VibeDevRunData, model: VibeDevRunViewModel) {
    val turns = data.turns?.turns.orEmpty()
    if (turns.isEmpty()) item(key = "turns-empty") { EmptyLine("No turns yet.") }
    turns.forEachIndexed { index, turn ->
        item(key = "turn:${turn.taskId}") {
            TurnSection(
                turn = turn,
                index = index,
                expanded = turn.taskId in data.expanded,
                onToggle = { model.toggleTurn(turn.taskId) },
            )
        }
        if (turn.taskId in data.expanded) item(key = "timeline:${turn.taskId}") {
            VibeDevTurnTimeline(data.timelines[turn.taskId]) { model.loadTimeline(turn.taskId) }
        }
    }
}

@Composable
private fun TurnSection(turn: VibeDevRunTurn, index: Int, expanded: Boolean, onToggle: () -> Unit) {
    val fonts = LocalMagicanFontFamilies.current
    Surface(
        Modifier.fillMaxWidth().padding(bottom = 6.dp),
        shape = ai.magicbeans.magdroid.ui.Lively.cardShape,
        color = activePalette.card,
        border = androidx.compose.foundation.BorderStroke(1.dp, activePalette.cardBorder),
    ) {
        Row(
            Modifier.clickable(role = Role.Button, onClick = onToggle)
                .semantics { stateDescription = if (expanded) "Expanded" else "Collapsed" }
                .padding(12.dp),
            verticalAlignment = Alignment.Top,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Box(Modifier.padding(top = 5.dp)) { RunDot(turn.state, turn.isPartlyDone) }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text(
                    (if (index == 0) "" else "Follow-up · ") + turn.prompt,
                    color = activePalette.text, fontSize = 13.5.sp, fontFamily = fonts.body,
                    fontWeight = if (index == 0) FontWeight.SemiBold else FontWeight.Normal,
                    maxLines = 2, overflow = TextOverflow.Ellipsis,
                )
                MetaText(
                    buildList {
                        if (turn.mode == "discuss") add("Discuss")
                        turn.engine?.let(::add)
                        add(turn.statusLabel)
                        turn.costUsd?.let { add(money(it)) }
                        turn.filesChanged?.let { add(if (it == 1) "1 file" else "$it files") }
                    }.joinToString(" · "),
                    maxLines = 1,
                )
            }
            Icon(if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, contentDescription = null,
                tint = activePalette.secondaryText)
        }
    }
}

private fun LazyListScope.changeItems(chrome: VibeDevChrome, data: VibeDevRunData, model: VibeDevRunViewModel) {
    val turns = data.turns?.turns.orEmpty()
    if (turns.size > 1) item(key = "changes-filter") {
        Row(
            Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(bottom = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Pill("Whole run", data.changesTurn == null, { model.showChanges(null) })
            turns.forEachIndexed { index, turn ->
                Pill("Turn ${index + 1}", data.changesTurn == turn.taskId, { model.showChanges(turn.taskId) })
            }
        }
    }
    val changes = data.changes
    when {
        changes == null && data.changesError != null -> item(key = "changes-error") { EmptyLine(data.changesError.orEmpty()) }
        changes == null -> item(key = "changes-loading") { LoadingLine() }
        changes.files.isEmpty() -> item(key = "changes-empty") { EmptyLine("No changes yet.") }
        else -> {
            if (changes.source == "proposals") item(key = "changes-source") {
                MetaText("Proposed changes, not yet applied.", Modifier.padding(start = 4.dp, bottom = 6.dp))
            }
            items(changes.files, key = { "file:${it.path}" }) { file ->
                Row(
                    Modifier.fillMaxWidth().heightIn(min = 48.dp)
                        .clickable(role = Role.Button) {
                            chrome.push(VibeDevRoute.Diff(changes.runId, data.changesTurn, changes.files, file.path))
                        }
                        .padding(horizontal = 4.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Text(file.path, Modifier.weight(1f), color = activePalette.text, fontSize = 12.5.sp,
                        fontFamily = LocalMagicanFontFamilies.current.mono, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    DiffCounts(file.additions, file.deletions)
                }
                HorizontalDivider(color = activePalette.cardBorder)
            }
        }
    }
}

@Composable
internal fun DiffCounts(additions: Int, deletions: Int) {
    val mono = LocalMagicanFontFamilies.current.mono
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("+$additions", color = activePalette.success, fontSize = 12.sp, fontFamily = mono, fontWeight = FontWeight.SemiBold)
        Text("−$deletions", color = activePalette.danger, fontSize = 12.sp, fontFamily = mono, fontWeight = FontWeight.SemiBold)
    }
}

/**
 * The sticky action bar: Steer + Stop while live, Approve when it waits on a
 * diff, Open in Attention for any other ask, Follow up when ended.
 */
@Composable
private fun RunActions(chrome: VibeDevChrome, data: VibeDevRunData, status: VibeDevStatus, model: VibeDevRunViewModel) {
    val turns = data.turns ?: return
    if (data.state.isEnded && (!turns.trusted || data.pendingDiscard)) return
    var menu by remember { mutableStateOf(false) }
    Surface(color = activePalette.surface, modifier = Modifier.fillMaxWidth()) {
        Column {
            HorizontalDivider(color = activePalette.cardBorder)
            Row(
                Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 16.dp, vertical = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                when {
                    data.canAnswerApproval -> {
                        VibeButton("Reject", { model.approve(false) }, Modifier.weight(1f), busy = "approval" in status.busy)
                        VibeButton("Approve", { model.approve(true) }, Modifier.weight(1f), kind = VibeButtonKind.Primary,
                            enabled = "approval" !in status.busy)
                    }
                    // Not a diff: the ask is answered where it lives.
                    data.state == VibeDevRunState.NeedsYou -> {
                        if (data.liveTaskId != null && !data.pendingStop) {
                            VibeButton("Stop", model::stop, Modifier.weight(1f), kind = VibeButtonKind.Danger)
                        }
                        VibeButton("Open in Attention", { AppShortcutLinks.request(AppShortcutTarget.OpenAttention) },
                            Modifier.weight(1f), kind = VibeButtonKind.Primary)
                    }
                    data.isLive -> {
                        VibeButton("Steer", {
                            chrome.sheet = VibeDevSheet.Steer(turns.title, data.live?.liveTaskId ?: turns.liveTaskId) { text -> model.steer(text) }
                        }, Modifier.weight(1f), busy = "steer" in status.busy)
                        VibeButton("Stop", model::stop, Modifier.weight(1f), kind = VibeButtonKind.Danger)
                    }
                    else -> {
                        if (turns.trusted && !data.pendingDiscard) {
                            VibeButton("Open PR", model::openPullRequest, busy = "land" in status.busy)
                            Box {
                                VibeIconButton(onClick = { menu = true }) {
                                    Icon(Icons.Outlined.MoreVert, contentDescription = "More landing actions", tint = activePalette.text,
                                        modifier = Modifier.size(22.dp))
                                }
                                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                                    DropdownMenuItem(text = { Text("Merge into base") },
                                        onClick = { menu = false; model.merge() })
                                    DropdownMenuItem(text = { Text("Discard branch", color = activePalette.danger) },
                                        onClick = { menu = false; model.discard() })
                                }
                            }
                        }

                    }
                }
            }
        }
    }
}
