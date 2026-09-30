package ai.magicbeans.magdroid.today

import ai.magicbeans.magdroid.glance.MagicanGlanceSnapshot
import ai.magicbeans.magdroid.glance.MagicanGlanceStore
import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.toFailure
import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import java.util.UUID

data class TodayUiState(
    val payload: TodayResponse? = null,
    val hiddenItems: List<HiddenTodayItem> = emptyList(),
    val resurfacingCards: List<ResurfacingCard> = emptyList(),
    val resurfacingTotal: Int = 0,
    val messageFollowUps: List<ChannelFollowUp> = emptyList(),
    val messageFollowUpTotal: Int = 0,
    val activityItems: List<TodayActivityItem> = emptyList(),
    val briefings: List<TodayBriefing> = emptyList(),
    val briefingRenders: Map<String, TodayBriefingRender> = emptyMap(),
    val pulse: TodayPulse? = null,
    /** Realtime wire: feed, agent updates and live frames, newest first. */
    val wireItems: List<TodayWireItem> = emptyList(),
    /** Events recorded in the last 24h; 0 until the count query answers. */
    val eventCount24h: Long = 0,
    val agentCounts: TodayAgentCounts? = null,
    /** State of the Crew (last 24 h); null until the first read answers. */
    val crew: TodayCrew? = null,
    val readingRoomMode: TodayReadingRoomMode = TodayReadingRoomMode.Deck,
    val broadsheetTab: TodayBroadsheetTab = TodayBroadsheetTab.ForYou,
    /** Broadsheet pages, fetched per page and independent of the deck's lists. */
    val followUpPage: TodayBroadsheetPage<ChannelFollowUp> = TodayBroadsheetPage(),
    val worthPage: TodayBroadsheetPage<ResurfacingCard> = TodayBroadsheetPage(),
    /** Deck cards swiped this session, by card id, until "Review Again". */
    val deckTriaged: Map<String, TodayDeckLane> = emptyMap(),
    val loading: Boolean = false,
    val refreshing: Boolean = false,
    val loadingMore: Set<String> = emptySet(),
    /** A failed action on the day — hiding a card, sending a reply. */
    val primaryError: String? = null,
    /**
     * The day itself could not be read, classified.
     *
     * Its own field because it annotates the persistent empty/stale Today
     * workspace, where [primaryError] reports a failed action on that workspace.
     */
    val primaryFailure: Failure? = null,
    val sectionErrors: Map<String, String> = emptyMap(),
    val pending: Set<String> = emptySet(),
    val lastHiddenItem: HiddenTodayItem? = null,
    val itemDetail: TodayItem? = null,
    val resurfacingDetails: Map<String, ResurfacingDetail> = emptyMap(),
    val channelMessages: Map<String, ChannelMessageView> = emptyMap(),
    val writingPreferences: Map<String, List<ChannelWritingPreference>> = emptyMap(),
    val composedDrafts: Map<String, ChannelActionDraft> = emptyMap(),
    val actionResults: Map<String, ResurfacingActionResult> = emptyMap(),
    val feedbackReceipts: Map<String, AttentionFeedbackReceipt> = emptyMap(),
    val digestOffset: Int = 0,
    val activityFilter: TodayActivityFilter = TodayActivityFilter.All,
    val activityQuery: String = "",
    /** The Activity sheet opened from the wire drawer. */
    val activityOpen: Boolean = false,
    val navigationTaskId: String? = null,
    val notice: String? = null,
) {
    val counts: TodayCounts get() = payload?.counts ?: TodayCounts()
    val digest: TodayDigest get() = payload?.digest ?: TodayDigest()
    val headline: String get() = payload?.headline.orEmpty()

    fun items(section: TodaySection): List<TodayItem> = payload?.sections?.items(section).orEmpty()
    fun count(section: TodaySection): Int = when (section) {
        TodaySection.FollowUps -> counts.followups + messageFollowUpTotal
        TodaySection.WorthALook -> resurfacingTotal
        else -> counts.count(section, resurfacingTotal)
    }
    val hasMoreFollowUps: Boolean get() = messageFollowUps.size < messageFollowUpTotal
    val hasMoreResurfacing: Boolean get() = resurfacingCards.size < resurfacingTotal

    /** Every deck card, interleaved two dispatches to one reading-room card. */
    fun deckCards(): List<TodayDeckCard> = interleaveDeck(messageFollowUps, resurfacingCards)

    /** Cards still to triage in [tab]. */
    fun deckStack(tab: TodayDeckTab): List<TodayDeckCard> =
        deckCards().filter { it.id !in deckTriaged && tab.includes(it) }

    fun deckTriagedCount(tab: TodayDeckTab): Int = deckTriaged.count { (_, lane) ->
        when (tab) {
            TodayDeckTab.All -> true
            TodayDeckTab.ForYou -> lane == TodayDeckLane.Dispatch
            TodayDeckTab.Worth -> lane == TodayDeckLane.ReadingRoom
        }
    }
    fun filteredActivity(): List<TodayActivityItem> {
        val query = activityQuery.trim().lowercase()
        return activityItems.filter { activityFilter.matches(it) }.filter {
            query.isEmpty() || it.title.lowercase().contains(query) || it.summary.orEmpty().lowercase().contains(query)
        }
    }

    fun isTodayItemMutationPending(itemId: String): Boolean =
        "today:$itemId" in pending || "execute:$itemId" in pending

    fun isFollowUpMutationPending(itemId: String): Boolean =
        "followup:$itemId" in pending || pending.any { it.startsWith("commit:$itemId:") }

    fun isResurfacingMutationPending(itemId: String): Boolean =
        "resurfacing:$itemId" in pending || pending.any { it.startsWith("resurfacing-action:$itemId:") }
}

