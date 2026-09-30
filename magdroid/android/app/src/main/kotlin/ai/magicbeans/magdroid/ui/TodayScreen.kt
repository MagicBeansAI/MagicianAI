@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.apps.AppSurfacingViewModel
import ai.magicbeans.magdroid.today.ChannelActionDescriptor
import ai.magicbeans.magdroid.today.ChannelDismissOption
import ai.magicbeans.magdroid.today.ChannelFollowUp
import ai.magicbeans.magdroid.today.ResurfacingActionKind
import ai.magicbeans.magdroid.today.ResurfacingCard
import ai.magicbeans.magdroid.today.ResurfacingFeedbackAction
import ai.magicbeans.magdroid.today.TODAY_DELIVERED_PAGE
import ai.magicbeans.magdroid.today.TodayActivityItem
import ai.magicbeans.magdroid.today.TodayBriefing
import ai.magicbeans.magdroid.today.TodayDeckCard
import ai.magicbeans.magdroid.today.TodayBroadsheetTab
import ai.magicbeans.magdroid.today.TodayItem
import ai.magicbeans.magdroid.today.TodayReadingRoomMode
import ai.magicbeans.magdroid.today.TodaySection
import ai.magicbeans.magdroid.today.TodaySnoozeOption
import ai.magicbeans.magdroid.today.TodayUiState
import ai.magicbeans.magdroid.today.TodayViewModel
import ai.magicbeans.magdroid.today.TodayWireItem
import ai.magicbeans.magdroid.today.readingRoomCountLabel
import ai.magicbeans.magdroid.today.titleCase
import ai.magicbeans.magdroid.today.todaySnoozeMinutes
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material.icons.outlined.AddTask
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.Lightbulb
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.Snooze
import androidx.compose.material.icons.outlined.TaskAlt
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

internal enum class TodayContentMode { Loading, Dashboard }

/**
 * Keep the Today workspace mounted after a failed first read, matching iOS.
 * A failure annotates the empty dashboard; it must not replace its navigation,
 * section affordances, or independently loadable projections.
 */
internal fun todayContentMode(state: TodayUiState): TodayContentMode =
    if (state.loading && state.payload == null) TodayContentMode.Loading else TodayContentMode.Dashboard

private val TodayGutter = Modifier.padding(horizontal = 16.dp)

/**
 * Today as the Morning Edition (web `/today`): masthead, realtime wire, lead
 * story, economics ledger, reading room (Morning Brief deck or broadsheet),
 * app widgets, special reports, deliverables and the chronicle. Each section,
 * and each broadsheet card, is its own lazy row so impressions only start for
 * cards actually on screen.
 */
