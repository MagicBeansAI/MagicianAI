package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.MagicianTextField
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.VibeDevCrewQuestion
import ai.magicbeans.magdroid.vibedev.VibeDevProjectViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevRunRow
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/** One project: header, VibeDev crews, and its runs a page at a time. */
@Composable
internal fun VibeDevProjectScreen(chrome: VibeDevChrome, model: VibeDevProjectViewModel, snackbar: SnackbarHostState) {
    AttachWhileVisible(model, chrome)
    VibeDevMessages(model, snackbar)
    val data by model.data.collectAsStateWithLifecycle()
    val status by model.loadStatus.collectAsStateWithLifecycle()
    val hidden by model.hiddenCrewIds.collectAsStateWithLifecycle()
    val fonts = LocalMagicanFontFamilies.current
    val project = data.project
    val trusted = project?.isTrusted == true

    Box(Modifier.fillMaxSize().background(activePalette.background)) {
        LazyColumn(
            Modifier.fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 96.dp),
        ) {
            item(key = "stale") { StaleLine(status, project != null, onRetry = { model.refresh() }) }
            if (project == null) {
                if (status.loading) item(key = "loading") { LoadingLine() }
                return@LazyColumn
            }
            item(key = "header") {
                VibeCard {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(project.name, Modifier.weight(1f), color = activePalette.text, fontSize = 18.sp,
                            fontWeight = FontWeight.Bold, fontFamily = fonts.display, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        TrustBadge(trusted)
                    }
                    KeyValues(buildList {
                        add("Repo" to project.repoLabel.ifBlank { "—" })
                        if (trusted) add("Base" to (project.baseBranch ?: "the repo's current branch"))
                    })
                    if (trusted) {
                        Row(Modifier.padding(top = 4.dp)) {
                            VibeButton("New VibeDev crew", {
                                chrome.sheet = VibeDevSheet.NewRun(project.projectId, crew = true)
                            })
                        }
                    }
                }
            }
            val crews = data.openCrews.filterNot { it.crewId in hidden }
            if (crews.isNotEmpty() || data.finishedCrews > 0) {
                item(key = "crews-label") { SectionLabel("VibeDev crews", crews.size) }
                items(crews, key = { "crew:${it.crewId}" }) { crew ->
                    VibeDevCrewCard(
                        crew = crew, projectName = null, question = VibeDevCrewQuestion.of(crew),
                        busy = "crew:${crew.crewId}" in status.busy,
                        onDecide = { model.decide(project.projectId, crew, it) },
                        onOpenRun = { id, title -> chrome.push(VibeDevRoute.Run(id, title)) },
                    )
                }
                if (data.finishedCrews > 0) item(key = "crews-finished") {
                    MetaText("${data.finishedCrews} finished", Modifier.padding(start = 4.dp, bottom = 4.dp))
                }
            }
            item(key = "runs-label") { SectionLabel("Runs", data.totalRuns) }
            item(key = "search") {
                MagicianTextField(
                    value = data.query,
                    onValueChange = model::search,
                    modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
                    singleLine = true,
                    placeholder = { Text("Search runs", color = activePalette.secondaryText, fontSize = 13.sp) },
                    leadingIcon = { Icon(Icons.Outlined.Search, contentDescription = null, tint = activePalette.secondaryText, modifier = Modifier.size(16.dp)) },
                )
            }
            val runs = data.runs
            if (runs.isEmpty() && !status.loading) item(key = "runs-empty") {
                EmptyLine(if (data.query.isBlank()) "No runs yet. Start one below." else "No run matches.")
            }
            if (runs.isEmpty() && status.loading) item(key = "runs-loading") { LoadingLine() }
            items(runs, key = { "run:${it.root.id}" }) { row ->
                ProjectRunRow(row) { chrome.push(VibeDevRoute.Run(row.root.id, row.root.title)) }
            }
            if (data.hasMore || data.moreError != null) item(key = "more") {
                Box(Modifier.fillMaxWidth().padding(vertical = 6.dp), contentAlignment = Alignment.Center) {
                    if (data.moreError != null) {
                        Column(horizontalAlignment = Alignment.CenterHorizontally) {
                            MetaText(data.moreError.orEmpty())
                            VibeButton("Retry", model::loadMore)
                        }
                    } else {
                        VibeButton("Load more", model::loadMore, busy = data.loadingMore)
                    }
                }
            }
        }
        ExtendedFloatingActionButton(
            onClick = { chrome.sheet = VibeDevSheet.NewRun(project?.projectId) },
            modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
            containerColor = activePalette.accentFill,
            contentColor = activePalette.onAccent,
            icon = { Icon(Icons.Outlined.Add, contentDescription = null) },
            text = { Text("New run", fontWeight = FontWeight.SemiBold, fontFamily = fonts.body) },
        )
    }
}

@Composable
private fun ProjectRunRow(row: VibeDevRunRow, onOpen: () -> Unit) {
    val now = rememberNow()
    VibeCard(onClick = onOpen) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            RunDot(row.state)
            Text(row.root.title.removePrefix("VibeDev · "), Modifier.weight(1f), color = activePalette.text, fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold, fontFamily = LocalMagicanFontFamilies.current.body,
                maxLines = 2, overflow = TextOverflow.Ellipsis)
            row.lane?.let { Tag("$it lane", strong = true) }
        }
        val turns = row.turns.size
        MetaText(
            listOf(
                if (turns == 1) "1 turn" else "$turns turns",
                agoIso(row.updatedAt, now).let { if (it.isEmpty() || it == "just now") it else "$it ago" },
                row.state.label,
            ).filter { it.isNotEmpty() }.joinToString(" · "),
            Modifier.padding(start = 18.dp),
            maxLines = 1,
        )
    }
}
