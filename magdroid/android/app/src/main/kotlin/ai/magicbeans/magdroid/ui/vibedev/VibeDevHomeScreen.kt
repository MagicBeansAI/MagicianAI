package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.AppShortcutLinks
import ai.magicbeans.magdroid.ui.AppShortcutTarget
import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.VibeDevLiveRun
import ai.magicbeans.magdroid.vibedev.VibeDevNeedsYou
import ai.magicbeans.magdroid.vibedev.VibeDevNowData
import ai.magicbeans.magdroid.vibedev.VibeDevNowViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevProject
import ai.magicbeans.magdroid.vibedev.VibeDevRunState
import ai.magicbeans.magdroid.vibedev.VibeDevStatus
import ai.magicbeans.magdroid.vibedev.isoMillis
import androidx.compose.animation.core.Animatable
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.TabRowDefaults
import androidx.compose.material3.TabRowDefaults.tabIndicatorOffset
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlin.math.roundToInt

/** VibeDev home: Now (does anything need me?) and Projects. */
@Composable
internal fun VibeDevHomeScreen(chrome: VibeDevChrome, model: VibeDevNowViewModel, snackbar: SnackbarHostState) {
    AttachWhileVisible(model, chrome)
    VibeDevMessages(model, snackbar)
    val loaded by model.data.collectAsStateWithLifecycle()
    val hidden by model.hiddenCrewIds.collectAsStateWithLifecycle()
    val status by model.loadStatus.collectAsStateWithLifecycle()
    val data = loaded.copy(hiddenCrews = hidden)
    val fonts = LocalMagicanFontFamilies.current

    Column(Modifier.fillMaxSize().background(activePalette.background)) {
        val selected = chrome.homeTab.ordinal
        TabRow(
            selectedTabIndex = selected,
            containerColor = activePalette.background,
            contentColor = activePalette.accent,
            indicator = { positions ->
                TabRowDefaults.SecondaryIndicator(Modifier.tabIndicatorOffset(positions[selected]), height = 3.dp, color = activePalette.accent)
            },
            divider = { HorizontalDivider(color = activePalette.cardBorder) },
        ) {
            VibeDevHomeTab.entries.forEach { tab ->
                Tab(
                    selected = chrome.homeTab == tab,
                    onClick = { chrome.homeTab = tab },
                    selectedContentColor = activePalette.accent,
                    unselectedContentColor = activePalette.secondaryText,
                    text = {
                        Text(
                            when (tab) {
                                VibeDevHomeTab.Now -> data.needsYou.size.let { if (it > 0) "Now · $it" else "Now" }
                                VibeDevHomeTab.Projects -> "Projects"
                            },
                            fontWeight = FontWeight.SemiBold, fontFamily = fonts.body, fontSize = 14.sp,
                        )
                    },
                )
            }
        }
        Box(Modifier.weight(1f).fillMaxWidth()) {
            val hasData = status.updatedAtMs != null
            LazyColumn(
                Modifier.fillMaxSize(),
                contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 6.dp, bottom = 96.dp),
            ) {
                item(key = "stale") { StaleLine(status, hasData, onRetry = { model.refresh() }) }
                if (!hasData && status.loading) item(key = "loading") { LoadingLine() }
                if (hasData) when (chrome.homeTab) {
                    VibeDevHomeTab.Now -> nowItems(chrome, model, data, status)
                    VibeDevHomeTab.Projects -> projectItems(chrome, data)
                }
            }
            ExtendedFloatingActionButton(
                onClick = {
                    val project = data.openProjects.firstOrNull()
                    chrome.sheet = VibeDevSheet.NewRun(projectId = project?.projectId)
                },
                modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
                containerColor = activePalette.accentFill,
                contentColor = activePalette.onAccent,
                icon = { Icon(Icons.Outlined.Add, contentDescription = null) },
                text = { Text("New run", fontWeight = FontWeight.SemiBold, fontFamily = fonts.body) },
            )
        }
    }
}

