@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.HiddenTodayItem
import ai.magicbeans.magdroid.today.TodayBriefing
import ai.magicbeans.magdroid.today.TodayDigestBullet
import ai.magicbeans.magdroid.today.TodayItem
import ai.magicbeans.magdroid.today.TodayUiState
import ai.magicbeans.magdroid.today.briefingMeta
import ai.magicbeans.magdroid.today.deliverableDateline
import ai.magicbeans.magdroid.today.mastheadDateline
import ai.magicbeans.magdroid.today.mastheadVolume
import ai.magicbeans.magdroid.today.titleCase
import ai.magicbeans.magdroid.today.todayRelativeTime
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Restore
import androidx.compose.material.icons.outlined.Snooze
import androidx.compose.material.icons.outlined.Visibility
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import java.time.LocalDateTime

/**
 * The wall clock for the masthead, re-read every minute so a page left open
 * across midnight moves to the new date, volume and issue.
 */
@Composable
internal fun rememberMastheadClock(): LocalDateTime {
    var now by remember { mutableStateOf(LocalDateTime.now()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(60_000L - (System.currentTimeMillis() % 60_000L) + 50L)
            now = LocalDateTime.now()
        }
    }
    return now
}

// ---------------------------------------------------------------- 1. Masthead

@Composable
internal fun TodayMasthead(modifier: Modifier = Modifier) {
    val date = rememberMastheadClock().toLocalDate()
    // The greeting lives in the top bar and refresh in its actions, so the
    // masthead is the title plus one dateline row.
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            "Today's",
            modifier = Modifier.fillMaxWidth().semantics { heading() },
            style = newsSerif(44.sp, FontWeight.ExtraBold, lineHeight = 48.sp, letterSpacing = (-1).sp),
            textAlign = TextAlign.Center,
        )
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            val meta = newsSerif(10.sp, FontWeight.Medium, color = Secondary, letterSpacing = 1.4.sp)
            Text(mastheadDateline(date), style = meta, maxLines = 1, softWrap = false, modifier = Modifier.weight(1f))
            Text(mastheadVolume(date), style = meta, maxLines = 1, softWrap = false)
        }
        DoubleRule(Modifier.padding(top = 2.dp))
    }
}

// ---------------------------------------------------------------- 3. Lead story

