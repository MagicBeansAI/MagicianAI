package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.meetings.MeetingsUiState
import ai.magicbeans.magdroid.observe.DeckLane
import ai.magicbeans.magdroid.observe.RecentMeeting
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.automirrored.outlined.ScreenShare
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.Hearing
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.PersonAdd
import androidx.compose.material.icons.outlined.Troubleshoot
import androidx.compose.material.icons.outlined.VideoCall
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Capture launchpad: Listen / Join as agent / Share screen / Brainstorm.
 * One form open at a time; the tile toggles its form.
 */
@Composable
internal fun CaptureLaunchpad(
    localLive: Boolean,
    listening: Boolean,
    sharing: Boolean,
    meetingState: MeetingsUiState,
    listenTitle: String,
    onListenTitleChange: (String) -> Unit,
    onListen: () -> Unit,
    onOpenUrl: (String) -> Unit,
    onSendBot: (String) -> Unit,
    onToggleScreenShare: () -> Unit,
    onBrainstorm: () -> Unit,
) {
    var open by remember { mutableStateOf<LaunchTile?>(null) }
    // A tile that disappears (Listen, once this phone is live) closes its form.
    val tiles = launchpadTiles(localLive)
    if (open != null && open !in tiles) open = null

    ObserveCard {
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text("Capture launchpad", color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            Text(
                "Instant triggers to listen, join as agent, or share screen",
                color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            tiles.chunked(2).forEach { row ->
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    row.forEach { tile ->
                        LaunchTileView(
                            tile = tile,
                            open = open == tile,
                            sharing = sharing,
                            onClick = {
                                when {
                                    tile == LaunchTile.Brainstorm -> onBrainstorm()
                                    // While listening the share toggle is valid
                                    // right away; there is nothing to fill in.
                                    tile == LaunchTile.ShareScreen && listening -> onToggleScreenShare()
                                    else -> open = toggleLaunchTile(open, tile)
                                }
                            },
                            modifier = Modifier.weight(1f),
                        )
                    }
                    if (row.size == 1) Spacer(Modifier.weight(1f))
                }
            }
        }
        when (open) {
            LaunchTile.Listen -> RoomCaptureForm(listenTitle, onListenTitleChange, onListen)
            LaunchTile.JoinAgent -> BotJoinForm(
                busy = meetingState.joining,
                problem = meetingState.joinError,
                onOpenUrl = onOpenUrl,
                onSendBot = onSendBot,
            )
            LaunchTile.ShareScreen -> ShareScreenExplainer(
                localLive = localLive,
                onGoListen = { open = LaunchTile.Listen },
            )
            else -> Unit
        }
    }
}