private fun LazyListScope.nowItems(
    chrome: VibeDevChrome,
    model: VibeDevNowViewModel,
    data: VibeDevNowData,
    status: VibeDevStatus,
) {
    val asks = data.needsYou
    item(key = "needs-label") { SectionLabel("Needs you", asks.size) }
    if (asks.isEmpty()) item(key = "needs-empty") { EmptyLine("Nothing needs you right now.") }
    items(asks, key = { it.key }) { ask ->
        when (ask) {
            is VibeDevNeedsYou.CrewAsk -> VibeDevCrewCard(
                crew = ask.crew,
                projectName = ask.projectName,
                question = ask.question,
                busy = "crew:${ask.crew.crewId}" in status.busy,
                onDecide = { decision -> model.decide(ask.projectId, ask.crew, decision) },
                onOpenRun = { id, title -> chrome.push(VibeDevRoute.Run(id, title)) },
            )
            is VibeDevNeedsYou.RunAsk -> RunAskCard(
                run = ask.run,
                busy = "run:${ask.run.runId}" in status.busy,
                onReview = { chrome.push(VibeDevRoute.Run(ask.run.runId, ask.run.title, openChanges = true)) },
                onApprove = { model.approve(ask.run) },
            )
        }
    }
    val crews = data.runningCrews
    if (crews.isNotEmpty()) {
        item(key = "crews-label") { SectionLabel("VibeDev crews", crews.size) }
        items(crews, key = { "crew-running:${it.second.crewId}" }) { (project, crew) ->
            VibeDevCrewCard(
                crew = crew, projectName = project.name, question = null,
                busy = "crew:${crew.crewId}" in status.busy,
                onDecide = { model.decide(project.projectId, crew, it) },
                onOpenRun = { id, title -> chrome.push(VibeDevRoute.Run(id, title)) },
            )
        }
    }
    val working = data.working
    item(key = "working-label") { SectionLabel("Working", working.size) }
    if (working.isEmpty()) {
        item(key = "working-empty") { EmptyLine("No run is going.") }
    } else {
        items(working, key = { "working:${it.runId}" }) { run ->
            LiveRunRow(
                run = run,
                onOpen = { chrome.push(VibeDevRoute.Run(run.runId, run.title)) },
                onSteer = { chrome.sheet = VibeDevSheet.Steer(run.title) { text -> model.steer(run, text) } },
                onStop = { model.stop(run) },
            )
        }
        item(key = "swipe-hint") { MetaText("Swipe a run left to steer or stop", Modifier.padding(start = 4.dp, top = 2.dp)) }
    }
    val finished = data.justFinished
    if (finished.isNotEmpty()) {
        item(key = "finished-label") { SectionLabel("Just finished", finished.size) }
        items(finished, key = { "finished:${it.runId}" }) { run ->
            LiveRunRow(run = run, onOpen = { chrome.push(VibeDevRoute.Run(run.runId, run.title, openChanges = run.state == VibeDevRunState.Done)) })
        }
    }
}

private fun LazyListScope.projectItems(chrome: VibeDevChrome, data: VibeDevNowData) {
    val projects = data.openProjects
    if (projects.isEmpty()) item(key = "projects-empty") { EmptyLine("No projects yet. Tap + to add one.") }
    items(projects, key = { "project:${it.projectId}" }) { project ->
        ProjectCard(project, data.liveCount(project.projectId)) {
            chrome.push(VibeDevRoute.Project(project.projectId, project.name))
        }
    }
}

@Composable
private fun ProjectCard(project: VibeDevProject, live: Int, onOpen: () -> Unit) {
    VibeCard(onClick = onOpen) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Box(Modifier.weight(1f)) { CardTitle(project.name, maxLines = 1) }
            TrustBadge(project.isTrusted)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            MetaText(project.repoLabel.ifBlank { "No repository" }, Modifier.weight(1f), mono = true, maxLines = 1)
            MetaText(if (live > 0) "$live running" else "idle", maxLines = 1)
        }
    }
}

@Composable
private fun RunAskCard(run: VibeDevLiveRun, busy: Boolean, onReview: () -> Unit, onApprove: () -> Unit) {
    VibeCard(onClick = onReview) {
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
            RunDot(VibeDevRunState.NeedsYou)
            Text("${run.projectName} · waiting on you", color = activePalette.warning, fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold, fontFamily = LocalMagicanFontFamilies.current.body, maxLines = 1)
        }
        CardTitle(run.title.removePrefix("VibeDev · "))
        (run.step ?: run.summary)?.let { MetaText(it) }
        Row(Modifier.padding(top = 4.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            VibeButton("Review", onReview, kind = VibeButtonKind.Primary)
            // Only a diff approval is answered here; any other ask in Attention.
            if (run.hasApproval) VibeButton("Approve", onApprove, busy = busy)
            else VibeButton("Open in Attention", { AppShortcutLinks.request(AppShortcutTarget.OpenAttention) })
        }
    }
}

/**
 * One live run: state dot, title, project · lane, step, minutes. Swiping it
 * left reveals Steer and Stop; the overflow menu offers the same two for
 * TalkBack and anyone who does not swipe.
 */