@Composable
fun TodayScreen(
    viewModel: TodayViewModel = viewModel(),
    appSurfacing: AppSurfacingViewModel = viewModel(),
    onOpenTask: (String) -> Unit,
    onOpenMonitor: (String, String?) -> Unit,
    onOpenAttention: (String?) -> Unit,
    onOpenThread: (String) -> Unit,
    onOpenTasks: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    var snoozeItem by remember { mutableStateOf<TodayItem?>(null) }
    var dismissFollowUp by remember { mutableStateOf<ChannelFollowUp?>(null) }
    var dismissWorth by remember { mutableStateOf<ResurfacingCard?>(null) }
    var channelDetail by remember { mutableStateOf<ChannelFollowUp?>(null) }
    var writingFor by remember { mutableStateOf<ChannelFollowUp?>(null) }
    var composeFor by remember { mutableStateOf<Pair<ChannelFollowUp, ChannelActionDescriptor>?>(null) }
    var resurfacingDetail by remember { mutableStateOf<ResurfacingCard?>(null) }
    var actionInput by remember { mutableStateOf<Pair<ResurfacingCard, ResurfacingActionKind>?>(null) }
    var briefingDetail by remember { mutableStateOf<TodayBriefing?>(null) }
    var showAllBriefings by rememberSaveable { mutableStateOf(false) }
    var deliveredVisible by rememberSaveable { mutableIntStateOf(TODAY_DELIVERED_PAGE) }

    DisposableEffect(viewModel) { viewModel.start(); onDispose(viewModel::stop) }
    LaunchedEffect(state.navigationTaskId) {
        state.navigationTaskId?.let { onOpenTask(it); viewModel.consumeNavigation() }
    }

    fun openUrl(raw: String?) {
        raw?.takeIf(String::isNotBlank)?.let { runCatching {
            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(it)))
        } }
    }

    fun openBriefing(briefing: TodayBriefing) { briefingDetail = briefing; viewModel.loadBriefing(briefing) }

    /** App-relative or absolute route from a digest bullet, wire line or source. Returns false when unroutable. */
    fun openRoute(raw: String?): Boolean {
        val route = raw?.trim().orEmpty()
        when {
            route.isEmpty() -> return false
            route.startsWith("/attention") -> onOpenAttention(
                route.substringAfter('?', "").split('&').firstNotNullOfOrNull { pair ->
                    pair.substringAfter('=', "").takeIf { pair.substringBefore('=') in setOf("selected", "selected_item", "item_id") && it.isNotEmpty() }
                }?.let(Uri::decode),
            )
            route.startsWith("/tasks") -> when (val target = TaskDeepLinks.parse(route)) {
                is TaskDeepLinkTarget.Monitor -> onOpenMonitor(target.taskId, target.updateId)
                is TaskDeepLinkTarget.Task -> onOpenTask(target.taskId)
                null -> onOpenTasks()
            }
            route.startsWith("/t/") -> onOpenThread(Uri.decode(route.removePrefix("/t/").substringBefore('?')))
            route.startsWith("/briefing") -> state.briefings.firstOrNull { route.contains(it.surface.surfaceId) }
                ?.let(::openBriefing) ?: return false
            route.startsWith("/feed") -> viewModel.focusActivity(Uri.decode(route.substringAfter("selected_item=", "")))
            route.startsWith("http") -> openUrl(route)
            else -> return false
        }
        return true
    }

    fun openItem(item: TodayItem) {
        viewModel.markSeen(item)
        val route = item.sourceUrl.orEmpty()
        if (TodaySection.fromWire(item.section) == TodaySection.NeedsYou || route.startsWith("/attention")) {
            onOpenAttention(item.attentionItemId); return
        }
        item.monitorTarget?.let { onOpenMonitor(it.taskId, it.updateId); return }
        if (item.isMeetingAction) { viewModel.showItemDetail(item); return }
        item.taskId?.takeIf(String::isNotBlank)?.let { onOpenTask(it); return }
        when {
            route.startsWith("/tasks") -> when (val target = TaskDeepLinks.parse(route)) {
                is TaskDeepLinkTarget.Monitor -> onOpenMonitor(target.taskId, target.updateId)
                is TaskDeepLinkTarget.Task -> onOpenTask(target.taskId)
                null -> onOpenTask(item.sourceId)
            }
            route.startsWith("/t/") -> onOpenThread(item.threadId ?: Uri.decode(route.removePrefix("/t/").substringBefore('?')))
            route.startsWith("/briefing") -> state.briefings.firstOrNull {
                it.surface.surfaceId == item.sourceId || it.surface.taskId == item.taskId
            }?.let(::openBriefing) ?: viewModel.showItemDetail(item)
            route.startsWith("/feed") -> viewModel.focusActivity(item.sourceId)
            TodaySection.fromWire(item.section) == TodaySection.Delivered -> state.briefings.firstOrNull {
                it.surface.taskId == item.taskId || it.surface.surfaceId == item.sourceId
            }?.let(::openBriefing) ?: viewModel.showItemDetail(item)
            route.startsWith("http") -> openUrl(route)
            else -> viewModel.showItemDetail(item)
        }
    }

    fun openActivityItem(item: TodayActivityItem) {
        state.briefings.firstOrNull {
            item.itemType in setOf("data_delivery", "routine_result") &&
                (it.id == item.briefingSurfaceId || it.surface.taskId == item.taskId)
        }?.let(::openBriefing)
            ?: item.taskId?.let(onOpenTask)
            ?: item.threadId?.takeIf { item.itemType == "agent_message" }?.let(onOpenThread)
            ?: viewModel.showItemDetail(TodayItem(
                id = item.id, section = TodaySection.Changed.wire, title = item.title,
                summary = item.summary, reason = "Activity", sourceKind = item.itemType,
                sourceId = item.id, threadId = item.threadId, taskId = item.taskId,
                agentId = item.agentId, status = item.status, createdAt = item.createdAt,
                updatedAt = item.updatedAt,
            ))
    }

    fun openWireItem(item: TodayWireItem) {
        item.taskId?.let { onOpenTask(it); return }
        item.threadId?.let(onOpenThread)
    }

    fun openFollowUp(item: ChannelFollowUp) { channelDetail = item; viewModel.loadFollowUpMessage(item) }
    fun openWorth(card: ResurfacingCard) { resurfacingDetail = card; viewModel.loadResurfacingDetail(card) }
    fun runResurfacingAction(card: ResurfacingCard, kind: ResurfacingActionKind) {
        val capability = card.actions.firstOrNull { it.kind == kind.wire }
        if (capability?.requiresInput == true) actionInput = card to kind else viewModel.performResurfacingAction(card, kind)
    }

    /** Deck "Open": the source when the card links one, else its detail sheet. */
    fun openWorthSource(card: ResurfacingCard) {
        if (openRoute(card.sourceRoute?.takeIf { it.startsWith("/") })) return
        card.openUrl?.takeIf(String::isNotBlank)?.let { openUrl(it); return }
        openWorth(card)
    }

    val followUpHandlers = FollowUpHandlers(
        onOpen = ::openFollowUp,
        onDismissWithReason = { dismissFollowUp = it },
        onWriting = { item -> writingFor = item; viewModel.loadWritingPreferences(item) },
        onCompose = { item, action -> composeFor = item to action; if (action.needsCompose) viewModel.composeFollowUp(item, action) },
        onOpenUrl = ::openUrl,
    )

    Column(Modifier.fillMaxSize().background(Ground)) {
        if (state.refreshing) LinearProgressIndicator(Modifier.fillMaxWidth(), color = Coral)
        state.notice?.let { NoticeBar(it, viewModel::clearNotice) }
        when (todayContentMode(state)) {
            TodayContentMode.Loading -> TodayLoading()
            TodayContentMode.Dashboard -> LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                // 1. Masthead
                item(key = "masthead") {
                    TodayMasthead(modifier = TodayGutter.padding(top = 4.dp))
                }
                state.primaryFailure?.let { failure ->
                    item(key = "failure") { FailureBanner(failure = failure, onRetry = viewModel::refresh, modifier = TodayGutter) }
                }
                // 2. Realtime wire
                item(key = "wire") {
                    TodayRealtimeWire(state, onOpenItem = ::openWireItem, onOpenActivity = viewModel::openActivity, modifier = TodayGutter)
                }
                // 3. Lead story / slate clear
                item(key = "lead") {
                    TodayLeadStory(
                        needsYou = state.items(TodaySection.NeedsYou),
                        onTakeAction = ::openItem,
                        onOpenAttention = { onOpenAttention(null) },
                        modifier = TodayGutter,
                    )
                }
                // 4. Economics of operations
                item(key = "ledger") {
                    TodayLedger(
                        pulse = state.pulse, agents = state.agentCounts, pulseError = state.sectionErrors["pulse"],
                        onRetry = viewModel::refresh, onOpenTasks = onOpenTasks, onOpenTask = onOpenTask,
                        crew = state.crew, crewError = state.sectionErrors["crew"], onRetryCrew = viewModel::refreshCrew,
                        modifier = TodayGutter,
                    )
                }
                // 5. Reading room
                item(key = "reading-room") {
                    Box(TodayGutter) {
                        ReadingRoomHeader(
                            mode = state.readingRoomMode,
                            deckCountLabel = readingRoomCountLabel(state.messageFollowUpTotal + state.resurfacingTotal),
                            onMode = viewModel::setReadingRoomMode,
                        )
                    }
                }
                if (state.readingRoomMode == TodayReadingRoomMode.Deck) {
                    item(key = "deck") {
                        Box(TodayGutter) {
                            TodayTriageDeck(
                                state = state,
                                onAction = viewModel::triageDeckCard,
                                onPrimaryCommitted = { card -> card.worth?.let(::openWorthSource) },
                                onOpenCard = { card: TodayDeckCard ->
                                    card.followUp?.let(::openFollowUp) ?: card.worth?.let(::openWorth)
                                },
                                onOpenUrl = ::openUrl,
                                onOpenBroadsheet = { viewModel.setReadingRoomMode(TodayReadingRoomMode.Broadsheet) },
                                onReviewAgain = viewModel::reviewDeckAgain,
                                onLoadMore = viewModel::loadMoreDeck,
                                onImpression = { viewModel.recordImpression(it, it.minVisibleMs) },
                            )
                        }
                    }
                } else {
                    broadsheet(
                        state = state, viewModel = viewModel, handlers = followUpHandlers,
                        onOpenItem = ::openItem, onSnooze = { snoozeItem = it },
                        onWorthDetail = ::openWorth, onWorthAction = ::runResurfacingAction,
                        onWorthDismiss = { dismissWorth = it },
                    )
                }
                item(key = "hidden") {
                    TodayHiddenDrawer(
                        items = state.hiddenItems, lastHidden = state.lastHiddenItem, pending = state.pending,
                        error = state.sectionErrors["hidden"], onRestore = viewModel::restore,
                        onUndo = viewModel::undoLastHidden, modifier = TodayGutter,
                    )
                }
                // 6. App additions follow the day on both phones.
                item(key = "apps-primary") {
                    AppWidgetSlotRegion(page = "/", region = "primary", viewModel = appSurfacing, modifier = TodayGutter)
                }
                item(key = "apps-secondary") {
                    AppWidgetSlotRegion(page = "/", region = "secondary", viewModel = appSurfacing, modifier = TodayGutter)
                }
                // 7. § 3 Special reports
                item(key = "reports") {
                    TodaySpecialReports(
                        briefings = state.briefings, error = state.sectionErrors["briefings"], showAll = showAllBriefings,
                        onOpen = ::openBriefing,
                        onViewAll = { showAllBriefings = true; viewModel.loadAllBriefings() },
                        onRetry = viewModel::loadAllBriefings,
                    )
                }
                // 8. § 4 Completed deliverables
                val delivered = state.items(TodaySection.Delivered)
                if (delivered.isNotEmpty()) {
                    val total = maxOf(state.counts.delivered, delivered.size)
                    item(key = "delivered-banner") { TodayDeliverablesBanner(total, TodayGutter) }
                    items(delivered.take(deliveredVisible), key = { "delivered:${it.id}" }) { item ->
                        TodayDeliverableCard(
                            item = item, busy = state.isTodayItemMutationPending(item.id),
                            onInspect = { openItem(item) }, onAcknowledge = { viewModel.hide(item, "dismiss") },
                            modifier = TodayGutter,
                        )
                    }
                    state.sectionErrors[TodaySection.Delivered.wire]?.let { error ->
                        item(key = "delivered-error") { Box(TodayGutter) { TodayInlineError(error) { viewModel.loadMore(TodaySection.Delivered) } } }
                    }
                    if (deliveredVisible < delivered.size || total > delivered.size) item(key = "delivered-more") {
                        Box(TodayGutter) {
                            TodayLoadMore("lane:${TodaySection.Delivered.wire}" in state.loadingMore) {
                                if (deliveredVisible + TODAY_DELIVERED_PAGE > delivered.size && total > delivered.size) viewModel.loadMore(TodaySection.Delivered)
                                deliveredVisible += TODAY_DELIVERED_PAGE
                            }
                        }
                    }
                }
                // 9. § 5 Chronicle & digest
                if (state.digest.bullets.isNotEmpty()) item(key = "chronicle") {
                    TodayChronicle(
                        state = state,
                        onRefresh = { viewModel.loadDigest(state.digestOffset) },
                        onPage = viewModel::loadDigest,
                        onOpen = { bullet -> if (!openRoute(bullet.sourceUrl)) viewModel.focusActivity(bullet.sourceId) },
                        modifier = TodayGutter,
                    )
                }
                // 10. Footer
                item(key = "footer") { TodayFooter(TodayGutter.padding(bottom = 28.dp)) }
            }
        }
    }

    // A failed action is an error whether or not the day ever loaded.
    state.primaryError?.let { error ->
        AlertDialog(
            onDismissRequest = viewModel::clearError,
            confirmButton = { TextButton(onClick = viewModel::clearError) { Text("OK", color = Coral) } },
            title = { Text("Today action failed") }, text = { Text(error) },
        )
    }
    if (state.activityOpen) {
        TodayActivitySheet(
            state = state, onQuery = viewModel::setActivityQuery, onFilter = viewModel::setActivityFilter,
            onOpen = { item -> viewModel.closeActivity(); openActivityItem(item) },
            onRemove = viewModel::removeActivity, onClear = viewModel::clearActivity, onDismiss = viewModel::closeActivity,
        )
    }
    state.itemDetail?.let { ItemDetailSheet(it, onDismiss = { viewModel.showItemDetail(null) }, onOpenUrl = ::openUrl) }
    snoozeItem?.let { item -> SnoozeDialog(item, onDismiss = { snoozeItem = null }) { option ->
        snoozeItem = null; viewModel.hide(item, "snooze", todaySnoozeMinutes(option))
    } }
    dismissFollowUp?.let { item -> DismissReasonDialog(item.subject, ChannelDismissOption.all, onDismiss = { dismissFollowUp = null }) { reason ->
        dismissFollowUp = null; channelDetail = null; viewModel.resolveFollowUp(item, "dismiss", reason = reason)
    } }
    dismissWorth?.let { card -> DismissReasonDialog(card.sourceTitle.ifBlank { card.line }, ChannelDismissOption.resurfacing, onDismiss = { dismissWorth = null }) { reason ->
        dismissWorth = null; resurfacingDetail = null; viewModel.resolveResurfacing(card, ResurfacingFeedbackAction.Dismiss, reason)
    } }
    channelDetail?.let { item ->
        ChannelMessageSheet(item, state, viewModel, followUpHandlers, onDismiss = { channelDetail = null })
    }
    writingFor?.let { item -> WritingPreferencesSheet(item, state, viewModel, onDismiss = { writingFor = null }) }
    composeFor?.let { (item, descriptor) -> ComposeActionDialog(item, descriptor, state, viewModel) { composeFor = null } }
    resurfacingDetail?.let { card ->
        ResurfacingDetailSheet(card, state, viewModel, onDismiss = { resurfacingDetail = null }, onOpenUrl = ::openUrl,
            onAction = { kind -> runResurfacingAction(card, kind) }, onDismissWithReason = { dismissWorth = card })
    }
    actionInput?.let { (card, kind) -> ResurfacingInputDialog(card, kind, onDismiss = { actionInput = null }) { input ->
        actionInput = null; viewModel.performResurfacingAction(card, kind, input)
    } }
    briefingDetail?.let { briefing -> BriefingSheet(briefing, state, viewModel, onDismiss = { briefingDetail = null }, onOpenTask = onOpenTask) }
}