@Composable
private fun LaunchTileView(
    tile: LaunchTile,
    open: Boolean,
    sharing: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val (icon, tint) = when (tile) {
        LaunchTile.Listen -> Icons.Outlined.Mic to activePalette.success
        LaunchTile.JoinAgent -> Icons.Outlined.PersonAdd to activePalette.info
        LaunchTile.ShareScreen -> Icons.AutoMirrored.Outlined.ScreenShare to (if (sharing) Danger else activePalette.discovery)
        LaunchTile.Brainstorm -> Icons.Outlined.Troubleshoot to Coral
    }
    val title = if (tile == LaunchTile.ShareScreen && sharing) "Stop sharing" else tile.title
    val shape = RoundedCornerShape(10.dp)
    Surface(
        color = if (open) Coral.copy(alpha = 0.08f) else Ground,
        shape = shape,
        border = BorderStroke(1.dp, if (open) Coral else BorderSoft),
        modifier = modifier
            .heightIn(min = 58.dp)
            .clip(shape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { selected = open },
    ) {
        Row(Modifier.padding(horizontal = 10.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            DeckIconBadge(icon, tint, size = 28, iconSize = 16)
            Spacer(Modifier.width(8.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                Text(
                    tile.subtitle, color = Muted, fontSize = 10.sp, lineHeight = 13.sp,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/** The former "Listen to this room" card, as the Listen tile's form. */
@Composable
private fun RoomCaptureForm(title: String, onTitleChange: (String) -> Unit, onStart: () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(9.dp)) {
        DeckDivider()
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Outlined.Hearing, null, tint = Coral, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
            Text("Listen to this room", color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        }
        Text(
            "No calendar event? Put your phone on the table and Magician will build a live transcript and notes.",
            color = Secondary, fontSize = 12.sp, lineHeight = 17.sp,
        )
        MagicianTextField(
            value = title,
            onValueChange = onTitleChange,
            singleLine = true,
            placeholder = { Text("Title (optional)", color = Muted, fontSize = 13.sp) },
            modifier = Modifier.fillMaxWidth(),
        )
        CompactAction(
            label = "Listen here", icon = Icons.Outlined.Mic,
            emphasized = true, onClick = onStart, modifier = Modifier.fillMaxWidth(),
        )
        Text(
            "Audio is sent to Magician while the persistent recording notification is visible. " +
                "After listening starts, screen sharing is available in the live card.",
            color = Muted, fontSize = 10.sp, lineHeight = 15.sp,
        )
    }
}

/** The former "Join a Google Meet as the bot" card, as the Join-as-agent form. */
@Composable
private fun BotJoinForm(
    busy: Boolean,
    problem: String?,
    onOpenUrl: (String) -> Unit,
    onSendBot: (String) -> Unit,
) {
    var url by remember { mutableStateOf("") }
    Column(verticalArrangement = Arrangement.spacedBy(9.dp)) {
        DeckDivider()
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Outlined.PersonAdd, null, tint = Coral, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
            Text("Join a Google Meet as the bot", color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        }
        Text(
            "Send the agent to attend a Google Meet directly and take notes.",
            color = Secondary, fontSize = 12.sp,
        )
        MagicianTextField(
            value = url,
            onValueChange = { url = it },
            enabled = !busy,
            singleLine = true,
            placeholder = { Text("meet.google.com/…", color = Muted, fontSize = 13.sp) },
            modifier = Modifier.fillMaxWidth(),
        )
        problem?.let { Text(it, color = Danger, fontSize = 11.sp) }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            CompactAction(
                label = "Join (Me)", icon = Icons.AutoMirrored.Outlined.OpenInNew,
                enabled = url.isNotBlank() && !busy, onClick = { onOpenUrl(url) },
                modifier = Modifier.weight(1f),
            )
            CompactAction(
                label = if (busy) "Sending…" else "Send bot",
                icon = Icons.Outlined.PersonAdd,
                emphasized = true, enabled = url.isNotBlank() && !busy,
                onClick = { onSendBot(url); url = "" },
                modifier = Modifier.weight(1f),
            )
        }
    }
}

/**
 * Screen sharing rides a running capture (the frames land in its thread), so
 * before listening there is nothing to share into. Said plainly rather than
 * starting a recording the owner did not ask for.
 */
@Composable
private fun ShareScreenExplainer(localLive: Boolean, onGoListen: () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(9.dp)) {
        DeckDivider()
        Text(
            if (localLive) "Listening is starting — Share screen appears on the live card as soon as it is running."
            else "Start listening first, then share your screen from the live card.",
            color = Secondary, fontSize = 12.sp, lineHeight = 17.sp,
        )
        if (!localLive) {
            CompactAction(
                label = "Set up listening", icon = Icons.Outlined.Mic,
                onClick = onGoListen, modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

/** Upcoming calendar meetings with every row action, plus loading/failure. */
@Composable
internal fun UpcomingSection(
    state: MeetingsUiState,
    localListening: Boolean,
    onOpenUrl: (String) -> Unit,
    onListen: (String?, String?) -> Unit,
    onSendBot: (String, String?) -> Unit,
    onOpenThread: (String) -> Unit,
    onRefresh: () -> Unit,
) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        DeckSectionHeader(
            title = "Upcoming",
            loading = state.loadingUpcoming,
            onRefresh = onRefresh,
        )
        state.upcomingFailure?.let { InlineFailure(it, onRetry = onRefresh) }
        if (state.upcoming.isEmpty() && state.loadingUpcoming) {
            DeckLoadingRow("Reading your calendar…")
        }
        if (state.upcoming.isEmpty() && state.upcomingFailure == null && !state.loadingUpcoming) {
            Text("No meetings on your calendar in the next few hours.", color = Muted, fontSize = 12.sp)
        }
        state.upcoming.forEach { meeting ->
            ObserveCard {
                Row(verticalAlignment = Alignment.Top) {
                    Column(Modifier.weight(1f)) {
                        Text(
                            meeting.title, color = Ink, fontSize = 13.sp,
                            fontWeight = FontWeight.SemiBold, maxLines = 2,
                        )
                        Text(
                            listOfNotNull(meetingWindowText(meeting.start, meeting.end), meeting.account)
                                .filter(String::isNotBlank).joinToString(" · "),
                            color = Muted, fontSize = 11.sp, maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    if (meeting.liveNow) DeckPill("NOW", Danger)
                }
                val active = activeSessionForUpcoming(meeting, state.visibleActive)
                if (active != null) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            if (active.isAttendee) Icons.Outlined.PersonAdd else Icons.Outlined.Hearing,
                            null, tint = Coral, modifier = Modifier.size(17.dp),
                        )
                        Spacer(Modifier.width(7.dp))
                        Text(
                            if (active.isAttendee) "Bot joined" else "Listening now",
                            color = Secondary, fontSize = 11.sp, modifier = Modifier.weight(1f),
                        )
                        active.threadId?.takeIf(String::isNotBlank)?.let { thread ->
                            CompactAction(label = "Open", icon = Icons.Outlined.Forum, onClick = { onOpenThread(thread) })
                        }
                    }
                } else {
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                        if (meeting.isJoinable) {
                            CompactAction(
                                label = "Join (Me)", icon = Icons.Outlined.VideoCall,
                                onClick = { onOpenUrl(meeting.meetUrl.orEmpty()) },
                                modifier = Modifier.weight(1f),
                            )
                        }
                        CompactAction(
                            label = "Listen", icon = Icons.Outlined.Hearing,
                            enabled = !localListening,
                            onClick = { onListen(meeting.title, meeting.meetUrl) },
                            modifier = Modifier.weight(1f),
                        )
                        if (meeting.isJoinable) {
                            CompactAction(
                                label = "Send bot", icon = Icons.Outlined.PersonAdd,
                                emphasized = true, enabled = !state.joining,
                                onClick = { onSendBot(meeting.meetUrl.orEmpty(), meeting.title) },
                                modifier = Modifier.weight(1f),
                            )
                        }
                    }
                }
            }
        }
        // A Send bot from a row fails here too, not only from the form.
        state.joinError?.let { Text(it, color = Danger, fontSize = 11.sp) }
    }
}

/** Recent captures from `GET /meetings`, newest first; a row opens its transcript thread. */
@Composable
internal fun RecentSection(lane: DeckLane<List<RecentMeeting>>, onOpenThread: (String) -> Unit, onRetry: () -> Unit) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        DeckSectionHeader(title = "Recent", hint = "Meeting captures, newest first", loading = lane.loading, onRefresh = onRetry)
        lane.error?.let { DeckErrorRow(it, onRetry) }
        val rows = lane.value
        when {
            rows == null && lane.loading -> DeckLoadingRow("Loading recent captures…")
            rows != null && rows.isEmpty() -> Text("Nothing captured yet", color = Muted, fontSize = 12.sp)
            rows != null -> Surface(
                color = Panel,
                shape = RoundedCornerShape(14.dp),
                border = BorderStroke(1.dp, BorderSoft),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column {
                    rows.forEachIndexed { index, row ->
                        if (index > 0) DeckDivider()
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .clickable(role = Role.Button) { onOpenThread(row.threadId) }
                                .padding(horizontal = 14.dp, vertical = 11.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Surface(color = Coral.copy(alpha = 0.12f), shape = CircleShape, modifier = Modifier.size(28.dp)) {
                                Icon(Icons.Outlined.Hearing, null, tint = Coral, modifier = Modifier.padding(6.dp))
                            }
                            Spacer(Modifier.width(10.dp))
                            Column(Modifier.weight(1f)) {
                                Text(
                                    row.displayTitle, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Medium,
                                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                                )
                                Text(
                                    listOfNotNull(
                                        relativeWhen(row.updatedAtMillis).takeIf(String::isNotBlank),
                                        row.mode?.takeIf(String::isNotBlank),
                                        row.agentId?.takeIf(String::isNotBlank),
                                    ).joinToString(" · "),
                                    color = Muted, fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                                )
                            }
                            Icon(Icons.Outlined.ChevronRight, "Open transcript", tint = Muted, modifier = Modifier.size(18.dp))
                        }
                    }
                }
            }
        }
    }
}