@Composable
internal fun LiveRunRow(
    run: VibeDevLiveRun,
    onOpen: () -> Unit,
    onSteer: (() -> Unit)? = null,
    onStop: (() -> Unit)? = null,
) {
    val now = rememberNow()
    val fonts = LocalMagicanFontFamilies.current
    val swipe = onSteer != null && onStop != null
    val density = LocalDensity.current
    val revealPx = with(density) { REVEAL_WIDTH.dp.toPx() }
    val offset = remember { Animatable(0f) }
    val scope = rememberCoroutineScope()
    var menu by remember { mutableStateOf(false) }
    val sub = buildList {
        run.lane?.let { add("$it lane") }
        add(run.projectName)
        (if (run.state.isEnded) run.summary else run.step)?.takeIf { it.isNotBlank() }?.let(::add)
    }.joinToString(" · ")
    val right = when {
        run.state == VibeDevRunState.Working -> elapsed(isoMillis(run.startedAt), now)
            ?: agoIso(run.lastProgressAt ?: run.updatedAt, now)
        run.state.isEnded -> run.costUsd?.let(::money)
            ?: agoIso(run.updatedAt, now).let { if (it.isEmpty() || it == "just now") it else "$it ago" }
        else -> run.state.label
    }
    Box(Modifier.fillMaxWidth().padding(bottom = 8.dp)) {
        if (swipe) {
            Row(
                Modifier.matchParentSize().clip(ai.magicbeans.magdroid.ui.Lively.cardShape).background(activePalette.surface),
                horizontalArrangement = Arrangement.End,
            ) {
                RevealAction("Steer", activePalette.accentFill, activePalette.onAccent) {
                    scope.launch { offset.animateTo(0f) }; onSteer?.invoke()
                }
                RevealAction("Stop", activePalette.danger, activePalette.background) {
                    scope.launch { offset.snapTo(0f) }; onStop?.invoke()
                }
            }
        }
        Row(
            Modifier
                .offset { IntOffset(offset.value.roundToInt(), 0) }
                .fillMaxWidth()
                .heightIn(min = 56.dp)
                .background(activePalette.card, ai.magicbeans.magdroid.ui.Lively.cardShape)
                .then(
                    if (swipe) Modifier.draggable(
                        state = rememberDraggableState { delta ->
                            scope.launch { offset.snapTo((offset.value + delta).coerceIn(-revealPx, 0f)) }
                        },
                        orientation = Orientation.Horizontal,
                        onDragStopped = { velocity ->
                            offset.animateTo(if (offset.value < -revealPx / 2 || velocity < -1200f) -revealPx else 0f)
                        },
                    ) else Modifier,
                )
                .clickable(role = Role.Button, onClickLabel = "Open run", onClick = onOpen)
                .semantics {
                    customActions = buildList {
                        add(CustomAccessibilityAction("Open") { onOpen(); true })
                        if (swipe) {
                            add(CustomAccessibilityAction("Steer") { onSteer?.invoke(); true })
                            add(CustomAccessibilityAction("Stop") { onStop?.invoke(); true })
                        }
                    }
                }
                .padding(start = 14.dp, end = 4.dp, top = 10.dp, bottom = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            RunDot(run.state)
            Column(Modifier.weight(1f)) {
                Text(run.title.removePrefix("VibeDev · "), color = activePalette.text, fontSize = 14.sp,
                    fontWeight = FontWeight.SemiBold, fontFamily = fonts.body, maxLines = 1, overflow = TextOverflow.Ellipsis)
                MetaText(sub, maxLines = 1)
            }
            MetaText(right, maxLines = 1)
            if (swipe) {
                Box {
                    IconButton(onClick = { menu = true }) {
                        Icon(Icons.Outlined.MoreVert, contentDescription = "Steer or stop", tint = activePalette.secondaryText)
                    }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Steer") }, onClick = { menu = false; onSteer?.invoke() })
                        DropdownMenuItem(text = { Text("Stop", color = activePalette.danger) }, onClick = { menu = false; onStop?.invoke() })
                    }
                }
            } else {
                Box(Modifier.width(8.dp))
            }
        }
    }
}

private const val REVEAL_WIDTH = 168

@Composable
private fun RevealAction(label: String, fill: androidx.compose.ui.graphics.Color, ink: androidx.compose.ui.graphics.Color, onClick: () -> Unit) {
    Box(
        Modifier.width((REVEAL_WIDTH / 2).dp).fillMaxHeight().background(fill)
            .clickable(role = Role.Button, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = ink, fontSize = 13.sp, fontWeight = FontWeight.Bold, fontFamily = LocalMagicanFontFamilies.current.body)
    }
}