/**
 * 5b. Broadsheet: For You and Worth a look as tabs. Each tab is one server
 * page (5 cards) in a fixed-height, independently scrolling container with a
 * pager under it, so paging never moves the rest of the page.
 */
private fun LazyListScope.broadsheet(
    state: TodayUiState,
    viewModel: TodayViewModel,
    handlers: FollowUpHandlers,
    onOpenItem: (TodayItem) -> Unit,
    onSnooze: (TodayItem) -> Unit,
    onWorthDetail: (ResurfacingCard) -> Unit,
    onWorthAction: (ResurfacingCard, ResurfacingActionKind) -> Unit,
    onWorthDismiss: (ResurfacingCard) -> Unit,
) {
    item(key = "broadsheet") {
        val coreFollowUps = state.items(TodaySection.FollowUps)
        val forYou = state.followUpPage
        val worth = state.worthPage
        Column(TodayGutter, verticalArrangement = Arrangement.spacedBy(10.dp)) {
            BroadsheetTabs(
                selected = state.broadsheetTab,
                // Before a tab's first page answers, its badge uses the
                // totals the rest of the page already read.
                forYouCount = (if (forYou.loaded) forYou.total else state.messageFollowUpTotal) + state.counts.followups,
                worthCount = if (worth.loaded) worth.total else state.resurfacingTotal,
                onSelect = viewModel::setBroadsheetTab,
            )
            when (state.broadsheetTab) {
                TodayBroadsheetTab.ForYou -> {
                    BroadsheetColumnHeader("For You", forYouCountLabel(forYou.total + state.counts.followups), "Messages and threads needing a decision or reply.")
                    BroadsheetScrollContainer(pageKey = "for-you:${forYou.page}", loading = forYou.loading && !forYou.loaded) {
                        forYou.error?.let { TodayInlineError(it) { viewModel.loadFollowUpPage(forYou.page) } }
                        // Today's own follow-up items are few and unpaged; they lead page 1.
                        if (forYou.page == 1) coreFollowUps.forEach { item ->
                            BroadsheetTodayItemCard(item, state, viewModel, onOpenItem, onSnooze)
                        }
                        forYou.items.forEach { item -> BroadsheetFollowUpCard(item, state, viewModel, handlers) }
                        if (forYou.loaded && forYou.items.isEmpty() && (forYou.page > 1 || coreFollowUps.isEmpty())) {
                            BroadsheetEmpty("No dispatches waiting. Inbox is calm.")
                        }
                    }
                    BroadsheetPager(forYou, "For You", onPage = viewModel::loadFollowUpPage)
                }
                TodayBroadsheetTab.Worth -> {
                    BroadsheetColumnHeader("Worth a look", worthCountLabel(worth.total), "Resurfaced memory, project notes, and relevant knowledge.")
                    BroadsheetScrollContainer(pageKey = "worth:${worth.page}", loading = worth.loading && !worth.loaded) {
                        worth.error?.let { TodayInlineError(it) { viewModel.loadWorthPage(worth.page) } }
                        worth.items.forEach { card -> BroadsheetWorthCard(card, state, viewModel, onWorthDetail, onWorthAction, onWorthDismiss) }
                        if (worth.loaded && worth.items.isEmpty()) BroadsheetEmpty("Nothing worth a look right now. Your library is resting.")
                    }
                    BroadsheetPager(worth, "Worth a look", onPage = viewModel::loadWorthPage)
                }
            }
        }
    }
}

