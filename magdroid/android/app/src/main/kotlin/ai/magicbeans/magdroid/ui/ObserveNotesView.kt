package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.notes.PUBLISHED_NOTES_PAGE_SIZES
import ai.magicbeans.magdroid.notes.PUBLISHED_NOTES_SEARCH_MAX
import ai.magicbeans.magdroid.notes.PublishedNotesUiState
import ai.magicbeans.magdroid.notes.PublishedTaskNote
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowForward
import androidx.compose.material.icons.automirrored.outlined.MenuBook
import androidx.compose.material.icons.outlined.ChevronLeft
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Clear
import androidx.compose.material.icons.outlined.CloudUpload
import androidx.compose.material.icons.outlined.FormatListNumbered
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Psychology
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Published Notes at iOS parity (`PublishedTaskNotesSection`): search, page
 * size, Previous/Next with "a–b of total", Publish next 25, Promote to memory,
 * Open in Notes.
 */
@Composable
internal fun PublishedNotesPanel(
    state: PublishedNotesUiState,
    onSearch: (String) -> Unit,
    onClearSearch: () -> Unit,
    onPageSize: (Int) -> Unit,
    onPrevious: () -> Unit,
    onNext: () -> Unit,
    onReload: () -> Unit,
    onBackfill: () -> Unit,
    onPromote: (PublishedTaskNote) -> Unit,
    onOpen: (PublishedTaskNote) -> Unit,
) {
    var draft by rememberSaveable { mutableStateOf(state.appliedSearch) }
    var sizeMenu by remember { mutableStateOf(false) }
    val pager = state.pager

    ObserveCard {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.AutoMirrored.Outlined.MenuBook, null, tint = Coral, modifier = Modifier.size(20.dp))
            Spacer(Modifier.width(9.dp))
            Column(Modifier.weight(1f)) {
                Text("OBSERVED KNOWLEDGE", color = Coral, fontSize = 9.sp, fontWeight = FontWeight.Bold, letterSpacing = 0.7.sp)
                Text("Published Notes", color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            }
            DeckSectionHeaderRefresh(loading = state.loading, onRefresh = onReload)
        }
        Text(
            "Completed task pages you can search, reopen, or deliberately hand to memory review.",
            color = Secondary, fontSize = 12.sp, lineHeight = 16.sp,
        )
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Box {
                CompactAction(
                    label = "${state.pageSize} per page",
                    icon = Icons.Outlined.FormatListNumbered,
                    enabled = !state.loading,
                    onClick = { sizeMenu = true },
                )
                DropdownMenu(expanded = sizeMenu, onDismissRequest = { sizeMenu = false }) {
                    PUBLISHED_NOTES_PAGE_SIZES.forEach { size ->
                        DropdownMenuItem(
                            text = { Text("$size per page" + if (size == state.pageSize) "  ✓" else "", fontSize = 13.sp) },
                            onClick = { sizeMenu = false; onPageSize(size) },
                        )
                    }
                }
            }
            Spacer(Modifier.weight(1f))
            CompactAction(
                label = if (state.backfilling) "Publishing…" else "Publish next 25",
                icon = Icons.Outlined.CloudUpload,
                enabled = !state.backfilling && !state.loading,
                onClick = onBackfill,
            )
        }
        MagicianTextField(
            value = draft,
            onValueChange = { draft = it.take(PUBLISHED_NOTES_SEARCH_MAX) },
            singleLine = true,
            placeholder = { Text("Search title, task, agent, or tag", color = Muted, fontSize = 13.sp) },
            leadingIcon = { Icon(Icons.Outlined.Search, null, tint = Muted, modifier = Modifier.size(18.dp)) },
            trailingIcon = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (draft.isNotEmpty()) {
                        Icon(
                            Icons.Outlined.Clear, "Clear Published Notes search", tint = Muted,
                            modifier = Modifier.size(34.dp).clickable(role = Role.Button) { draft = ""; onClearSearch() }.padding(8.dp),
                        )
                    }
                    Icon(
                        Icons.AutoMirrored.Outlined.ArrowForward, "Search Published Notes", tint = Coral,
                        modifier = Modifier.size(34.dp).clickable(role = Role.Button, enabled = !state.loading) { onSearch(draft) }.padding(8.dp),
                    )
                }
            },
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            keyboardActions = KeyboardActions(onSearch = { onSearch(draft) }),
            modifier = Modifier.fillMaxWidth(),
        )
        state.success?.let { Text(it, color = Coral, fontSize = 12.sp) }
        state.error?.let { DeckErrorRow(it, onReload) }

        when {
            state.loading && state.items.isEmpty() -> DeckLoadingRow("Loading Published Notes…")
            state.items.isEmpty() && state.loaded -> Column(
                Modifier.fillMaxWidth().padding(vertical = 10.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text("No Published Notes", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    if (state.appliedSearch.isEmpty()) "No completed task pages have been published yet."
                    else "No published task pages match this search.",
                    color = Muted, fontSize = 12.sp,
                )
            }
            else -> Column(
                Modifier.alpha(if (state.loading) 0.58f else 1f),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                state.items.forEach { note ->
                    NoteCard(
                        note = note,
                        promoting = state.promotingTaskId == note.taskId,
                        promoteEnabled = state.promotingTaskId == null,
                        onPromote = { onPromote(note) },
                        onOpen = { onOpen(note) },
                    )
                }
            }
        }

        Row(verticalAlignment = Alignment.CenterVertically) {
            CompactAction(
                label = "Previous", icon = Icons.Outlined.ChevronLeft,
                enabled = pager.canPrevious && !state.loading, onClick = onPrevious,
            )
            Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                Text(pager.pageLabel, color = Ink, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
                Text(pager.rangeLabel, color = Muted, fontSize = 10.sp)
            }
            CompactAction(
                label = "Next", icon = Icons.Outlined.ChevronRight,
                enabled = pager.canNext && !state.loading, onClick = onNext,
            )
        }
    }
}