/** Android state machine matching iOS TodayViewModel's lifecycle and fail-soft behavior. */
class TodayViewModel private constructor(
    app: Application,
    private val source: TodayDataSource,
) : AndroidViewModel(app) {
    constructor(app: Application) : this(app, TodayRepository(app))

    private val preferences = app.getSharedPreferences("magdroid_today", 0)
    private val _state = MutableStateFlow(TodayUiState(
        readingRoomMode = TodayReadingRoomMode.fromWire(preferences.getString(READING_ROOM_KEY, null)),
        broadsheetTab = TodayBroadsheetTab.fromWire(preferences.getString(BROADSHEET_TAB_KEY, null)),
    ))
    val state: StateFlow<TodayUiState> = _state.asStateFlow()

    private var started = false
    private var generation = 0
    private var refreshJob: Job? = null
    private var pollingJob: Job? = null
    private var followUpPollingJob: Job? = null
    private var pulsePollingJob: Job? = null
    private var realtimeJob: Job? = null
    private var realtimeRefresh: Job? = null
    private val sectionCursors = mutableMapOf<TodaySection, String?>()
    private var resurfacingCursor: ResurfacingCursor? = null
    /** Broadsheet For You: the cursor that opens each known page (page 1 has none). */
    private val followUpPageCursors = mutableMapOf<Int, String?>(1 to null)
    /** Newest broadsheet request per tab; an older answer that lands late is dropped. */
    private var followUpPageGeneration = 0
    private var worthPageGeneration = 0
    private var followUpCursor: String? = null
    private var followUpProjectionReference: CanonicalAttentionProjectionReference? = null
    private var worthProjectionReference: CanonicalAttentionProjectionReference? = null
    private val impressionEventIds = mutableMapOf<String, String>()
    private val impressionReceipts = mutableMapOf<String, AttentionImpressionReceipt>()
    private val inFlightImpressions = mutableSetOf<String>()
    private val terminalImpressions = mutableSetOf<String>()

    init {
        // The lane picker is gone; its persisted selection is dead state.
        if (preferences.contains("section")) preferences.edit().remove("section").apply()
    }

    fun start() {
        if (started) return
        started = true
        refresh()
        refreshCrew()
        ensureBroadsheetPage()
        pollingJob = viewModelScope.launch { while (started) { delay(30_000); refresh(); reloadBroadsheetPages() } }
        followUpPollingJob = viewModelScope.launch { while (started) { delay(20_000); refreshFollowUps() } }
        pulsePollingJob = viewModelScope.launch { while (started) { delay(60_000); refreshPulse() } }
        realtimeJob = viewModelScope.launch {
            source.events().catch { /* repository reconnects; stale content remains usable */ }.collect { frame ->
                normalizeRealtimeWireEvent(frame.raw)?.let { item ->
                    val current = _state.value
                    val fresh = current.wireItems.none { it.id == item.id }
                    _state.value = current.copy(
                        wireItems = mergeWireItems(current.wireItems, listOf(item)),
                        eventCount24h = current.eventCount24h + if (fresh) 1 else 0,
                    )
                }
                if (!frame.refresh) return@collect
                realtimeRefresh?.cancel()
                realtimeRefresh = viewModelScope.launch { delay(750); refresh() }
            }
        }
    }

    fun stop() {
        started = false
        pollingJob?.cancel(); followUpPollingJob?.cancel(); pulsePollingJob?.cancel()
        realtimeJob?.cancel(); realtimeRefresh?.cancel()
        pollingJob = null; followUpPollingJob = null; pulsePollingJob = null; realtimeJob = null
    }

    fun refresh() {
        if (refreshJob?.isActive == true) return
        generation++
        val requestGeneration = generation
        val hadPayload = _state.value.payload != null
        _state.value = _state.value.copy(
            loading = !hadPayload,
            refreshing = hadPayload,
            primaryError = null,
            primaryFailure = null,
        )
        refreshJob = viewModelScope.launch {
            val results = coroutineScope {
                val today = async { captured { source.today() } }
                val hidden = async { captured { source.hidden() } }
                val resurfacing = async { captured { source.resurfacing() } }
                val followUps = async { captured { source.followUps() } }
                val feed = async { captured { source.feed() } }
                val briefings = async { captured { source.briefings() } }
                val pulse = async { captured { source.pulse(_state.value.pulse) } }
                val updates = async { captured { source.agentUpdates() } }
                val agents = async { captured { source.agents() } }
                val count = async { captured { source.eventCount24h() } }
                InitialResults(
                    today.await(), hidden.await(), resurfacing.await(), followUps.await(), feed.await(),
                    briefings.await(), pulse.await(), updates.await(), agents.await(), count.await(),
                )
            }
            if (requestGeneration != generation) return@launch
            applyInitial(results)
        }
    }

    private fun applyInitial(results: InitialResults) {
        var next = _state.value
        val errors = next.sectionErrors.toMutableMap()
        var resurfacingToBind: ResurfacingPage? = null
        var followUpsToBind: ChannelFollowUpPage? = null
        results.today.onSuccess { value ->
            next = next.copy(
                payload = suppressPendingToday(value, next.pending),
                primaryError = null,
                primaryFailure = null,
                digestOffset = 0,
            )
            sectionCursors.clear()
            if (MagicanGlanceStore.save(getApplication(), MagicanGlanceSnapshot.reduce(value))) {
                getApplication<Application>().sendBroadcast(
                    android.content.Intent("ai.magicbeans.magdroid.action.REFRESH_GLANCE")
                        .setPackage(getApplication<Application>().packageName),
                )
            }
        }.onFailure { error ->
            next = next.copy(primaryFailure = error.toFailure(getApplication(), "your day"))
        }
        results.hidden.fold({ next = next.copy(hiddenItems = it); errors.remove("hidden") }, { errors["hidden"] = message(it) })
        results.resurfacing.fold({ page ->
            val suppressed = next.pending.filter { it.startsWith("resurfacing:") }.map { it.removePrefix("resurfacing:") }.toSet()
            val cards = page.cards.filterNot { it.id in suppressed }
            next = next.copy(resurfacingCards = cards, resurfacingTotal = (page.total - (page.cards.size - cards.size)).coerceAtLeast(cards.size))
            resurfacingCursor = page.nextCursor; worthProjectionReference = page.canonicalProjectionReference
            errors.remove(TodaySection.WorthALook.wire)
            resurfacingToBind = page
        }, { errors[TodaySection.WorthALook.wire] = message(it) })
        results.followUps.fold({ page ->
            val suppressed = next.pending.filter { it.startsWith("followup:") }.map { it.removePrefix("followup:") }.toSet()
            val rows = page.items.filterNot { it.id in suppressed }
            next = next.copy(messageFollowUps = rows, messageFollowUpTotal = (page.total - (page.items.size - rows.size)).coerceAtLeast(rows.size))
            followUpCursor = page.nextCursor; followUpProjectionReference = page.canonicalProjectionReference
            errors.remove("message_followups")
            followUpsToBind = page
        }, { errors["message_followups"] = message(it) })
        results.feed.fold({ rows ->
            next = next.copy(activityItems = rows.filter(::isDurableTodayActivity))
            errors.remove("activity")
        }, { errors["activity"] = message(it) })
        // The wire is fail-soft and never shows placeholders: a failed read
        // simply contributes nothing.
        val wireIncoming = results.feed.getOrNull().orEmpty().take(TODAY_WIRE_FEED_LIMIT).map { normalizeFeedWireItem(it) } +
            results.updates.getOrNull().orEmpty().map { normalizeAgentUpdate(it) }
        next = next.copy(wireItems = mergeWireItems(next.wireItems, wireIncoming))
        results.agents.onSuccess { next = next.copy(agentCounts = it) }
        results.count.onSuccess { next = next.copy(eventCount24h = it) }
        results.briefings.fold({ next = next.copy(briefings = it); errors.remove("briefings") }, { errors["briefings"] = message(it) })
        results.pulse.fold({ next = next.copy(pulse = it); errors.remove("pulse") }, { errors["pulse"] = message(it) })
        next = next.copy(loading = false, refreshing = false, sectionErrors = errors)
        _state.value = next
        resurfacingToBind?.let(::bindWorthALook)
        followUpsToBind?.let(::bindFollowUps)
    }

    fun setReadingRoomMode(mode: TodayReadingRoomMode) {
        _state.value = _state.value.copy(readingRoomMode = mode)
        preferences.edit().putString(READING_ROOM_KEY, mode.wire).apply()
        ensureBroadsheetPage()
    }

    fun setBroadsheetTab(tab: TodayBroadsheetTab) {
        _state.value = _state.value.copy(broadsheetTab = tab)
        preferences.edit().putString(BROADSHEET_TAB_KEY, tab.wire).apply()
        ensureBroadsheetPage()
    }

    /** Loads the visible broadsheet tab's first page the first time it is shown. */
    private fun ensureBroadsheetPage() {
        val state = _state.value
        if (state.readingRoomMode != TodayReadingRoomMode.Broadsheet) return
        when (state.broadsheetTab) {
            TodayBroadsheetTab.ForYou -> if (!state.followUpPage.loaded && !state.followUpPage.loading) loadFollowUpPage(1)
            TodayBroadsheetTab.Worth -> if (!state.worthPage.loaded && !state.worthPage.loading) loadWorthPage(1)
        }
    }

    /** Re-reads the current page of every tab already shown (polling, after a mutation). */
    private fun reloadBroadsheetPages() {
        val state = _state.value
        if (state.followUpPage.loaded) loadFollowUpPage(state.followUpPage.page)
        if (state.worthPage.loaded) loadWorthPage(state.worthPage.page)
    }

    /**
     * Channel follow-ups page by opaque keyset cursor, so page N is reached by
     * walking forward from the nearest page whose cursor is known (web
     * `ensureFollowUpCursor`). A page past the end settles on the last one.
     */
    fun loadFollowUpPage(page: Int) {
        val target = page.coerceAtLeast(1)
        val generation = ++followUpPageGeneration
        _state.value = _state.value.copy(followUpPage = _state.value.followUpPage.copy(loading = true))
        viewModelScope.launch {
            captured {
                var known = followUpPageCursors.keys.filter { it <= target }.maxOrNull() ?: 1
                while (known < target) {
                    val walk = source.followUps(limit = TODAY_BROADSHEET_PAGE_SIZE, cursor = followUpPageCursors[known])
                    val next = walk.nextCursor ?: break
                    known += 1
                    followUpPageCursors[known] = next
                }
                val result = source.followUps(limit = TODAY_BROADSHEET_PAGE_SIZE, cursor = followUpPageCursors[known])
                result.nextCursor?.let { followUpPageCursors[known + 1] = it }
                known to result
            }.fold({ (landed, result) ->
                if (generation != followUpPageGeneration) return@fold
                val current = _state.value
                _state.value = current.copy(followUpPage = TodayBroadsheetPage(
                    page = landed,
                    items = result.items.filterNot { "followup:${it.id}" in current.pending },
                    total = result.total, loaded = true,
                ))
            }, { error ->
                if (generation != followUpPageGeneration) return@fold
                val current = _state.value
                _state.value = current.copy(followUpPage = current.followUpPage.copy(loading = false, loaded = true, error = message(error)))
            })
        }
    }

    fun loadWorthPage(page: Int) {
        val target = page.coerceAtLeast(1)
        val generation = ++worthPageGeneration
        _state.value = _state.value.copy(worthPage = _state.value.worthPage.copy(loading = true))
        viewModelScope.launch {
            captured {
                source.resurfacing(limit = TODAY_BROADSHEET_PAGE_SIZE, offset = (target - 1) * TODAY_BROADSHEET_PAGE_SIZE)
            }.fold({ result ->
                if (generation != worthPageGeneration) return@fold
                val current = _state.value
                _state.value = current.copy(worthPage = TodayBroadsheetPage(
                    page = target,
                    items = result.cards.filterNot { "resurfacing:${it.id}" in current.pending },
                    total = result.total, loaded = true,
                ))
            }, { error ->
                if (generation != worthPageGeneration) return@fold
                val current = _state.value
                _state.value = current.copy(worthPage = current.worthPage.copy(loading = false, loaded = true, error = message(error)))
            })
        }
    }

    /**
     * Swipe or button on the top deck card. Returns false when the card is
     * locked by an in-flight mutation, so the deck springs it back instead of
     * hiding a card nothing was done to. Rollback on failure un-triages it.
     */
    fun triageDeckCard(card: TodayDeckCard, action: TodayDeckAction): Boolean {
        val state = _state.value
        card.followUp?.let { item ->
            if (state.isFollowUpMutationPending(item.id)) return false
            _state.value = state.copy(deckTriaged = state.deckTriaged + (card.id to card.lane))
            resolveFollowUp(item, when (action) {
                TodayDeckAction.Useful -> "useful"
                TodayDeckAction.Acknowledge -> "acknowledge"
                TodayDeckAction.Dismiss -> "dismiss"
                TodayDeckAction.Primary -> "approve"
            })
            return true
        }
        card.worth?.let { worth ->
            if (state.isResurfacingMutationPending(worth.id)) return false
            _state.value = state.copy(deckTriaged = state.deckTriaged + (card.id to card.lane))
            resolveResurfacing(worth, when (action) {
                TodayDeckAction.Acknowledge -> ResurfacingFeedbackAction.Acknowledge
                TodayDeckAction.Dismiss -> ResurfacingFeedbackAction.Dismiss
                TodayDeckAction.Useful, TodayDeckAction.Primary -> ResurfacingFeedbackAction.Open
            })
            return true
        }
        return false
    }

    /** Clears only the local triaged set; resolved cards stay resolved. */
    fun reviewDeckAgain() { _state.value = _state.value.copy(deckTriaged = emptyMap()) }

    /** Next follow-up and worth pages, so the deck keeps flowing. */
    fun loadMoreDeck() {
        val state = _state.value
        if (state.hasMoreFollowUps) loadMoreFollowUps()
        if (state.hasMoreResurfacing) loadMoreResurfacing()
    }

    fun clearError() { _state.value = _state.value.copy(primaryError = null, notice = null) }
    fun clearNotice() { _state.value = _state.value.copy(notice = null) }
    fun setActivityFilter(filter: TodayActivityFilter) { _state.value = _state.value.copy(activityFilter = filter) }
    fun setActivityQuery(query: String) { _state.value = _state.value.copy(activityQuery = query) }
    fun openActivity() { _state.value = _state.value.copy(activityOpen = true) }
    fun closeActivity() { _state.value = _state.value.copy(activityOpen = false) }
    fun focusActivity(query: String) { _state.value = _state.value.copy(activityOpen = true, activityQuery = query) }
    fun showItemDetail(item: TodayItem?) { _state.value = _state.value.copy(itemDetail = item) }
    fun consumeNavigation() { _state.value = _state.value.copy(navigationTaskId = null) }

    fun loadMore(section: TodaySection) {
        val key = "lane:${section.wire}"
        if (key in _state.value.loadingMore) return
        if (section == TodaySection.WorthALook) return loadMoreResurfacing()
        _state.value = _state.value.copy(loadingMore = _state.value.loadingMore + key)
        viewModelScope.launch {
            val existingIds = _state.value.items(section).map(TodayItem::id).toSet()
            val startingCursor = sectionCursors[section]
            captured {
                val first = source.today(section, startingCursor)
                if (startingCursor == null && first.sections.items(section).none { it.id !in existingIds }) {
                    first.sectionPage?.nextCursor?.let { source.today(section, it) } ?: first
                } else first
            }.fold({ page ->
                val current = _state.value
                val existing = current.items(section)
                val incoming = page.sections.items(section).filterNot { "today:${it.id}" in current.pending }
                val merged = (existing + incoming).distinctBy(TodayItem::id)
                sectionCursors[section] = page.sectionPage?.nextCursor
                val payload = current.payload?.copy(
                    sections = current.payload.sections.replacing(section, merged), counts = page.counts,
                )
                _state.value = current.copy(payload = payload, sectionErrors = current.sectionErrors - section.wire)
            }, { setSectionError(section.wire, it) })
            _state.value = _state.value.copy(loadingMore = _state.value.loadingMore - key)
        }
    }

    fun loadMoreResurfacing() {
        val key = "lane:${TodaySection.WorthALook.wire}"
        if (key in _state.value.loadingMore) return
        _state.value = _state.value.copy(loadingMore = _state.value.loadingMore + key)
        viewModelScope.launch {
            captured { source.resurfacing(cursor = resurfacingCursor) }.fold({ page ->
                val current = _state.value
                val incoming = page.cards.filterNot { "resurfacing:${it.id}" in current.pending }
                val merged = (current.resurfacingCards + incoming).distinctBy(ResurfacingCard::id)
                _state.value = current.copy(
                    resurfacingCards = merged,
                    resurfacingTotal = (page.total - (page.cards.size - incoming.size)).coerceAtLeast(merged.size),
                    sectionErrors = current.sectionErrors - TodaySection.WorthALook.wire,
                )
                resurfacingCursor = page.nextCursor
            }, { setSectionError(TodaySection.WorthALook.wire, it) })
            _state.value = _state.value.copy(loadingMore = _state.value.loadingMore - key)
        }
    }

    fun loadMoreFollowUps() {
        val key = "lane:message_followups"
        if (key in _state.value.loadingMore) return
        _state.value = _state.value.copy(loadingMore = _state.value.loadingMore + key)
        viewModelScope.launch {
            captured { source.followUps(cursor = followUpCursor) }.fold({ page ->
                val current = _state.value
                val incoming = page.items.filterNot { "followup:${it.id}" in current.pending }
                val merged = (current.messageFollowUps + incoming).distinctBy(ChannelFollowUp::id)
                _state.value = current.copy(
                    messageFollowUps = merged,
                    messageFollowUpTotal = (page.total - (page.items.size - incoming.size)).coerceAtLeast(merged.size),
                    sectionErrors = current.sectionErrors - "message_followups",
                )
                followUpCursor = page.nextCursor
            }, { setSectionError("message_followups", it) })
            _state.value = _state.value.copy(loadingMore = _state.value.loadingMore - key)
        }
    }

    fun loadDigest(offset: Int) {
        val key = "digest"
        if (key in _state.value.loadingMore) return
        _state.value = _state.value.copy(loadingMore = _state.value.loadingMore + key)
        viewModelScope.launch {
            captured { source.today(digestOffset = offset.coerceAtLeast(0)) }.fold({ page ->
                val current = _state.value
                _state.value = current.copy(
                    payload = current.payload?.copy(generatedAt = page.generatedAt, digest = page.digest),
                    digestOffset = offset.coerceAtLeast(0), sectionErrors = current.sectionErrors - key,
                )
            }, { setSectionError(key, it) })
            _state.value = _state.value.copy(loadingMore = _state.value.loadingMore - key)
        }
    }

    fun hide(item: TodayItem, action: String, snoozeMinutes: Int? = null) {
        require(action in setOf("dismiss", "snooze"))
        val key = "today:${item.id}"
        if (_state.value.isTodayItemMutationPending(item.id)) return
        val section = TodaySection.fromWire(item.section)
        val anchor = CardAnchor.capture(_state.value.items(section).map(TodayItem::id), item.id)
        val now = System.currentTimeMillis()
        val hidden = HiddenTodayItem(item.id, if (action == "snooze") "snoozed" else "dismissed", HiddenTodayRecord(
            seenAt = item.seenAt, dismissedAt = now.takeIf { action == "dismiss" },
            snoozedUntil = snoozeMinutes?.let { now + it * 60_000L },
            snapshot = TodayVisibilitySnapshot(item.title, item.summary, item.reason, item.section, item.sourceKind,
                item.sourceId, item.sourceUrl, item.spaceIds, item.updatedAt),
        ))
        val original = _state.value
        _state.value = removeTodayItem(original, item).copy(
            hiddenItems = listOf(hidden) + original.hiddenItems.filterNot { it.id == item.id },
            lastHiddenItem = hidden, pending = original.pending + key,
        )
        viewModelScope.launch {
            captured { source.setVisibility(item, item.id, action, snoozeMinutes) }.fold({
                _state.value = _state.value.copy(pending = _state.value.pending - key)
                delayedRefresh()
            }, { error ->
                val state = _state.value
                _state.value = insertTodayItem(state.copy(
                    hiddenItems = state.hiddenItems.filterNot { it.id == item.id },
                    lastHiddenItem = state.lastHiddenItem?.takeUnless { it.id == item.id }, pending = state.pending - key,
                    primaryError = message(error),
                ), item, section, anchor.insertionIndex(state.items(section).map(TodayItem::id)))
            })
        }
    }

    fun markSeen(item: TodayItem) { viewModelScope.launch { captured { source.setVisibility(null, item.id, "mark_seen") } } }

    fun restore(item: HiddenTodayItem) {
        val key = "restore:${item.id}"
        if (key in _state.value.pending) return
        val previous = _state.value
        _state.value = previous.copy(hiddenItems = previous.hiddenItems.filterNot { it.id == item.id }, pending = previous.pending + key)
        viewModelScope.launch {
            captured { source.setVisibility(null, item.id, "restore") }.fold({
                _state.value = _state.value.copy(pending = _state.value.pending - key, lastHiddenItem = null)
                delayedRefresh()
            }, { error ->
                val state = _state.value
                _state.value = state.copy(hiddenItems = (state.hiddenItems + item).distinctBy(HiddenTodayItem::id), pending = state.pending - key, primaryError = message(error))
            })
        }
    }

    fun undoLastHidden() { _state.value.lastHiddenItem?.let(::restore) }

    fun execute(action: TodayAction, item: TodayItem) {
        val endpoint = action.executionEndpoint ?: return setPrimaryError("This Today action is no longer available.")
        val key = "execute:${item.id}"
        if (_state.value.isTodayItemMutationPending(item.id)) return
        _state.value = _state.value.copy(pending = _state.value.pending + key)
        viewModelScope.launch {
            captured { source.executeTodayAction(endpoint) }.fold({ taskId ->
                _state.value = removeTodayItem(_state.value, item).copy(pending = _state.value.pending - key, navigationTaskId = taskId)
                delayedRefresh()
            }, { error -> _state.value = _state.value.copy(pending = _state.value.pending - key, primaryError = message(error)) })
        }
    }

    fun resolveFollowUp(item: ChannelFollowUp, action: String, hint: String? = null, reason: String? = null) {
        val key = "followup:${item.id}"
        if (_state.value.isFollowUpMutationPending(item.id)) return
        val previous = _state.value
        val anchor = CardAnchor.capture(previous.messageFollowUps.map(ChannelFollowUp::id), item.id)
        _state.value = previous.copy(
            messageFollowUps = previous.messageFollowUps.filterNot { it.id == item.id },
            messageFollowUpTotal = (previous.messageFollowUpTotal - 1).coerceAtLeast(0), pending = previous.pending + key,
            followUpPage = previous.followUpPage.without(item.id, ChannelFollowUp::id),
        )
        viewModelScope.launch {
            captured { source.resolveFollowUp(item, action, hint, reason) }.fold({ receipt ->
                val state = _state.value
                _state.value = state.copy(pending = state.pending - key,
                    feedbackReceipts = receipt?.let { state.feedbackReceipts + (item.id to it) } ?: state.feedbackReceipts)
                // The server page shifted by one; re-read it to backfill.
                if (_state.value.followUpPage.loaded) loadFollowUpPage(_state.value.followUpPage.page)
            }, { error ->
                if (_state.value.followUpPage.loaded) loadFollowUpPage(_state.value.followUpPage.page)
                val state = _state.value
                val rows = state.messageFollowUps.toMutableList().apply {
                    if (none { it.id == item.id }) add(anchor.insertionIndex(map(ChannelFollowUp::id)), item)
                }
                _state.value = state.copy(messageFollowUps = rows, messageFollowUpTotal = state.messageFollowUpTotal + 1,
                    pending = state.pending - key, primaryError = message(error),
                    deckTriaged = state.deckTriaged - deckCardId(item))
            })
        }
    }

    fun loadFollowUpMessage(item: ChannelFollowUp) = loadMapValue(
        key = "message:${item.id}", action = { source.followUpMessage(item.id) },
        apply = { state, value -> state.copy(channelMessages = state.channelMessages + (item.id to value)) },
    )

    fun composeFollowUp(item: ChannelFollowUp, descriptor: ChannelActionDescriptor, hint: String? = null) = loadMapValue(
        key = "compose:${item.id}:${descriptor.id}", action = { source.composeFollowUp(item, descriptor, hint) },
        apply = { state, value -> state.copy(composedDrafts = state.composedDrafts + ("${item.id}:${descriptor.id}" to value)) },
    )

    fun commitFollowUp(item: ChannelFollowUp, descriptor: ChannelActionDescriptor, body: String?, composeId: String?) {
        val key = "commit:${item.id}:${descriptor.id}"
        if (_state.value.isFollowUpMutationPending(item.id)) return
        _state.value = _state.value.copy(pending = _state.value.pending + key)
        viewModelScope.launch {
            captured { source.commitFollowUp(item, descriptor, body, composeId) }.fold({ receipt ->
                val state = _state.value
                _state.value = state.copy(
                    messageFollowUps = state.messageFollowUps.filterNot { it.id == item.id },
                    messageFollowUpTotal = (state.messageFollowUpTotal - 1).coerceAtLeast(0), pending = state.pending - key,
                    composedDrafts = state.composedDrafts - "${item.id}:${descriptor.id}",
                    feedbackReceipts = receipt?.let { state.feedbackReceipts + (item.id to it) } ?: state.feedbackReceipts,
                    notice = "${descriptor.label} completed.",
                    followUpPage = state.followUpPage.without(item.id, ChannelFollowUp::id),
                )
                if (_state.value.followUpPage.loaded) loadFollowUpPage(_state.value.followUpPage.page)
            }, { error -> _state.value = _state.value.copy(pending = _state.value.pending - key, primaryError = message(error)) })
        }
    }

    fun loadWritingPreferences(item: ChannelFollowUp) = loadMapValue(
        key = "writing:${item.id}", action = { source.writingPreferences(item.id) },
        apply = { state, value -> state.copy(writingPreferences = state.writingPreferences + (item.id to value)) },
    )

    fun learnWritingPreference(item: ChannelFollowUp, scope: String, statement: String, promote: Boolean) {
        val key = "writing:${item.id}"
        if (key in _state.value.pending || statement.isBlank()) return
        _state.value = _state.value.copy(pending = _state.value.pending + key)
        viewModelScope.launch {
            captured { source.learnWritingPreference(item.id, scope, statement, promote); source.writingPreferences(item.id) }.fold({ rows ->
                val state = _state.value; _state.value = state.copy(pending = state.pending - key, writingPreferences = state.writingPreferences + (item.id to rows))
            }, { error -> _state.value = _state.value.copy(pending = _state.value.pending - key, primaryError = message(error)) })
        }
    }

    fun updateWritingPreference(item: ChannelFollowUp, id: String, action: String) {
        viewModelScope.launch { captured { source.updateWritingPreference(id, action); source.writingPreferences(item.id) }.fold({ rows ->
            _state.value = _state.value.copy(writingPreferences = _state.value.writingPreferences + (item.id to rows))
        }, { setPrimaryError(message(it)) }) }
    }

    fun resolveResurfacing(card: ResurfacingCard, action: ResurfacingFeedbackAction, reason: String? = null) {
        val key = "resurfacing:${card.id}"
        if (_state.value.isResurfacingMutationPending(card.id)) return
        val previous = _state.value
        val anchor = CardAnchor.capture(previous.resurfacingCards.map(ResurfacingCard::id), card.id)
        _state.value = previous.copy(resurfacingCards = previous.resurfacingCards.filterNot { it.id == card.id },
            resurfacingTotal = (previous.resurfacingTotal - 1).coerceAtLeast(0), pending = previous.pending + key,
            worthPage = previous.worthPage.without(card.id, ResurfacingCard::id))
        viewModelScope.launch {
            captured { source.resolveResurfacing(card, action, reason) }.fold({ receipt ->
                val state = _state.value; _state.value = state.copy(pending = state.pending - key,
                    feedbackReceipts = receipt?.let { state.feedbackReceipts + (card.id to it) } ?: state.feedbackReceipts)
                if (_state.value.worthPage.loaded) loadWorthPage(_state.value.worthPage.page)
            }, { error ->
                if (_state.value.worthPage.loaded) loadWorthPage(_state.value.worthPage.page)
                val state = _state.value
                val rows = state.resurfacingCards.toMutableList().apply {
                    if (none { it.id == card.id }) add(anchor.insertionIndex(map(ResurfacingCard::id)), card)
                }
                _state.value = state.copy(resurfacingCards = rows, resurfacingTotal = state.resurfacingTotal + 1,
                    pending = state.pending - key, primaryError = message(error),
                    deckTriaged = state.deckTriaged - deckCardId(card))
            })
        }
    }

    fun loadResurfacingDetail(card: ResurfacingCard, original: Boolean = false) = loadMapValue(
        key = "resurfacing-detail:${card.id}", action = { source.resurfacingDetail(card.id, original) },
        apply = { state, value -> state.copy(resurfacingDetails = state.resurfacingDetails + (card.id to value)) },
    ).also {
        if (!original) card.recommendedAction?.let { recommendation ->
            ResurfacingActionKind.fromWire(recommendation.kind)?.let { kind ->
                viewModelScope.launch { captured { source.recordRecommendation(card, kind, "presented") } }
            }
        }
    }

    fun performResurfacingAction(card: ResurfacingCard, kind: ResurfacingActionKind, input: JsonObject = JsonObject(emptyMap())) {
        val key = "resurfacing-action:${card.id}:${kind.wire}"
        if (_state.value.isResurfacingMutationPending(card.id)) return
        _state.value = _state.value.copy(pending = _state.value.pending + key)
        viewModelScope.launch {
            val recommended = card.recommendedAction?.kind == kind.wire
            if (recommended) captured { source.recordRecommendation(card, kind, "selected") }
            captured { source.performResurfacingAction(card, kind, input) }.fold({ result ->
                if (recommended) captured { source.recordRecommendation(card, kind, "completed") }
                val state = _state.value
                _state.value = state.copy(pending = state.pending - key, actionResults = state.actionResults + (card.id to result), notice = "${titleCase(kind.wire)} completed.")
                result.resultRef?.takeIf { kind == ResurfacingActionKind.CreateTask }?.let { taskId ->
                    _state.value = _state.value.copy(navigationTaskId = taskId)
                }
            }, { error -> _state.value = _state.value.copy(pending = _state.value.pending - key, primaryError = message(error)) })
        }
    }

    fun loadBriefing(briefing: TodayBriefing) = loadMapValue(
        key = "briefing:${briefing.id}", action = { source.briefingRender(briefing.id) },
        apply = { state, value -> state.copy(briefingRenders = state.briefingRenders + (briefing.id to value)) },
    )

    fun loadAllBriefings() {
        val key = "briefings"
        if (key in _state.value.pending) return
        _state.value = _state.value.copy(pending = _state.value.pending + key)
        viewModelScope.launch { captured { source.briefings(50) }.fold({
            _state.value = _state.value.copy(briefings = it, pending = _state.value.pending - key, sectionErrors = _state.value.sectionErrors - key)
        }, { error -> _state.value = _state.value.copy(pending = _state.value.pending - key, sectionErrors = _state.value.sectionErrors + (key to message(error))) }) }
    }

    fun removeActivity(item: TodayActivityItem) {
        val previous = _state.value.activityItems
        _state.value = _state.value.copy(activityItems = previous.filterNot { it.id == item.id })
        viewModelScope.launch { captured { source.deleteActivity(item.id) }.onFailure {
            _state.value = _state.value.copy(activityItems = previous, primaryError = message(it))
        } }
    }

    fun clearActivity() {
        val previous = _state.value.activityItems
        _state.value = _state.value.copy(activityItems = emptyList())
        viewModelScope.launch {
            val failed = mutableListOf<TodayActivityItem>()
            previous.forEach { item -> captured { source.deleteActivity(item.id) }.onFailure { failed += item } }
            if (failed.isNotEmpty()) _state.value = _state.value.copy(activityItems = failed, primaryError = "Some activity could not be removed.")
        }
    }

    fun recordImpression(binding: AttentionDeliveryBinding, visibleMs: Int) {
        val identity = binding.identity
        if (identity in inFlightImpressions || identity in terminalImpressions || impressionReceipts[identity] != null || binding.expiresAt <= System.currentTimeMillis()) return
        inFlightImpressions += identity
        val eventId = impressionEventIds.getOrPut(identity) { UUID.randomUUID().toString() }
        viewModelScope.launch {
            try {
                repeat(4) { attempt ->
                    val result = captured { source.recordImpression(binding, visibleMs, eventId) }
                    result.onSuccess { receipt -> impressionReceipts[identity] = receipt; terminalImpressions += identity; return@launch }
                    val error = result.exceptionOrNull()
                    if (error is TodayApiError && error.status in 400..499) return@launch
                    if (attempt < 3) delay(500L shl attempt)
                }
            } finally {
                inFlightImpressions -= identity
                terminalImpressions += identity
            }
        }
    }

    private fun refreshFollowUps() {
        viewModelScope.launch { captured { source.followUps() }.fold({ page ->
            val state = _state.value
            val rows = page.items.filterNot { "followup:${it.id}" in state.pending }
            _state.value = state.copy(messageFollowUps = rows,
                messageFollowUpTotal = (page.total - (page.items.size - rows.size)).coerceAtLeast(rows.size),
                sectionErrors = state.sectionErrors - "message_followups"); followUpCursor = page.nextCursor
            followUpProjectionReference = page.canonicalProjectionReference; bindFollowUps(page)
        }, { setSectionError("message_followups", it) }) }
    }

    private fun refreshPulse() {
        viewModelScope.launch { captured { source.pulse(_state.value.pulse) }.fold({
            _state.value = _state.value.copy(pulse = it, sectionErrors = _state.value.sectionErrors - "pulse")
        }, { setSectionError("pulse", it) }) }
        // Agent counts ride the pulse cadence; both are fail-soft.
        viewModelScope.launch { captured { source.agents() }.onSuccess { _state.value = _state.value.copy(agentCounts = it) } }
        refreshCrew()
        viewModelScope.launch { captured { source.eventCount24h() }.onSuccess { _state.value = _state.value.copy(eventCount24h = it) } }
    }

    fun refreshCrew() {
        viewModelScope.launch {
            captured { source.crew() }.fold({
                _state.value = _state.value.copy(crew = it, sectionErrors = _state.value.sectionErrors - "crew")
            }, { setSectionError("crew", it) })
        }
    }

    private fun bindFollowUps(page: ChannelFollowUpPage) {
        val reference = page.canonicalProjectionReference ?: return
        viewModelScope.launch {
            captured { source.canonicalDeliveries("follow_up", reference, page.items.size.coerceAtLeast(1)) }.getOrNull()?.let { bindings ->
                if (followUpProjectionReference != reference) return@let
                val current = _state.value
                applyAttentionDelivery(bindings, current.messageFollowUps)?.let { _state.value = current.copy(messageFollowUps = it) }
            }
        }
    }

    private fun bindWorthALook(page: ResurfacingPage) {
        val reference = page.canonicalProjectionReference ?: return
        viewModelScope.launch {
            captured { source.canonicalDeliveries("worth_a_look", reference, page.cards.size.coerceAtLeast(1)) }.getOrNull()?.let { bindings ->
                if (worthProjectionReference != reference) return@let
                val current = _state.value
                applyWorthAttentionDelivery(bindings, current.resurfacingCards)?.let { _state.value = current.copy(resurfacingCards = it) }
            }
        }
    }

    private fun <T> loadMapValue(
        key: String, action: suspend () -> T, apply: (TodayUiState, T) -> TodayUiState,
    ) {
        if (key in _state.value.pending) return
        _state.value = _state.value.copy(pending = _state.value.pending + key)
        viewModelScope.launch { captured(action).fold({ value ->
            val state = _state.value; _state.value = apply(state, value).copy(pending = state.pending - key, sectionErrors = state.sectionErrors - key)
        }, { error ->
            val state = _state.value; _state.value = state.copy(pending = state.pending - key, sectionErrors = state.sectionErrors + (key to message(error)))
        }) }
    }

    private suspend fun delayedRefresh() { if (started) { delay(250); refresh() } }
    private fun setPrimaryError(value: String) { _state.value = _state.value.copy(primaryError = value) }
    private fun setSectionError(key: String, error: Throwable) { _state.value = _state.value.copy(sectionErrors = _state.value.sectionErrors + (key to message(error))) }
    private fun message(error: Throwable): String = error.message?.trim()?.takeIf(String::isNotEmpty) ?: "Today is unavailable."

    private suspend fun <T> captured(action: suspend () -> T): Result<T> = try {
        Result.success(action())
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Throwable) {
        Result.failure(error)
    }


    private fun suppressPendingToday(value: TodayResponse, pending: Set<String>): TodayResponse {
        val ids = pending.filter { it.startsWith("today:") }.map { it.removePrefix("today:") }.toSet()
        if (ids.isEmpty()) return value
        var state = TodayUiState(payload = value)
        TodaySection.entries.filterNot { it == TodaySection.WorthALook }.forEach { section ->
            value.sections.items(section).filter { it.id in ids }.forEach { state = removeTodayItem(state, it) }
        }
        return state.payload ?: value
    }

    private data class InitialResults(
        val today: Result<TodayResponse>, val hidden: Result<List<HiddenTodayItem>>,
        val resurfacing: Result<ResurfacingPage>, val followUps: Result<ChannelFollowUpPage>,
        val feed: Result<List<TodayActivityItem>>, val briefings: Result<List<TodayBriefing>>,
        val pulse: Result<TodayPulse>, val updates: Result<List<TodayAgentUpdate>>,
        val agents: Result<TodayAgentCounts>, val count: Result<Long>,
    )

    private companion object {
        const val READING_ROOM_KEY = "reading_room_mode"
        const val BROADSHEET_TAB_KEY = "broadsheet_tab"
    }
}

