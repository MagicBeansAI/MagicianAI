@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.ChannelActionDescriptor
import ai.magicbeans.magdroid.today.ChannelFollowUp
import ai.magicbeans.magdroid.today.ResurfacingActionKind
import ai.magicbeans.magdroid.today.ResurfacingCard
import ai.magicbeans.magdroid.today.ResurfacingFeedbackAction
import ai.magicbeans.magdroid.today.TodayAction
import ai.magicbeans.magdroid.today.TodayItem
import ai.magicbeans.magdroid.today.TodayUiState
import ai.magicbeans.magdroid.today.TodayViewModel
import ai.magicbeans.magdroid.today.dispatchCategory
import ai.magicbeans.magdroid.today.readingRoomCategory
import ai.magicbeans.magdroid.today.TodayBroadsheetPage
import ai.magicbeans.magdroid.today.TodayBroadsheetTab
import ai.magicbeans.magdroid.today.broadsheetRangeLabel
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.Snooze
import androidx.compose.material.icons.outlined.ThumbUp
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/*
 * Broadsheet columns: "For You" (core follow-ups + channel follow-ups) and
 * "Worth a look" (resurfacing). The existing cards, swipe rails, menus and
 * paging, restyled as newspaper items.
 */

/** Fixed height for a broadsheet tab's card area; it scrolls on its own. */
internal val BROADSHEET_CONTAINER_HEIGHT = 460.dp

@Composable
internal fun BroadsheetTabs(
    selected: TodayBroadsheetTab,
    forYouCount: Int,
    worthCount: Int,
    onSelect: (TodayBroadsheetTab) -> Unit,
) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        BroadsheetTabChip("For You", forYouCount, selected == TodayBroadsheetTab.ForYou) { onSelect(TodayBroadsheetTab.ForYou) }
        BroadsheetTabChip("Worth a Look", worthCount, selected == TodayBroadsheetTab.Worth) { onSelect(TodayBroadsheetTab.Worth) }
    }
}