@Composable private fun NoticeBar(message: String, dismiss: () -> Unit) = Row(Modifier.fillMaxWidth().background(TodaySuccess.copy(alpha = .12f)).padding(horizontal = 12.dp, vertical = 7.dp), verticalAlignment = Alignment.CenterVertically) {
    Icon(Icons.Outlined.Check, null, tint = TodaySuccess, modifier = Modifier.size(16.dp)); Text(message, color = TodaySuccess, fontSize = 11.sp, modifier = Modifier.weight(1f).padding(horizontal = 7.dp)); IconButton(onClick = dismiss, modifier = Modifier.size(28.dp)) { Icon(Icons.Outlined.Close, "Dismiss", tint = TodaySuccess, modifier = Modifier.size(14.dp)) }
}

@Composable private fun TodayLoading() = Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
    CircularProgressIndicator(color = Coral); Spacer(Modifier.height(12.dp)); Text("Preparing your day…", color = Muted)
}

/** Core Today items keep their time picker: `/today/items/{id}/visibility` takes `snooze_minutes`. */
@Composable private fun SnoozeDialog(item: TodayItem, onDismiss: () -> Unit, onSelect: (TodaySnoozeOption) -> Unit) {
    AlertDialog(onDismissRequest = onDismiss, title = { Text("Snooze ${item.title}") }, text = { Column { TodaySnoozeOption.entries.forEach { option -> TextButton(onClick = { onSelect(option) }, modifier = Modifier.fillMaxWidth()) { Icon(Icons.Outlined.Snooze, null); Text(" ${option.label}", modifier = Modifier.fillMaxWidth()) } } } }, confirmButton = {}, dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } })
}

