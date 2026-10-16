package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.meetings.ActiveMeeting
import ai.magicbeans.magdroid.meetings.MeetingsUiState
import ai.magicbeans.magdroid.meetings.TranscriptLine
import ai.magicbeans.magdroid.observe.ObserveState
import ai.magicbeans.magdroid.voice.AmbientPowerBlock
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ScreenShare
import androidx.compose.material.icons.outlined.BatteryAlert
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.Hearing
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The LIVE block: above the views, on every view, whenever something is live
 * or starting. The cards are the former Now section, unchanged in what they
 * offer — the in-app capture hero (timer, summary, live transcript, Share /
 * Stop screen, Stop, Open transcript), every other server session (Stop, Open
 * transcript), and the active-rail failure/Retry and action error.
 */
@Composable
internal fun ObserveLiveBlock(
    localState: ObserveState,
    chunks: Long,
    sharing: Boolean,
    powerWarning: AmbientPowerBlock?,
    localThreadId: String?,
    localMeeting: ActiveMeeting?,
    localTranscript: List<TranscriptLine>,
    otherActive: List<ActiveMeeting>,
    transcriptState: MeetingsUiState,
    onOpenThread: (String) -> Unit,
    onStopLocal: () -> Unit,
    onToggleScreenShare: () -> Unit,
    onStopRemote: (ActiveMeeting) -> Unit,
    onRetryActive: () -> Unit,
) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(7.dp).background(Danger, CircleShape))
            Spacer(Modifier.width(7.dp))
            DeckLabel("Live", color = Danger)
        }
        if (localState == ObserveState.Starting || localState == ObserveState.Listening) {
            ObserveCard(accent = Danger.copy(alpha = 0.45f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(
                        Modifier.size(46.dp).background(Danger.copy(alpha = 0.13f), CircleShape),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(Icons.Outlined.Hearing, null, tint = Danger, modifier = Modifier.size(25.dp))
                    }
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            if (localState == ObserveState.Starting) "Starting…" else "Listening",
                            color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            if (localState == ObserveState.Starting) "Opening the meeting thread"
                            else "${observeElapsed(chunks)} captured on this phone",
                            color = Secondary, fontSize = 12.sp,
                        )
                    }
                    Box(Modifier.size(9.dp).background(Danger, CircleShape))
                }
                // Published by the service on every chunk and never shown
                // before: capture keeps going on low battery by design, so the
                // owner has to be told rather than stopped.
                powerWarning?.let { warning ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(Icons.Outlined.BatteryAlert, null, tint = MWarn, modifier = Modifier.size(15.dp))
                        Spacer(Modifier.width(6.dp))
                        Text(
                            "${warning.shortLabel} — capture continues until you stop it.",
                            color = MWarn, fontSize = 11.sp, lineHeight = 15.sp,
                        )
                    }
                }
                localMeeting?.latestSummary?.takeIf(String::isNotBlank)?.let {
                    Text(it, color = Secondary, fontSize = 12.sp, lineHeight = 17.sp, maxLines = 4)
                }
                if (localTranscript.isNotEmpty()) TranscriptPanel(localTranscript)
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (localState == ObserveState.Listening) {
                        CompactAction(
                            label = if (sharing) "Stop screen" else "Share screen",
                            icon = Icons.AutoMirrored.Outlined.ScreenShare,
                            selected = sharing,
                            onClick = onToggleScreenShare,
                            modifier = Modifier.weight(1f),
                        )
                    }
                    CompactAction(
                        label = "Stop",
                        icon = Icons.Outlined.StopCircle,
                        danger = true,
                        onClick = onStopLocal,
                        modifier = Modifier.weight(1f),
                    )
                }
                localThreadId?.takeIf(String::isNotBlank)?.let { thread ->
                    CompactAction(
                        label = "Open transcript",
                        icon = Icons.Outlined.Forum,
                        onClick = { onOpenThread(thread) },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        }
        otherActive.forEach { meeting ->
            ObserveCard(accent = Danger.copy(alpha = 0.35f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.size(8.dp).background(if (meeting.paused) MWarn else Danger, CircleShape))
                    Spacer(Modifier.width(9.dp))
                    Text(
                        meeting.displayTitle, color = Ink, fontSize = 14.sp,
                        fontWeight = FontWeight.SemiBold, maxLines = 1,
                        overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
                    )
                    Text(
                        if (meeting.isAttendee) "Bot" else "Listener",
                        color = Secondary, fontSize = 11.sp,
                    )
                }
                meeting.latestSummary?.takeIf(String::isNotBlank)?.let {
                    Text(it, color = Secondary, fontSize = 12.sp, maxLines = 3)
                }
                if (meeting.sessionId == transcriptState.transcriptSessionId &&
                    transcriptState.transcript.isNotEmpty()
                ) {
                    TranscriptPanel(transcriptState.transcript)
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    meeting.threadId?.takeIf(String::isNotBlank)?.let { thread ->
                        CompactAction(
                            label = "Open transcript", icon = Icons.Outlined.Forum,
                            onClick = { onOpenThread(thread) }, modifier = Modifier.weight(1f),
                        )
                    }
                    CompactAction(
                        label = "Stop", icon = Icons.Outlined.StopCircle, danger = true,
                        onClick = { onStopRemote(meeting) }, modifier = Modifier.weight(1f),
                    )
                }
            }
        }
        transcriptState.activeFailure?.let { failure ->
            InlineFailure(failure, onRetry = onRetryActive)
        }
        transcriptState.activeError?.let { Text(it, color = Danger, fontSize = 11.sp) }
    }
}

/**
 * The meeting as it is being said.
 *
 * Newest last and scrolled to the bottom, because a transcript is read
 * forwards. Bounded height: it sits inside a meeting card, and a long meeting
 * must not push the controls that end it off the screen.
 */
@Composable
internal fun TranscriptPanel(lines: List<TranscriptLine>) {
    val listState = rememberLazyListState()
    LaunchedEffect(lines.size) {
        if (lines.isNotEmpty()) listState.animateScrollToItem(lines.lastIndex)
    }
    Surface(
        color = Ground,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            DeckLabel("Live transcript")
            LazyColumn(
                state = listState,
                modifier = Modifier.heightIn(max = 200.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                items(lines, key = { it.id }) { line ->
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        // Absent rather than blank when the line carried no
                        // attributable prefix: an empty column would read as a
                        // speaker whose name failed to load.
                        line.speaker?.let {
                            Text(it, color = Coral, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
                        }
                        Text(line.text, color = Ink, fontSize = 12.sp)
                    }
                }
            }
        }
    }
}