internal data class CardAnchor(val beforeId: String?, val afterId: String?, val originalIndex: Int) {
    fun insertionIndex(currentIds: List<String>): Int {
        beforeId?.let { before -> currentIds.indexOf(before).takeIf { it >= 0 }?.let { return it + 1 } }
        afterId?.let { after -> currentIds.indexOf(after).takeIf { it >= 0 }?.let { return it } }
        return originalIndex.coerceIn(0, currentIds.size)
    }

    companion object {
        fun capture(ids: List<String>, id: String): CardAnchor {
            val index = ids.indexOf(id).coerceAtLeast(0)
            return CardAnchor(ids.getOrNull(index - 1), ids.getOrNull(index + 1), index)
        }
    }
}

internal fun removeTodayItem(state: TodayUiState, item: TodayItem): TodayUiState {
    val payload = state.payload ?: return state
    val section = TodaySection.fromWire(item.section)
    if (payload.sections.items(section).none { it.id == item.id }) return state
    val sections = payload.sections.replacing(section, payload.sections.items(section).filterNot { it.id == item.id })
    val counts = payload.counts.adjusted(section, -1)
    return state.copy(payload = payload.copy(sections = sections, counts = counts))
}

internal fun insertTodayItem(state: TodayUiState, item: TodayItem, section: TodaySection, index: Int): TodayUiState {
    val payload = state.payload ?: return state
    val existing = payload.sections.items(section)
    if (existing.any { it.id == item.id }) return state
    val rows = existing.toMutableList().apply { add(index.coerceIn(0, size), item) }
    return state.copy(payload = payload.copy(sections = payload.sections.replacing(section, rows), counts = payload.counts.adjusted(section, 1)))
}

private fun TodayCounts.adjusted(section: TodaySection, delta: Int): TodayCounts = when (section) {
    TodaySection.NeedsYou -> copy(needsYou = (needsYou + delta).coerceAtLeast(0), total = (total + delta).coerceAtLeast(0))
    TodaySection.FollowUps -> copy(followups = (followups + delta).coerceAtLeast(0), total = (total + delta).coerceAtLeast(0))
    TodaySection.ActiveWork -> copy(activeWork = (activeWork + delta).coerceAtLeast(0), total = (total + delta).coerceAtLeast(0))
    TodaySection.Delivered -> copy(delivered = (delivered + delta).coerceAtLeast(0), total = (total + delta).coerceAtLeast(0))
    TodaySection.Changed -> copy(changed = (changed + delta).coerceAtLeast(0), total = (total + delta).coerceAtLeast(0))
    TodaySection.WorthALook -> this
}