@Composable private fun DismissReasonDialog(subject: String?, options: List<ChannelDismissOption>, onDismiss: () -> Unit, resolve: (String?) -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Dismiss because…") },
        text = {
            Column {
                subject?.takeIf(String::isNotBlank)?.let { Text(it, color = Muted, fontSize = 12.sp) }
                options.forEach { option ->
                    TextButton(onClick = { resolve(option.code) }, modifier = Modifier.fillMaxWidth()) {
                        Text(option.label, modifier = Modifier.fillMaxWidth(), color = if (option.code == "spam") Danger else Ink)
                    }
                }
            }
        },
        confirmButton = {}, dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable private fun ItemDetailSheet(item: TodayItem, onDismiss: () -> Unit, onOpenUrl: (String?) -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = Ground) {
        Column(Modifier.fillMaxWidth().padding(18.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(item.title, style = newsSerif(22.sp, FontWeight.Bold)); item.detailMarkdown?.let { Text(it, color = Secondary, fontSize = 13.sp) }
            if (item.reason.isNotBlank()) Text("Why now · ${item.reason}", color = Muted, fontSize = 11.sp)
            item.sourceUrl?.let { Button(shape = MagicanButtonShape, onClick = { onOpenUrl(it) }, colors = ButtonDefaults.buttonColors(containerColor = Coral)) { Icon(Icons.AutoMirrored.Outlined.OpenInNew, null); Text(" Open source") } }
            Spacer(Modifier.height(18.dp))
        }
    }
}

/** Channel follow-up detail: the message plus every follow-up action (the deck's tap target). */
@Composable private fun ChannelMessageSheet(item: ChannelFollowUp, state: TodayUiState, viewModel: TodayViewModel, handlers: FollowUpHandlers, onDismiss: () -> Unit) {
    val message = state.channelMessages[item.id]
    val busy = state.isFollowUpMutationPending(item.id)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = Ground) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(18.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            NewsKicker(ai.magicbeans.magdroid.today.dispatchCategory(item), color = Coral)
            Text(item.subject ?: "Message", style = newsSerif(22.sp, FontWeight.Bold)); item.sender?.let { Text("From $it", color = Muted, fontSize = 11.sp) }
            item.actionSummary?.let { Text(it, color = Muted, fontSize = 11.sp) }
            item.reason?.let { Text("Why now · $it", color = TodayWarning, fontSize = 11.sp) }
            when {
                message != null -> {
                    if (message.hasNewer) Text("A newer message exists in this thread.", color = TodayWarning, fontSize = 11.sp)
                    Text(message.body ?: message.summary ?: "No message body is available.", color = Secondary, fontSize = 13.sp)
                    message.evidenceMessages.forEach { evidence -> Card(colors = CardDefaults.cardColors(containerColor = Panel)) { Text(evidence.body ?: evidence.summary.orEmpty(), color = Ink, fontSize = 12.sp, modifier = Modifier.padding(10.dp)) } }
                }
                "message:${item.id}" in state.pending -> CircularProgressIndicator(color = Coral)
                else -> state.sectionErrors["message:${item.id}"]?.let { TodayInlineError(it) { viewModel.loadFollowUpMessage(item) } }
            }
            Hairline()
            FollowUpActions(item, busy, viewModel, handlers.copy(onOpen = {}))
            Row { item.openUrl?.let { TextButton(onClick = { handlers.onOpenUrl(it) }) { Text("Open original") } }; TextButton(onClick = { viewModel.loadFollowUpMessage(item) }) { Text("Refresh") } }
            Spacer(Modifier.height(18.dp))
        }
    }
    // A resolved follow-up leaves the lists; its sheet closes with it.
    val stillPresent = state.messageFollowUps.any { it.id == item.id } || busy
    LaunchedEffect(stillPresent) { if (!stillPresent) onDismiss() }
}