@Composable
internal fun TodayLeadStory(needsYou: List<TodayItem>, onTakeAction: (TodayItem) -> Unit, onOpenAttention: () -> Unit, modifier: Modifier = Modifier) {
    val lead = needsYou.firstOrNull()
    if (lead == null) {
        Row(
            modifier.fillMaxWidth()
                .background(TodaySuccess.copy(alpha = .08f), RoundedCornerShape(8.dp))
                .border(1.dp, TodaySuccess.copy(alpha = .3f), RoundedCornerShape(8.dp))
                .padding(horizontal = 12.dp, vertical = 9.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("☕", fontSize = 16.sp)
            Text(
                androidx.compose.ui.text.buildAnnotatedString {
                    pushStyle(androidx.compose.ui.text.SpanStyle(fontWeight = FontWeight.Bold, color = Ink))
                    append("Slate is Clear"); pop()
                    append(" · Nothing needs attention.")
                },
                color = Secondary, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        }
        return
    }
    Column(
        modifier.fillMaxWidth()
            .background(Coral.copy(alpha = .06f), RoundedCornerShape(8.dp))
            .border(1.dp, Coral.copy(alpha = .35f), RoundedCornerShape(8.dp))
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(7.dp),
    ) {
        NewsKicker("The Lead Story · Urgent Decision", color = Coral)
        Text(lead.title, style = newsSerif(24.sp, FontWeight.Bold, lineHeight = 29.sp))
        (lead.reason.takeIf(String::isNotBlank) ?: lead.summary)?.let { Text(it, color = Secondary, fontSize = 14.sp, lineHeight = 20.sp) }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Button(onClick = { onTakeAction(lead) }, shape = MagicanButtonShape,
                colors = ButtonDefaults.buttonColors(containerColor = Coral, contentColor = OnAccent)) { Text("Take action now →") }
            if (needsYou.size > 1) {
                TextButton(onClick = onOpenAttention) { Text("+${needsYou.size - 1} more urgent →", color = Coral) }
            }
        }
    }
}

// ---------------------------------------------------------------- 5c. Hidden drawer

@Composable
internal fun TodayHiddenDrawer(
    items: List<HiddenTodayItem>, lastHidden: HiddenTodayItem?, pending: Set<String>, error: String?,
    onRestore: (HiddenTodayItem) -> Unit, onUndo: () -> Unit, modifier: Modifier = Modifier,
) {
    if (items.isEmpty() && lastHidden == null && error == null) return
    var expanded by remember { mutableStateOf(false) }
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(Modifier.fillMaxWidth().clickable { expanded = !expanded }.padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("${items.size} hidden · ${if (expanded) "Hide" else "Show"}", color = Secondary, fontSize = 12.sp, modifier = Modifier.weight(1f))
            Icon(if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, null, tint = Muted, modifier = Modifier.size(18.dp))
        }
        error?.let { TodayInlineError(it) }
        if (expanded) items.forEach { item ->
            Row(
                Modifier.fillMaxWidth().background(Panel, RoundedCornerShape(8.dp)).border(1.dp, BorderSoft, RoundedCornerShape(8.dp)).padding(10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(if (item.hiddenKind == "snoozed") Icons.Outlined.Snooze else Icons.Outlined.Visibility, null, tint = Muted, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(9.dp))
                Column(Modifier.weight(1f)) {
                    Text(item.record.snapshot?.title ?: "Hidden item", color = Ink, fontSize = 13.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    Text(titleCase(item.hiddenKind), color = Muted, fontSize = 10.sp)
                }
                TextButton(onClick = { onRestore(item) }, enabled = "restore:${item.id}" !in pending) { Text("Restore") }
            }
        }
        lastHidden?.let {
            TextButton(onClick = onUndo) { Icon(Icons.Outlined.Restore, null, Modifier.size(16.dp)); Text(" Undo ${it.hiddenKind}") }
        }
    }
}

// ---------------------------------------------------------------- 7. § 3 Special reports

@Composable
internal fun TodaySpecialReports(
    briefings: List<TodayBriefing>, error: String?, showAll: Boolean,
    onOpen: (TodayBriefing) -> Unit, onViewAll: () -> Unit, onRetry: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        NewsSectionBanner(
            marker = "§ 3",
            title = "Special Reports & Briefings",
            subtitle = "Curated research dossiers, project briefs, and executive syntheses.",
            modifier = Modifier.padding(horizontal = 16.dp),
        ) { TextButton(onClick = onViewAll) { Text("View all →", color = Coral, fontSize = 12.sp) } }
        error?.let { Box(Modifier.padding(horizontal = 16.dp)) { TodayInlineError(it, onRetry) } }
        val shown = if (showAll) briefings else briefings.take(6)
        if (shown.isEmpty()) {
            Box(Modifier.padding(horizontal = 16.dp)) { BroadsheetEmpty("No special reports published today. Press room is clear.") }
        } else {
            LazyRow(
                contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                items(shown, key = TodayBriefing::id) { briefing -> SpecialReportCard(briefing) { onOpen(briefing) } }
            }
        }
    }
}

@Composable
private fun SpecialReportCard(briefing: TodayBriefing, onOpen: () -> Unit) {
    Column(
        Modifier.width(264.dp).height(210.dp)
            .background(Panel, RoundedCornerShape(10.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(10.dp))
            .clickable(onClick = onOpen)
            .padding(13.dp),
        verticalArrangement = Arrangement.spacedBy(5.dp),
    ) {
        NewsKicker("Special Edition", color = Coral)
        briefingMeta(briefing).takeIf(String::isNotEmpty)?.let { Text(it, color = Muted, fontSize = 10.sp, maxLines = 1, overflow = TextOverflow.Ellipsis) }
        Text(briefing.surface.title, style = newsSerif(17.sp, FontWeight.SemiBold, lineHeight = 21.sp), maxLines = 2, overflow = TextOverflow.Ellipsis)
        briefing.surface.summary?.takeIf(String::isNotBlank)?.let {
            Text(it, color = Secondary, fontSize = 12.sp, maxLines = 3, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
        }
        Spacer(Modifier.weight(1f))
        Text("Read report →", color = Coral, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
    }
}

// ---------------------------------------------------------------- 8. § 4 Deliverables

/** Bare count, right-aligned in the banner; the § title already names what it counts. */
internal fun deliverableCountLabel(count: Int): String = "$count"

@Composable
internal fun TodayDeliverablesBanner(count: Int, modifier: Modifier = Modifier) {
    NewsSectionBanner(
        marker = "§ 4",
        title = "Completed Deliverables",
        subtitle = "Official records and signed-off deliverables ready for review.",
        modifier = modifier,
    ) {
        Spacer(Modifier.size(8.dp))
        NewsKicker(deliverableCountLabel(count), color = Muted, modifier = Modifier.semantics { contentDescription = "$count deliverables" })
    }
}

@Composable
internal fun TodayDeliverableCard(item: TodayItem, busy: Boolean, onInspect: () -> Unit, onAcknowledge: () -> Unit, modifier: Modifier = Modifier) {
    Column(
        modifier.fillMaxWidth()
            .background(Panel, RoundedCornerShape(8.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(8.dp))
            .padding(13.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            NewsKicker(deliverableDateline(item), color = Muted, modifier = Modifier.weight(1f))
            Text(todayRelativeTime(item.updatedAt.takeIf { it > 0 } ?: item.createdAt), color = Muted, fontSize = 10.sp)
        }
        Text(item.title, style = newsSerif(18.sp, FontWeight.SemiBold, lineHeight = 22.sp), maxLines = 3, overflow = TextOverflow.Ellipsis)
        (item.summary?.takeIf(String::isNotBlank) ?: item.reason.takeIf(String::isNotBlank))?.let {
            Text(it, color = Secondary, fontSize = 13.sp, lineHeight = 19.sp, maxLines = 4, overflow = TextOverflow.Ellipsis)
        }
        Hairline()
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "✓ RESOLVED",
                modifier = Modifier.border(1.5.dp, TodaySuccess, RoundedCornerShape(4.dp)).padding(horizontal = 6.dp, vertical = 2.dp),
                style = newsSerif(12.sp, FontWeight.ExtraBold, color = TodaySuccess, letterSpacing = 1.sp),
            )
            Spacer(Modifier.weight(1f))
            TextButton(onClick = onAcknowledge, enabled = !busy) { Text("Acknowledge", color = Secondary, fontSize = 12.sp) }
            TextButton(onClick = onInspect, enabled = !busy) { Text("Inspect →", color = Coral, fontSize = 12.sp) }
        }
    }
}

// ---------------------------------------------------------------- 9. § 5 Chronicle

@Composable
internal fun TodayChronicle(
    state: TodayUiState,
    onRefresh: () -> Unit,
    onPage: (Int) -> Unit,
    onOpen: (TodayDigestBullet) -> Unit,
    modifier: Modifier = Modifier,
) {
    val digest = state.digest
    val busy = "digest" in state.loadingMore
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        NewsSectionBanner(
            marker = "§ 5",
            title = "The Chronicle & Digest",
            subtitle = "Automated ledger of state changes, memory saves, and task updates across your spaces.",
        ) { TextButton(onClick = onRefresh, enabled = !busy) { Text("↻ Refresh digest", color = Coral, fontSize = 12.sp) } }
        state.sectionErrors["digest"]?.let { TodayInlineError(it) { onPage(state.digestOffset) } }
        digest.bullets.forEach { bullet ->
            Row(
                Modifier.fillMaxWidth().clickable { onOpen(bullet) }.padding(vertical = 6.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.Top,
            ) {
                Text("▪", color = Coral, fontSize = 12.sp)
                Column(Modifier.weight(1f)) {
                    Text(bullet.text, color = Ink, fontSize = 13.sp, lineHeight = 18.sp)
                    Text(bullet.sourceKind.replace('_', ' '), color = Muted, fontSize = 10.sp)
                }
                Text("→", color = Muted, fontSize = 13.sp)
            }
            Hairline()
        }
        val limit = digest.limit.takeIf { it > 0 } ?: ai.magicbeans.magdroid.today.TODAY_DIGEST_PAGE
        if (digest.total > limit) Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            val start = state.digestOffset + 1
            val end = (state.digestOffset + digest.bullets.size).coerceAtMost(digest.total)
            Text("$start–$end of ${digest.total}", color = Muted, fontSize = 11.sp, modifier = Modifier.weight(1f))
            TextButton(onClick = { onPage((state.digestOffset - limit).coerceAtLeast(0)) }, enabled = state.digestOffset > 0 && !busy) { Text("Newer") }
            TextButton(onClick = { onPage(state.digestOffset + limit) }, enabled = state.digestOffset + limit < digest.total && !busy) { Text("Older") }
        }
    }
}

// ---------------------------------------------------------------- 10. Footer

@Composable
internal fun TodayFooter(modifier: Modifier = Modifier) {
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        DoubleRule()
        Text(
            "TODAY'S · MORNING EDITION",
            modifier = Modifier.fillMaxWidth(),
            style = newsSerif(11.sp, FontWeight.SemiBold, color = Muted, letterSpacing = 2.sp),
            textAlign = TextAlign.Center,
        )
    }
}