/** Same themed rounded-rect chip as the deck tabs. */
@Composable
private fun BroadsheetTabChip(label: String, count: Int, selected: Boolean, onClick: () -> Unit) {
    Row(
        Modifier
            .background(if (selected) Coral else Panel, MagicanButtonShape)
            .border(1.dp, if (selected) Coral else BorderSoft, MagicanButtonShape)
            .clickable(onClick = onClick)
            .semantics { this.selected = selected }
            .padding(horizontal = 12.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(label, color = if (selected) OnAccent else Ink, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        if (count > 0) {
            Text(
                "$count",
                modifier = Modifier
                    .background(if (selected) OnAccent.copy(alpha = .22f) else Coral.copy(alpha = .14f), RoundedCornerShape(4.dp))
                    .padding(horizontal = 6.dp, vertical = 1.dp),
                color = if (selected) OnAccent else Coral, fontSize = 10.sp, fontWeight = FontWeight.Bold,
            )
        }
    }
}

/**
 * The tab's cards in a fixed-height box that scrolls independently. A new
 * page starts at the top; the first load shows a spinner instead of an empty box.
 */
@Composable
internal fun BroadsheetScrollContainer(pageKey: String, loading: Boolean, content: @Composable () -> Unit) {
    val scroll = remember(pageKey) { ScrollState(0) }
    Box(
        Modifier.fillMaxWidth().height(BROADSHEET_CONTAINER_HEIGHT)
            .background(Soft.copy(alpha = .35f), RoundedCornerShape(10.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(10.dp)),
    ) {
        if (loading) {
            CircularProgressIndicator(Modifier.align(Alignment.Center).size(24.dp), strokeWidth = 2.dp, color = Coral)
        } else {
            Column(
                Modifier.fillMaxSize().verticalScroll(scroll).padding(8.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) { content() }
        }
    }
}

/** Server-side pager (web ServerPager): range, previous, page of pages, next. */
@Composable
internal fun BroadsheetPager(page: TodayBroadsheetPage<*>, label: String, onPage: (Int) -> Unit) {
    Row(
        Modifier.fillMaxWidth().semantics { contentDescription = "$label pagination" },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(broadsheetRangeLabel(page), color = Muted, fontFamily = newsMono(), fontSize = 11.sp, modifier = Modifier.weight(1f))
        if (page.loading && page.loaded) CircularProgressIndicator(Modifier.size(14.dp).padding(end = 4.dp), strokeWidth = 2.dp, color = Coral)
        IconButton(onClick = { onPage(page.page - 1) }, enabled = page.hasPrevious && !page.loading) {
            Icon(Icons.AutoMirrored.Outlined.KeyboardArrowLeft, "Previous page", tint = if (page.hasPrevious) Coral else BorderSoft)
        }
        Text("${page.page} / ${page.pageCount}", color = Ink, fontFamily = newsMono(), fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        IconButton(onClick = { onPage(page.page + 1) }, enabled = page.hasNext && !page.loading) {
            Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, "Next page", tint = if (page.hasNext) Coral else BorderSoft)
        }
    }
}

internal fun forYouCountLabel(count: Int): String = "$count item${if (count == 1) "" else "s"}"
internal fun worthCountLabel(count: Int): String = "$count spark${if (count == 1) "" else "s"}"

@Composable
internal fun BroadsheetColumnHeader(title: String, count: String, subtitle: String) {
    Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Row(verticalAlignment = Alignment.Bottom) {
            Text(title, style = newsSerif(20.sp, FontWeight.Bold), modifier = Modifier.weight(1f))
            NewsKicker(count, color = Muted)
        }
        Text(subtitle, style = newsSerif(13.sp, FontWeight.Normal, italic = true, color = Secondary))
        Hairline(Modifier.padding(top = 3.dp))
    }
}

@Composable
internal fun BroadsheetEmpty(text: String) {
    Box(
        Modifier.fillMaxWidth().border(1.dp, BorderSoft, RoundedCornerShape(8.dp)).padding(16.dp),
        contentAlignment = Alignment.Center,
    ) { Text(text, style = newsSerif(14.sp, FontWeight.Normal, italic = true, color = Muted)) }
}

@Composable
internal fun TodayLoadMore(busy: Boolean, action: () -> Unit) = OutlinedButton(
    shape = MagicanButtonShape, onClick = action, enabled = !busy, modifier = Modifier.fillMaxWidth(),
) {
    if (busy) CircularProgressIndicator(Modifier.size(15.dp), strokeWidth = 2.dp) else Icon(Icons.Outlined.ExpandMore, null, Modifier.size(16.dp))
    Text(" Load more")
}

@Composable
private fun NewsCardFrame(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(
        modifier.fillMaxWidth()
            .background(Panel, RoundedCornerShape(10.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(10.dp))
            .padding(13.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) { content() }
}

@Composable
private fun CardAction(label: String, color: Color = Coral, enabled: Boolean = true, icon: androidx.compose.ui.graphics.vector.ImageVector? = null, onClick: () -> Unit) {
    TextButton(onClick = onClick, enabled = enabled, contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 8.dp, vertical = 2.dp)) {
        icon?.let { Icon(it, null, Modifier.size(14.dp), tint = color) }
        Text(if (icon != null) " $label" else label, color = color, fontSize = 12.sp)
    }
}

// ---------------------------------------------------------------- Core Today follow-ups

@Composable
internal fun BroadsheetTodayItemCard(
    item: TodayItem, state: TodayUiState, viewModel: TodayViewModel,
    onOpen: (TodayItem) -> Unit, onSnooze: (TodayItem) -> Unit,
) {
    val busy = state.isTodayItemMutationPending(item.id)
    TodaySwipeActionCard(
        itemId = item.id,
        leadingActions = listOf(TodaySwipeAction("open", "Open", Icons.AutoMirrored.Outlined.OpenInNew, Coral) { onOpen(item) }),
        trailingActions = listOf(TodaySwipeAction("dismiss", "Dismiss", Icons.Outlined.Close, Danger) { viewModel.hide(item, "dismiss") }),
        enabled = !busy,
    ) {
        TodayItemCard(item, busy, onOpen = { onOpen(item) }, onDismiss = { viewModel.hide(item, "dismiss") },
            onSnooze = { onSnooze(item) }, onAction = { viewModel.execute(it, item) })
    }
}

@Composable
private fun TodayItemCard(item: TodayItem, busy: Boolean, onOpen: () -> Unit, onDismiss: () -> Unit, onSnooze: () -> Unit, onAction: (TodayAction) -> Unit) {
    var moreActionsExpanded by remember(item.id) { mutableStateOf(false) }
    val actions = remember(item.executableActions) { partitionTodayActions(item.executableActions) }
    NewsCardFrame(Modifier.clickable(enabled = !busy, onClick = onOpen)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            NewsKicker("FOLLOW-UP · ${item.sourceKind.replace('_', ' ')}", color = todayStatusColor(item.status), modifier = Modifier.weight(1f))
            if (busy) CircularProgressIndicator(Modifier.size(15.dp), strokeWidth = 2.dp, color = Coral)
        }
        Text(item.title, style = newsSerif(18.sp, FontWeight.SemiBold, lineHeight = 22.sp), maxLines = 3, overflow = TextOverflow.Ellipsis)
        item.summary?.let { Text(it, color = Secondary, fontSize = 13.sp, maxLines = 3, overflow = TextOverflow.Ellipsis) }
        if (item.reason.isNotBlank()) Text(item.reason, color = Muted, fontSize = 11.sp)
        if (item.learnedItems.isNotEmpty()) {
            Column(Modifier.background(TodayDiscovery.copy(alpha = .08f), RoundedCornerShape(8.dp)).padding(8.dp)) {
                NewsKicker("Learned", color = TodayDiscovery, size = 9.sp)
                item.learnedItems.forEach { Text("• ${it.title}", color = Ink, fontSize = 11.sp) }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(2.dp), verticalAlignment = Alignment.CenterVertically) {
            actions.inline.forEach { action -> CardAction(action.label, enabled = !busy) { onAction(action) } }
            if (actions.overflow.isNotEmpty()) {
                Box {
                    IconButton(onClick = { moreActionsExpanded = true }, enabled = !busy, modifier = Modifier.size(34.dp)) {
                        Icon(Icons.Outlined.MoreHoriz, "More actions", tint = Coral, modifier = Modifier.size(18.dp))
                    }
                    DropdownMenu(expanded = moreActionsExpanded, onDismissRequest = { moreActionsExpanded = false }) {
                        actions.overflow.forEach { action ->
                            DropdownMenuItem(text = { Text(action.label) }, onClick = { moreActionsExpanded = false; onAction(action) }, enabled = !busy)
                        }
                    }
                }
            }
            Spacer(Modifier.weight(1f))
            IconButton(onClick = onSnooze, enabled = !busy, modifier = Modifier.size(34.dp)) { Icon(Icons.Outlined.Snooze, "Snooze", tint = Muted, modifier = Modifier.size(18.dp)) }
            IconButton(onClick = onDismiss, enabled = !busy, modifier = Modifier.size(34.dp)) { Icon(Icons.Outlined.Close, "Dismiss", tint = Muted, modifier = Modifier.size(18.dp)) }
        }
    }
}

// ---------------------------------------------------------------- Channel follow-ups

/** Everything a channel follow-up can do; shared by the card and its detail sheet. */
internal data class FollowUpHandlers(
    val onOpen: (ChannelFollowUp) -> Unit,
    val onDismissWithReason: (ChannelFollowUp) -> Unit,
    val onWriting: (ChannelFollowUp) -> Unit,
    val onCompose: (ChannelFollowUp, ChannelActionDescriptor) -> Unit,
    val onOpenUrl: (String?) -> Unit,
)

@Composable
internal fun BroadsheetFollowUpCard(item: ChannelFollowUp, state: TodayUiState, viewModel: TodayViewModel, handlers: FollowUpHandlers) {
    val busy = state.isFollowUpMutationPending(item.id)
    TodaySwipeActionCard(
        itemId = item.id,
        leadingActions = listOf(TodaySwipeAction("useful", "Useful", Icons.Outlined.ThumbUp, TodaySuccess) { viewModel.resolveFollowUp(item, "useful") }),
        trailingActions = listOf(TodaySwipeAction("dismiss", "Dismiss", Icons.Outlined.Close, Danger) { viewModel.resolveFollowUp(item, "dismiss") }),
        enabled = !busy,
    ) {
        NewsCardFrame(Modifier.clickable { handlers.onOpen(item) }) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                NewsKicker(listOfNotNull(dispatchCategory(item), item.sender?.takeIf(String::isNotBlank)).joinToString(" · "),
                    color = Coral, modifier = Modifier.weight(1f))
                if (busy) CircularProgressIndicator(Modifier.size(15.dp), strokeWidth = 2.dp, color = Coral)
            }
            Text(item.subject?.takeIf(String::isNotBlank) ?: "Untitled Message", style = newsSerif(18.sp, FontWeight.SemiBold, lineHeight = 22.sp),
                maxLines = 3, overflow = TextOverflow.Ellipsis)
            item.accountAlias.takeIf(String::isNotBlank)?.let { Text(it, color = Muted, fontSize = 11.sp) }
            item.summary?.let { Text(it, color = Secondary, fontSize = 13.sp, maxLines = 3, overflow = TextOverflow.Ellipsis) }
            item.actionSummary?.let { Text(it, color = Muted, fontSize = 11.sp) }
            item.reason?.let { Text("Why now · $it", color = TodayWarning, fontSize = 11.sp) }
            FollowUpActions(item, busy, viewModel, handlers)
        }
    }
}

@Composable
internal fun FollowUpActions(item: ChannelFollowUp, busy: Boolean, viewModel: TodayViewModel, handlers: FollowUpHandlers) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
        CardAction("⚡ Do it", enabled = !busy) { viewModel.resolveFollowUp(item, "approve") }
        item.openUrl?.let { url -> CardAction("Open", enabled = true) { handlers.onOpenUrl(url) } }
        CardAction("Useful", TodaySuccess, !busy, Icons.Outlined.ThumbUp) { viewModel.resolveFollowUp(item, "useful") }
        if (item.canAcknowledge) CardAction("Seen", Secondary, !busy, Icons.Outlined.CheckCircle) { viewModel.resolveFollowUp(item, "acknowledge") }
        // Follow-up snooze has no duration on the wire: it hides the card from Today.
        CardAction("Snooze — hide from Today", Secondary, !busy, Icons.Outlined.Snooze) { viewModel.resolveFollowUp(item, "snooze") }
        CardAction("Writing style", Secondary) { handlers.onWriting(item) }
        item.availableActions.forEach { descriptor -> CardAction(descriptor.label, enabled = !busy) { handlers.onCompose(item, descriptor) } }
        CardAction("Dismiss…", Danger, !busy) { handlers.onDismissWithReason(item) }
    }
}