@Composable private fun WritingPreferencesSheet(item: ChannelFollowUp, state: TodayUiState, viewModel: TodayViewModel, onDismiss: () -> Unit) {
    var statement by remember { mutableStateOf("") }; var scope by remember { mutableStateOf("sender") }; var promote by remember { mutableStateOf(false) }
    val rows = state.writingPreferences[item.id].orEmpty()
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = Ground) {
        Column(Modifier.fillMaxWidth().padding(18.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text("Writing style", style = newsSerif(22.sp, FontWeight.Bold)); Text("Learn exact preferences for ${item.sender ?: item.accountEmail ?: "this sender"}.", color = Muted, fontSize = 12.sp)
            state.sectionErrors["writing:${item.id}"]?.let { TodayInlineError(it) { viewModel.loadWritingPreferences(item) } }
            rows.forEach { preference -> Card(colors = CardDefaults.cardColors(containerColor = Panel), border = BorderStroke(1.dp, BorderSoft)) { Column(Modifier.padding(10.dp)) { Text(preference.statement, color = Ink, fontSize = 12.sp); Text("${titleCase(preference.scopeKind)} · ${titleCase(preference.status)} · ${preference.evidenceCount} evidence", color = Muted, fontSize = 9.sp); Row { if (preference.status != "promoted") TextButton(onClick = { viewModel.updateWritingPreference(item, preference.id, "promote") }) { Text("Promote") }; TextButton(onClick = { viewModel.updateWritingPreference(item, preference.id, "dismiss") }) { Text("Dismiss", color = Danger) } } } }
            }
            MagicianTextField(statement, { statement = it }, modifier = Modifier.fillMaxWidth(), label = { Text("New preference") }, minLines = 2)
            Row { FilterChip(scope == "sender", { scope = "sender" }, { Text("Sender") }); Spacer(Modifier.width(6.dp)); FilterChip(scope == "domain", { scope = "domain" }, { Text("Domain") }); Spacer(Modifier.width(6.dp)); FilterChip(promote, { promote = !promote }, { Text("Promote now") }) }
            Button(shape = MagicanButtonShape, onClick = { viewModel.learnWritingPreference(item, scope, statement, promote); statement = "" }, enabled = statement.isNotBlank() && "writing:${item.id}" !in state.pending, colors = ButtonDefaults.buttonColors(containerColor = Coral)) { Text("Learn preference") }
            Spacer(Modifier.height(18.dp))
        }
    }
}