@Composable
private fun DeckSectionHeaderRefresh(loading: Boolean, onRefresh: () -> Unit) {
    if (loading) {
        androidx.compose.material3.CircularProgressIndicator(color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(16.dp))
    } else {
        Icon(
            Icons.Outlined.Refresh, "Refresh Published Notes", tint = Coral,
            modifier = Modifier.size(32.dp).clickable(role = Role.Button, onClick = onRefresh).padding(7.dp),
        )
    }
}

@Composable
private fun NoteCard(
    note: PublishedTaskNote,
    promoting: Boolean,
    promoteEnabled: Boolean,
    onPromote: () -> Unit,
    onOpen: () -> Unit,
) {
    Surface(
        color = Ground,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.Top) {
                Text(
                    note.title.ifBlank { note.taskId }, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                    maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
                )
                if (note.mode.isNotBlank()) {
                    Spacer(Modifier.width(6.dp))
                    DeckPill(note.mode.uppercase(), Coral)
                }
            }
            Text(
                listOf(note.status, note.agentId, noteSourceDate(note)).filter(String::isNotBlank).joinToString(" · "),
                color = Secondary, fontSize = 11.sp, maxLines = 2, overflow = TextOverflow.Ellipsis,
            )
            Text(
                "Published ${noteDate(note.publishedAt)} · ${note.notePath}",
                color = Muted, fontSize = 10.sp, maxLines = 3, overflow = TextOverflow.Ellipsis,
            )
            if (note.tags.isNotEmpty()) {
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                    note.tags.take(6).forEach { tag -> DeckPill(tag, Secondary) }
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                // Only where there is somewhere to go.
                if (note.isOpenable) {
                    CompactAction(
                        label = "Open in Notes", icon = Icons.AutoMirrored.Outlined.MenuBook,
                        emphasized = true, onClick = onOpen, modifier = Modifier.weight(1f),
                    )
                }
                CompactAction(
                    label = if (promoting) "Creating…" else "Promote to memory",
                    icon = Icons.Outlined.Psychology,
                    enabled = promoteEnabled, onClick = onPromote, modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

private fun noteSourceDate(note: PublishedTaskNote): String =
    note.taskCompletedAt?.takeIf(String::isNotBlank)?.let { "completed ${noteDate(it)}" }
        ?: note.sourceUpdatedAt.takeIf(String::isNotBlank)?.let { "updated ${noteDate(it)}" }
        ?: ""

private fun noteDate(value: String): String = runCatching {
    java.time.OffsetDateTime.parse(value).atZoneSameInstant(java.time.ZoneId.systemDefault())
        .format(java.time.format.DateTimeFormatter.ofPattern("d MMM yyyy, h:mm a"))
}.getOrDefault(value)

/** The drawer's Audio Notes, reachable from the Notes view too. */
@Composable
internal fun AudioNotesLink(onOpen: () -> Unit) {
    val shape = RoundedCornerShape(14.dp)
    Surface(
        color = Panel,
        shape = shape,
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth().clip(shape).clickable(role = Role.Button, onClick = onOpen),
    ) {
        Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
            DeckIconBadge(Icons.Outlined.GraphicEq, activePalette.warning, size = 28, iconSize = 16)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text("Audio notes", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Text("Recordings and transcripts from this phone", color = Muted, fontSize = 11.sp)
            }
            Icon(Icons.Outlined.ChevronRight, null, tint = Muted)
        }
    }
}