// ---------------------------------------------------------------- Worth a look

@Composable
internal fun BroadsheetWorthCard(
    card: ResurfacingCard, state: TodayUiState, viewModel: TodayViewModel,
    onDetail: (ResurfacingCard) -> Unit,
    onAction: (ResurfacingCard, ResurfacingActionKind) -> Unit,
    onDismissWithReason: (ResurfacingCard) -> Unit,
) {
    val busy = state.isResurfacingMutationPending(card.id)
    TodaySwipeActionCard(
        itemId = card.id,
        leadingActions = listOf(TodaySwipeAction("mark-useful", "Mark useful", Icons.Outlined.ThumbUp, TodaySuccess) {
            viewModel.resolveResurfacing(card, ResurfacingFeedbackAction.Open)
        }),
        trailingActions = listOf(
            TodaySwipeAction("dismiss", "Dismiss", Icons.Outlined.Close, Danger) { viewModel.resolveResurfacing(card, ResurfacingFeedbackAction.Dismiss) },
            TodaySwipeAction("acknowledge", "Acknowledge", Icons.Outlined.CheckCircle, Muted) { viewModel.resolveResurfacing(card, ResurfacingFeedbackAction.Acknowledge) },
        ),
        enabled = !busy,
    ) {
        NewsCardFrame(Modifier.clickable(enabled = !busy) { onDetail(card) }) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                NewsKicker(readingRoomCategory(card), color = TodayDiscovery, modifier = Modifier.weight(1f))
                if (busy) CircularProgressIndicator(Modifier.size(15.dp), strokeWidth = 2.dp, color = Coral)
            }
            Text(card.sourceTitle.ifBlank { card.line }.ifBlank { "Resurfaced Note" }, style = newsSerif(18.sp, FontWeight.SemiBold, lineHeight = 22.sp),
                maxLines = 3, overflow = TextOverflow.Ellipsis)
            if (card.line.isNotBlank() && card.line != card.sourceTitle && card.sourceTitle.isNotBlank()) Text(card.line, color = Ink, fontSize = 13.sp)
            if (card.summary.isNotBlank()) Text(card.summary, color = Secondary, fontSize = 13.sp, maxLines = 3, overflow = TextOverflow.Ellipsis)
            if (card.whyNow.isNotBlank()) Text("Why now · ${card.whyNow}", color = TodayDiscovery, fontSize = 11.sp)
            card.brief?.keyFacts?.take(3)?.forEach { Text("• $it", color = Ink, fontSize = 12.sp) }
            card.recommendedAction?.let { recommendation ->
                Text("Suggested · ${recommendation.label}${recommendation.rationale.takeIf(String::isNotBlank)?.let { " — $it" }.orEmpty()}",
                    color = TodaySuccess, fontSize = 11.sp)
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                CardAction(card.detailLabel) { onDetail(card) }
                card.actions.take(4).forEach { capability ->
                    ResurfacingActionKind.fromWire(capability.kind)?.let { kind -> CardAction(capability.label, enabled = !busy) { onAction(card, kind) } }
                }
                CardAction("Useful", TodaySuccess, !busy, Icons.Outlined.ThumbUp) { viewModel.resolveResurfacing(card, ResurfacingFeedbackAction.Open) }
                CardAction("Seen", Secondary, !busy, Icons.Outlined.CheckCircle) { viewModel.resolveResurfacing(card, ResurfacingFeedbackAction.Acknowledge) }
                CardAction("Dismiss…", Danger, !busy) { onDismissWithReason(card) }
            }
        }
    }
}