@Composable private fun ComposeActionDialog(item: ChannelFollowUp, descriptor: ChannelActionDescriptor, state: TodayUiState, viewModel: TodayViewModel, dismiss: () -> Unit) {
    val draftKey = "${item.id}:${descriptor.id}"
    val draft = state.composedDrafts[draftKey]; var body by remember(draft?.text) { mutableStateOf(draft?.text.orEmpty()) }
    val composeError = state.sectionErrors["compose:$draftKey"]
    AlertDialog(onDismissRequest = dismiss, title = { Text(descriptor.label) }, text = { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (descriptor.needsCompose && draft == null) {
            // A failed compose must not spin forever: say so and offer a retry.
            if (composeError != null && "compose:$draftKey" !in state.pending) TodayInlineError(composeError) { viewModel.composeFollowUp(item, descriptor) }
            else { CircularProgressIndicator(color = Coral); Text("Composing…", color = Muted) }
        }
        MagicianTextField(value = body, onValueChange = { body = it }, modifier = Modifier.fillMaxWidth(), minLines = 4, label = { Text("Message") })
        if (descriptor.confirm) Text("This action affects an external channel and requires confirmation.", color = TodayWarning, fontSize = 11.sp)
    } }, confirmButton = { Button(shape = MagicanButtonShape, onClick = { viewModel.commitFollowUp(item, descriptor, body, draft?.composeId); dismiss() }, enabled = (!descriptor.needsCompose || draft != null) && "commit:${item.id}:${descriptor.id}" !in state.pending, colors = ButtonDefaults.buttonColors(containerColor = Coral)) { Icon(Icons.AutoMirrored.Outlined.Send, null); Text(" Send") } }, dismissButton = { TextButton(onClick = dismiss) { Text("Cancel") } })
}

/** Worth-a-look detail. Resurfacing has no snooze; dismiss offers only its own reasons. */
@Composable private fun ResurfacingDetailSheet(
    card: ResurfacingCard, state: TodayUiState, viewModel: TodayViewModel, onDismiss: () -> Unit,
    onOpenUrl: (String?) -> Unit, onAction: (ResurfacingActionKind) -> Unit, onDismissWithReason: () -> Unit,
) {
    val detail = state.resurfacingDetails[card.id]
    val busy = state.isResurfacingMutationPending(card.id)
    val detailKey = "resurfacing-detail:${card.id}"
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = Ground) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(18.dp), verticalArrangement = Arrangement.spacedBy(9.dp)) {
            NewsKicker(ai.magicbeans.magdroid.today.readingRoomCategory(card), color = TodayDiscovery)
            Text(detail?.title ?: card.sourceTitle.ifBlank { card.line }, style = newsSerif(22.sp, FontWeight.Bold))
            when {
                detail != null -> {
                    detail.summary?.let { Text(it, color = Secondary, fontSize = 13.sp) }; detail.brief?.keyFacts?.forEach { Text("• $it", color = Ink, fontSize = 12.sp) }
                    detail.brief?.changes?.forEach { change -> Text("${change.aspect}: ${change.before.orEmpty()} → ${change.after ?: change.effectiveText.orEmpty()}", color = TodayDiscovery, fontSize = 11.sp) }
                    if (detail.sourceUpdated || detail.hasNewer) Text("The source has newer content.", color = TodayWarning, fontSize = 11.sp)
                    detail.openUrl?.let { TextButton(onClick = { onOpenUrl(it) }) { Icon(Icons.AutoMirrored.Outlined.OpenInNew, null); Text(" Open source") } }
                }
                detailKey in state.pending -> CircularProgressIndicator(color = Coral)
                else -> state.sectionErrors[detailKey]?.let { TodayInlineError(it) { viewModel.loadResurfacingDetail(card) } }
                    ?: run { if (card.summary.isNotBlank()) Text(card.summary, color = Secondary, fontSize = 13.sp) }
            }
            FlowRow {
                card.actions.forEach { capability -> ResurfacingActionKind.fromWire(capability.kind)?.let { kind -> TextButton(onClick = { onAction(kind) }, enabled = !busy) { Text(capability.label) } } }
                TextButton(onClick = { viewModel.loadResurfacingDetail(card, true) }, enabled = !busy) { Text("Show original") }
            }
            Hairline()
            FlowRow {
                TextButton(onClick = { viewModel.resolveResurfacing(card, ResurfacingFeedbackAction.Open); onDismiss() }, enabled = !busy) { Text("Useful", color = TodaySuccess) }
                TextButton(onClick = { viewModel.resolveResurfacing(card, ResurfacingFeedbackAction.Acknowledge); onDismiss() }, enabled = !busy) { Text("Seen", color = Secondary) }
                TextButton(onClick = onDismissWithReason, enabled = !busy) { Text("Dismiss…", color = Danger) }
            }
            Spacer(Modifier.height(18.dp))
        }
    }
}

