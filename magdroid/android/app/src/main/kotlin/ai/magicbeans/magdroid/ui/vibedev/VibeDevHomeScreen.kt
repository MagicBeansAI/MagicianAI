package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.VibeDevLiveRun
import ai.magicbeans.magdroid.vibedev.VibeDevNowViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevRunState
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
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.pulltorefresh.PullToRefreshDefaults
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
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


/** Run history; the composer opens only when starting or continuing a run. */
@Composable
@OptIn(ExperimentalMaterial3Api::class)
internal fun VibeDevHomeScreen(chrome: VibeDevChrome, model: VibeDevNowViewModel,
    snackbar: SnackbarHostState) {
    AttachWhileVisible(model, chrome)
    VibeDevMessages(model, snackbar)
    val data by model.data.collectAsStateWithLifecycle()
    val status by model.loadStatus.collectAsStateWithLifecycle()
    val refreshState = rememberPullToRefreshState()
    Column(Modifier.fillMaxSize().background(activePalette.background)) {
        PullToRefreshBox(isRefreshing = status.loading, onRefresh = { model.refresh() },
            modifier = Modifier.weight(1f).fillMaxWidth().semantics {
                customActions = listOf(CustomAccessibilityAction("Refresh runs") { model.refresh(); true })
            }, state = refreshState,
            indicator = {
                PullToRefreshDefaults.Indicator(state = refreshState, isRefreshing = status.loading,
                    modifier = Modifier.align(Alignment.TopCenter),
                    containerColor = activePalette.card, color = activePalette.accent)
            }) {
            LazyColumn(Modifier.fillMaxSize(),
                contentPadding = PaddingValues(horizontal = 8.dp, vertical = 6.dp)) {
                item(key = "stale") { StaleLine(status, status.updatedAtMs != null, onRetry = { model.refresh() }) }
                if (status.loading && data.history.isEmpty()) item(key = "loading") { LoadingLine() }
                if (!status.loading && data.history.isEmpty()) item(key = "empty") {
                    EmptyLine("No runs yet. Start a new run, or tap + to add a project.")
                }
                items(data.history, key = { it.runId }) { run ->
                    val stopping = run.runId in data.stopping
                    LiveRunRow(run, stopping = stopping, onOpen = { chrome.push(VibeDevRoute.Run(run.runId, run.title)) },
                        onSteer = if (run.state.isLive && !stopping) ({ chrome.sheet = VibeDevSheet.Steer(run.title, run.liveTaskId) { model.steer(run, it) } }) else null,
                        onStop = if (run.state.isLive && !stopping) ({ model.stop(run) }) else null)
                }
                data.pageError?.let { error -> item(key = "page-error") { MetaText(error) } }
                if (data.nextCursor != null) item(key = "more") {
                    VibeButton("Load older runs", model::loadMore, Modifier.fillMaxWidth(), busy = data.loadingMore)
                }
            }
        }
        VibeButton("+ New run", { chrome.sheet = VibeDevSheet.NewRun(projectId = null) },
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), kind = VibeButtonKind.Primary)
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
    stopping: Boolean = false,
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
    val location = run.workLocation?.let { when (it) {
        "original" -> "Original folder"
        "trusted" -> "Isolated branch"
        else -> "Isolated workspace"
    } }
    val repo = buildList {
        run.repoPath?.substringAfterLast('/')?.takeIf { it.isNotBlank() }?.let(::add)
        run.lane?.let { add("$it lane") }
    }.joinToString(" · ")
    val right = when {
        stopping -> "Stopping…"
        run.state == VibeDevRunState.Working -> elapsed(isoMillis(run.startedAt), now)
            ?: agoIso(run.lastProgressAt ?: run.updatedAt, now)
        run.state.isEnded -> run.costUsd?.let(::money)
            ?: agoIso(run.updatedAt, now)
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
                .padding(start = 10.dp, end = 4.dp, top = 10.dp, bottom = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            RunDot(if (stopping) VibeDevRunState.Stopped else run.state, run.isPartlyDone && !stopping)
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(run.title.removePrefix("VibeDev · "), modifier = Modifier.weight(1f), color = activePalette.text, fontSize = 14.sp,
                        fontWeight = FontWeight.SemiBold, fontFamily = fonts.body, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    if ((run.needsYouCount ?: 0) > 0) Tag("${run.needsYouCount} needs you", strong = true)
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    MetaText(run.projectName, Modifier.weight(1f), maxLines = 1)
                    location?.let { Tag(it) }
                }
                if (repo.isNotEmpty()) MetaText(repo, maxLines = 1)
                if (run.isPartlyDone && !stopping) {
                    Text(listOfNotNull(run.statusLabel, run.openItemsLine).joinToString(" · "), color = activePalette.warning,
                        fontSize = 12.sp, fontFamily = fonts.body, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
            MetaText(right, maxLines = 1)
            if (swipe) {
                Box {
                    VibeIconButton(onClick = { menu = true }) {
                        Icon(Icons.Outlined.MoreVert, contentDescription = "Steer or stop", tint = activePalette.secondaryText)
                    }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Steer") }, onClick = { menu = false; onSteer?.invoke() })
                        DropdownMenuItem(text = { Text("Stop", color = activePalette.danger) }, onClick = { menu = false; onStop?.invoke() })
                    }
                }
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