@Composable private fun ResurfacingInputDialog(card: ResurfacingCard, kind: ResurfacingActionKind, onDismiss: () -> Unit, run: (JsonObject) -> Unit) {
    var text by remember { mutableStateOf(card.line.ifBlank { card.sourceTitle }) }
    AlertDialog(onDismissRequest = onDismiss, title = { Text(titleCase(kind.wire)) }, text = { MagicianTextField(text, { text = it }, modifier = Modifier.fillMaxWidth(), minLines = 2, label = { Text(if (kind == ResurfacingActionKind.CreateReminder) "Reminder" else "Instruction") }) }, confirmButton = { Button(shape = MagicanButtonShape, onClick = { run(buildJsonObject { put("title", text); put("instruction", text) }) }, enabled = text.isNotBlank(), colors = ButtonDefaults.buttonColors(containerColor = Coral)) { Text("Continue") } }, dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } })
}

@Composable private fun BriefingSheet(briefing: TodayBriefing, state: TodayUiState, viewModel: TodayViewModel, onDismiss: () -> Unit, onOpenTask: (String) -> Unit) {
    val render = state.briefingRenders[briefing.id]
    val renderKey = "briefing:${briefing.id}"
    val muijDocument = render?.muijDocument
    val textContent = render?.textContent
    val jsonContent = render?.jsonContent
    val unavailableReason = render?.unavailableReason
    val sourceOutputSummary = briefing.sourceOutputSummary
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = Ground) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(18.dp), verticalArrangement = Arrangement.spacedBy(9.dp)) {
            NewsKicker("Special Edition", color = Coral)
            Text(briefing.surface.title, style = newsSerif(22.sp, FontWeight.Bold))
            briefing.surface.summary?.takeIf(String::isNotBlank)?.let { Text(it, color = Secondary, fontSize = 13.sp) }
            when {
                render == null && renderKey !in state.pending && state.sectionErrors[renderKey] != null ->
                    TodayInlineError(state.sectionErrors.getValue(renderKey)) { viewModel.loadBriefing(briefing) }
                render == null -> CircularProgressIndicator(color = Coral)
                muijDocument != null -> MuijDocumentRenderer(muijDocument)
                !textContent.isNullOrBlank() -> Text(textContent, color = Ink, fontSize = 14.sp)
                jsonContent != null -> MuijJsonContentRenderer(jsonContent)
                unavailableReason != null -> TodayInlineError(unavailableReason)
                !sourceOutputSummary.isNullOrBlank() -> Text(sourceOutputSummary, color = Ink, fontSize = 14.sp)
                else -> TodayInlineError("This briefing did not publish a displayable artifact.")
            }
            briefing.surface.taskId?.let { Button(shape = MagicanButtonShape, onClick = { onOpenTask(it) }, colors = ButtonDefaults.buttonColors(containerColor = Coral)) { Icon(Icons.Outlined.AddTask, null); Text(" Open task") } }
            Spacer(Modifier.height(18.dp))
        }
    }
}

internal fun todaySourceIcon(kind: String): ImageVector = when {
    "task" in kind -> Icons.Outlined.TaskAlt
    "learning" in kind || "memory" in kind -> Icons.Outlined.Lightbulb
    "delivery" in kind || "published" in kind -> Icons.Outlined.Description
    "message" in kind || "channel" in kind -> Icons.Outlined.Inbox
    "approval" in kind || "attention" in kind -> Icons.Outlined.Notifications
    else -> Icons.Outlined.AutoAwesome
}

internal fun todayStatusColor(status: String): Color = when (status.lowercase()) {
    "failed", "error", "blocked" -> Danger
    "done", "completed", "success", "delivered" -> TodaySuccess
    "running", "active", "in_progress" -> Coral
    "needs_action", "waiting", "paused" -> TodayWarning
    else -> Muted
}
