package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.toFailure
import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import ai.magicbeans.magdroid.chat.PendingHitlTracker
import ai.magicbeans.magdroid.tasks.PlanApprovalActions
import ai.magicbeans.magdroid.tasks.PlanApprovalChoice
import ai.magicbeans.magdroid.tasks.PlanApprovalOutcome
import ai.magicbeans.magdroid.tasks.PlanApprovalPost
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

data class AttentionUiState(
    val lane: AttentionLane = AttentionLane.All,
    /**
     * Reading what has already been answered rather than what is waiting.
     *
     * A mode rather than a lane: history is not a category of pending work, and
     * putting it in the lane row would make it look like one more thing to do.
     */
    val showHistory: Boolean = false,
    val resolved: List<ResolvedHitl> = emptyList(),
    val historyLoading: Boolean = false,
    val feed: AttentionFeedResponse = AttentionFeedResponse(),
    val loading: Boolean = false,
    /** Paging the current lane, which is a different thing from loading it. */
    val loadingMore: Boolean = false,
    /**
     * A failed action — dismissing a card, answering a request. The feed itself
     * is fine, so this is a line over readable content rather than a state.
     */
    val error: String? = null,
    /**
     * A failed read of the feed itself, classified.
     *
     * Separate from [error] because the screen does different things with them:
     * this one replaces the list, and must never be shown as "nothing needs
     * you" — not knowing is not the same as nothing.
     */
    val failure: Failure? = null,
    /** The Messages lane reads a different endpoint, and fails on its own. */
    val followUpsFailure: Failure? = null,
    val setupRequired: Boolean = false,
    /** Ids removed locally while their dismissal is in flight. */
    val dismissing: Set<String> = emptySet(),
    /** The last card taken off the list, so it can be put back. */
    val lastDismissed: AttentionItem? = null,
    /** The item whose answer form is open, if any. */
    val answering: AttentionItem? = null,
    val submitting: Boolean = false,
    val answerError: String? = null,
    /**
     * Said once after the sheet closed on its own terms: its ask was settled
     * on another surface (otherwise the sheet just vanishing reads as a
     * crash), or a plan decision landed and this is what it did.
     */
    val answerNotice: String? = null,
    /** Messages waiting on a reply — a separate endpoint from the feed. */
    val followUps: List<ChannelFollowUp> = emptyList(),
    val followUpsCursor: String? = null,
    /** Ids hidden while their resolution is in flight. */
    val resolving: Set<String> = emptySet(),
    /** Stalled-wait cards whose cancel is in flight. */
    val cancelling: Set<String> = emptySet(),
    val approvingAll: Boolean = false,
    /** What a bulk run did, until the owner dismisses it. */
    val bulkNotice: String? = null,
) {
    /**
     * Everything in this lane that a single tap could apply.
     *
     * A code change set is the one kind where reviewing each card individually
     * is usually not what the owner wants — they have already decided to take
     * the branch. Nothing else is bulk-approvable, deliberately.
     */
    val diffApprovals: List<AttentionItem>
        get() = items.filter { it.isActionable && it.request(it.metadata).inputType == "diff_approval" }

    val visibleFollowUps: List<ChannelFollowUp>
        get() = followUps.filterNot { it.id in resolving }

    val items: List<AttentionItem>
        // Optimistically-removed rows are filtered here rather than deleted
        // from the feed, so a failed dismissal restores by forgetting one id
        // instead of rebuilding the lane it came from.
        get() = feed.items(lane).filterNot { it.id in dismissing }
    val hasMore: Boolean
        get() = if (lane == AttentionLane.Messages) !followUpsCursor.isNullOrBlank()
        else feed.page(lane).hasMore
    fun count(of: AttentionLane): Long =
        if (of == AttentionLane.Messages) visibleFollowUps.size.toLong() else feed.laneCount(of)
}

/**
 * What is waiting, and which slice of it is on screen.
 *
 * The lane lives here rather than in the composable so that switching tabs does
 * not re-fetch: one response carries every lane, and the tabs are a view onto
 * it. Refetching per tab would make five taps five round trips for data already
 * in hand.
 */
class AttentionViewModel(app: Application) : AndroidViewModel(app) {

    private val repository = AttentionRepository(app)
    private val _state = MutableStateFlow(AttentionUiState())
    val state: StateFlow<AttentionUiState> = _state.asStateFlow()

    /** Cursors already spent, so a lane is never asked for the same page twice. */
    private val cursors = mutableMapOf<AttentionLane, String>()

    /**
     * A refresh asked for while one was in flight. Run once more when it lands,
     * so a live event that arrives mid-fetch is not lost to the in-flight guard.
     */
    private var refreshQueued = false

    init {
        refresh()
    }

    fun selectLane(lane: AttentionLane) {
        _state.value = _state.value.copy(lane = lane)
    }

    /**
     * Switch between what is waiting and what has been answered.
     *
     * The history is fetched on entry rather than kept warm: it is read
     * occasionally and a resolved request does not change again.
     */
    fun showHistory(show: Boolean) {
        _state.value = _state.value.copy(showHistory = show)
        if (!show) return
        _state.value = _state.value.copy(historyLoading = true)
        viewModelScope.launch {
            val rows = runCatching { repository.resolved() }.getOrElse { emptyList() }
            _state.value = _state.value.copy(resolved = rows, historyLoading = false)
        }
    }

    fun refresh() {
        if (_state.value.loading) {
            refreshQueued = true
            return
        }
        _state.value = _state.value.copy(
            loading = true,
            error = null,
            failure = null,
            followUpsFailure = null,
            setupRequired = false,
        )
        viewModelScope.launch {
            // Follow-ups are fetched alongside and allowed to fail on their
            // own. A comms outage should not blank the paused work, and a feed
            // error should not hide messages that are perfectly readable.
            launch {
                runCatching { repository.followUps() }
                    .onSuccess { page ->
                        _state.value = _state.value.copy(
                            followUps = page.items,
                            followUpsCursor = page.nextCursor,
                            followUpsFailure = null,
                            resolving = emptySet(),
                        )
                    }
                    // Recorded rather than swallowed. Without this the Messages
                    // lane showed "No messages waiting." after a failed read,
                    // which is the one thing it could not know.
                    .onFailure { problem ->
                        _state.value = _state.value.copy(
                            followUpsFailure = problem.toFailure(getApplication(), "waiting messages"),
                        )
                    }
            }
            runCatching { repository.feed() }
                .onSuccess { feed ->
                    cursors.clear()
                    // The open sheet's ask was answered on another surface
                    // (web, iOS, a push action, chat). Close it with a notice
                    // instead of leaving a form whose submit would be refused
                    // — or worse, accepted against a stale question. Never
                    // while our own answer is in flight: that path owns the
                    // close.
                    val open = _state.value.answering
                    val settled = open != null && !_state.value.submitting &&
                        if (exactAnswerKey == open.askKey) {
                            // The exact lookup, not absence from a paged list,
                            // is authoritative for a deep-linked request.
                            repository.pendingItem(open.askKey)?.isActionable != true
                        } else feed.settledElsewhere(open)
                    _state.value = _state.value.copy(
                        feed = feed,
                        loading = false,
                        error = null,
                        failure = null,
                        // The response is now the truth about what is listed,
                        // so local removals stop being needed. Keeping them
                        // would hide a card the server has restored.
                        dismissing = emptySet(),
                    ).let { next ->
                        if (settled) {
                            next.copy(answering = null, answerError = null, answerNotice = ANSWERED_ELSEWHERE)
                        } else next
                    }
                }
                .onFailure { problem ->
                    val failure = problem.toFailure(getApplication(), "what needs you")
                    _state.value = _state.value.copy(
                        loading = false,
                        failure = failure,
                        setupRequired = failure.setupRequired,
                    )
                }
            if (refreshQueued) {
                refreshQueued = false
                refresh()
            }
        }
    }

    /**
     * Keep the list current while the screen is on.
     *
     * The feed had no live path: it refreshed on open, resume, pull and its own
     * submits, so an ask raised or answered on another surface stayed as it
     * was until the owner did something. The shared realtime socket already
     * feeds every frame to [PendingHitlTracker]; a `HitlRequested` or a
     * resolution there refreshes this feed (debounced, since one pause emits a
     * small burst). The socket is the chat screen's, so while an answer sheet
     * is open a slow poll backs it up — that sheet is the one place a missed
     * frame lets the owner act on something already settled.
     *
     * Suspends until cancelled; run it from a lifecycle-bound effect so a
     * hidden screen does not keep fetching.
     */
    suspend fun followLive() = coroutineScope {
        launch {
            PendingHitlTracker.changes.drop(1).collectLatest {
                delay(LIVE_REFRESH_DEBOUNCE_MS)
                refresh()
            }
        }
        while (isActive) {
            delay(OPEN_SHEET_POLL_MS)
            if (_state.value.answering != null && !_state.value.submitting) refresh()
        }
    }

    fun clearAnswerNotice() {
        _state.value = _state.value.copy(answerNotice = null)
    }

    /**
     * Extend the lane on screen.
     *
     * Only that lane's cursor is sent, so the other four stay where they are.
     * The returned page replaces the lane's rows rather than appending, because
     * the server returns a window and appending a window that overlaps would
     * duplicate what is already listed — `merged()` dedupes All for the same
     * reason.
     */
    fun loadMore() {
        val current = _state.value
        val lane = current.lane
        if (current.loadingMore || !current.hasMore) return
        val next = current.feed.page(lane).nextCursor ?: return
        if (cursors[lane] == next) return

        _state.value = current.copy(loadingMore = true)
        viewModelScope.launch {
            runCatching {
                repository.feed(
                    requestsCursor = next.takeIf { lane == AttentionLane.Requests },
                    approvalsCursor = next.takeIf { lane == AttentionLane.Approvals },
                    escalationsCursor = next.takeIf { lane == AttentionLane.Escalations },
                    failedCursor = next.takeIf { lane == AttentionLane.Failed },
                )
            }
                .onSuccess { page ->
                    cursors[lane] = next
                    _state.value = _state.value.copy(feed = page, loadingMore = false)
                }
                .onFailure {
                    // The rows already on screen stay. A failed page is not a
                    // reason to empty a list the owner is reading.
                    _state.value = _state.value.copy(
                        loadingMore = false,
                        error = it.message ?: "Could not load more.",
                    )
                }
        }
    }

    /**
     * Take a failed card off the list.
     *
     * The row goes immediately and comes back if the server refuses. A card
     * that lingers until a round trip finishes reads as a tap that did not
     * register, and the second tap is the one that causes trouble — so the
     * in-flight id is also what stops a double send.
     */
    fun dismiss(item: AttentionItem) {
        if (item.id in _state.value.dismissing) return
        _state.value = _state.value.copy(
            dismissing = _state.value.dismissing + item.id,
            lastDismissed = item,
            error = null,
        )
        viewModelScope.launch {
            runCatching { repository.setDismissed(item.id, dismissed = true) }
                .onSuccess {
                    // Refetch rather than trusting the local edit: dismissing
                    // changes the lane totals and the badge, and both are the
                    // server's to state.
                    refresh()
                }
                .onFailure { problem ->
                    _state.value = _state.value.copy(
                        dismissing = _state.value.dismissing - item.id,
                        lastDismissed = null,
                        error = problem.message ?: "Could not dismiss this card. It has been restored.",
                    )
                }
        }
    }

    /** Put the last dismissed card back. */
    fun undoDismiss() {
        val item = _state.value.lastDismissed ?: return
        _state.value = _state.value.copy(lastDismissed = null)
        viewModelScope.launch {
            runCatching { repository.setDismissed(item.id, dismissed = false) }
                .onSuccess { refresh() }
                .onFailure { problem ->
                    _state.value = _state.value.copy(
                        error = problem.message ?: "Could not restore this card.",
                    )
                }
        }
    }

    /** Open the answer form, but only for something still waiting. */
    fun openAnswer(item: AttentionItem) {
        if (!item.isActionable) return
        exactAnswerKey = null
        _state.value = _state.value.copy(answering = item, answerError = null, answerNotice = null)
    }

    private var exactAnswerKey: String? = null
    private var openingTarget: String? = null
    fun openAnswerById(id: String, consumed: () -> Unit) {
        if (openingTarget == id) return
        openingTarget = id
        viewModelScope.launch {
            try {
                val item = repository.pendingItem(id)
                if (openingTarget != id) return@launch
                if (item?.isActionable == true) {
                    openAnswer(item)
                    exactAnswerKey = item.askKey
                }
                else _state.value = _state.value.copy(answerNotice = "This request is no longer pending.")
                consumed()
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (problem: Exception) {
                if (openingTarget == id) _state.value = _state.value.copy(error = problem.message ?: "Could not open this request.")
            } finally {
                if (openingTarget == id) openingTarget = null
            }
        }
    }

    fun closeAnswer() {
        _state.value = _state.value.copy(answering = null, answerError = null, submitting = false)
    }

    /**
     * Send an answer and close on success.
     *
     * The form stays open when the server refuses, with the reason on it —
     * closing would leave the owner believing they answered something that is
     * still paused.
     */
    /** The owner-private read of an alignment hold, for its answer sheet. */
    suspend fun alignmentDetail(correlationId: String): AlignmentDetailOutcome =
        repository.alignmentDetail(correlationId)

    fun submitAnswer(value: ai.magicbeans.magdroid.chat.HitlResponseValue) {
        // A closed sheet has nothing to answer: a settled-elsewhere refresh
        // clears `answering`, which is what keeps a late tap from posting.
        val item = _state.value.answering ?: return
        if (_state.value.submitting) return
        _state.value = _state.value.copy(submitting = true, answerError = null)
        val request = item.request(item.metadata)
        viewModelScope.launch {
            runCatching { repository.respond(request, value) }
                .onSuccess {
                    _state.value = _state.value.copy(answering = null, submitting = false)
                    // The composer dock and a task's deep panel show this ask
                    // too; have them re-read now rather than at the socket's
                    // leisure.
                    PendingHitlTracker.resync()
                    refresh()
                }
                .onFailure { problem ->
                    _state.value = if (problem is AttentionAlreadyAnswered) {
                        // Lost the race to another surface. Nothing to retry.
                        _state.value.copy(
                            answering = null,
                            submitting = false,
                            answerError = null,
                            answerNotice = ANSWERED_ELSEWHERE,
                        )
                    } else {
                        _state.value.copy(
                            submitting = false,
                            answerError = problem.message ?: "Could not send the answer.",
                        )
                    }
                    if (problem is AttentionAlreadyAnswered) refresh()
                }
        }
    }

    /**
     * Answer a plan approval: Reject, Approve, or Approve & Run.
     *
     * Approving a plan does not start a run, so Approve & Run is two calls —
     * the same confirmation Approve posts, then `POST /v3/tasks/{id}/execute`
     * (web `startRunAfterPlanApproval`). A failed start keeps the approval and
     * says so; the task's Run is still there. Review posts nothing and is the
     * screen's to handle.
     */
    fun answerPlan(choice: PlanApprovalChoice) {
        val item = _state.value.answering ?: return
        if (_state.value.submitting || choice == PlanApprovalChoice.Review) return
        val request = item.request(item.metadata)
        val taskId = item.openTaskId
        _state.value = _state.value.copy(submitting = true, answerError = null)
        viewModelScope.launch {
            var settledElsewhere = false
            suspend fun post(confirmed: Boolean): PlanApprovalPost = try {
                repository.respond(
                    request,
                    ai.magicbeans.magdroid.chat.HitlResponseValue.Confirmation(confirmed),
                    taskId = taskId,
                )
                PlanApprovalPost.Ok
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: AttentionAlreadyAnswered) {
                settledElsewhere = true
                PlanApprovalPost.Failed(ANSWERED_ELSEWHERE)
            } catch (problem: Exception) {
                PlanApprovalPost.Failed(problem.message ?: "Could not send the answer.")
            }
            fun outcomeOf(posted: PlanApprovalPost, ok: PlanApprovalOutcome) = when (posted) {
                PlanApprovalPost.Ok -> ok
                is PlanApprovalPost.Failed -> PlanApprovalOutcome.Failed(posted.reason)
            }
            val outcome = when (choice) {
                PlanApprovalChoice.Reject -> outcomeOf(post(false), PlanApprovalOutcome.Rejected)
                PlanApprovalChoice.Approve -> outcomeOf(post(true), PlanApprovalOutcome.Approved)
                PlanApprovalChoice.ApproveAndRun -> PlanApprovalActions.approveAndRun(
                    taskId = taskId,
                    approve = { post(true) },
                    startRun = { repository.startRun(it, request.originConnection) },
                )
                PlanApprovalChoice.Review -> return@launch
            }
            _state.value = when {
                settledElsewhere -> _state.value.copy(
                    answering = null, submitting = false, answerError = null, answerNotice = ANSWERED_ELSEWHERE,
                )
                // Nothing changed server-side; the sheet stays with the reason.
                outcome is PlanApprovalOutcome.Failed -> _state.value.copy(
                    submitting = false, answerError = outcome.message,
                )
                else -> _state.value.copy(
                    answering = null, submitting = false, answerError = null, answerNotice = outcome.message,
                )
            }
            if (settledElsewhere || outcome !is PlanApprovalOutcome.Failed) {
                PendingHitlTracker.resync()
                refresh()
            }
        }
    }

    /**
     * Resolve a waiting message.
     *
     * The card goes at once and stays gone: unlike a dismissed feed card there
     * is no undo, because `useful` and `acknowledge` are training signals as
     * well as resolutions and taking one back would teach the wrong thing.
     * A refusal restores it and says why.
     */
    fun resolveFollowUp(followUp: ChannelFollowUp, action: FollowUpAction, reason: String? = null) {
        if (followUp.id in _state.value.resolving) return
        if (action == FollowUpAction.Acknowledge && !followUp.canAcknowledge) return
        _state.value = _state.value.copy(
            resolving = _state.value.resolving + followUp.id,
            error = null,
        )
        viewModelScope.launch {
            runCatching { repository.resolveFollowUp(followUp.id, action, reason) }
                .onSuccess { refresh() }
                .onFailure { problem ->
                    _state.value = _state.value.copy(
                        resolving = _state.value.resolving - followUp.id,
                        error = problem.message ?: "Could not resolve that message.",
                    )
                }
        }
    }

    /**
     * Cancel the run behind a stalled-wait card (C3).
     *
     * The card stays until the refresh says it is gone — the server clears it
     * when the run leaves the waiting state, and removing it locally first
     * would hide a cancel that did not take.
     */
    fun cancelStalled(item: AttentionItem) {
        val executionId = item.stalledExecutionId ?: run {
            _state.value = _state.value.copy(error = "This run has no id to cancel. Open the task instead.")
            return
        }
        if (item.id in _state.value.cancelling) return
        _state.value = _state.value.copy(cancelling = _state.value.cancelling + item.id, error = null)
        viewModelScope.launch {
            runCatching { repository.cancelExecution(executionId) }
                .onSuccess {
                    _state.value = _state.value.copy(cancelling = _state.value.cancelling - item.id)
                    refresh()
                }
                .onFailure { problem ->
                    _state.value = _state.value.copy(
                        cancelling = _state.value.cancelling - item.id,
                        error = problem.message ?: "Could not cancel that run.",
                    )
                }
        }
    }

    fun clearBulkNotice() {
        _state.value = _state.value.copy(bulkNotice = null)
    }

    /**
     * Apply every code change set in this lane.
     *
     * Each is answered with the same `apply` choice one at a time. Sequential
     * rather than concurrent: these mutate a working tree, and a server
     * applying several at once is a merge nobody asked for.
     *
     * The count is reported honestly — applied and failed separately — because
     * a bulk action that says "done" while two of six failed is worse than one
     * that says so.
     */
    fun approveAllDiffApprovals() {
        val targets = _state.value.diffApprovals
        if (_state.value.approvingAll || targets.isEmpty()) return
        _state.value = _state.value.copy(approvingAll = true, bulkNotice = null, error = null)
        viewModelScope.launch {
            var applied = 0
            var failed = 0
            targets.forEach { item ->
                val request = item.request(item.metadata)
                runCatching {
                    repository.respond(
                        request,
                        ai.magicbeans.magdroid.chat.HitlResponseValue.Choice(selectedId = "apply"),
                    )
                }.onSuccess { applied += 1 }.onFailure { failed += 1 }
            }
            _state.value = _state.value.copy(
                approvingAll = false,
                bulkNotice = when {
                    failed == 0 -> "Applied $applied code change set${if (applied == 1) "" else "s"}."
                    applied == 0 -> "None of the $failed could be applied."
                    else -> "Applied $applied, $failed could not be applied."
                },
            )
            // One refresh at the end rather than per item: the lane totals and
            // the badge move once, and refreshing per card would make the list
            // jump under whoever is watching it.
            refresh()
        }
    }

    override fun onCleared() {
        repository.close()
        super.onCleared()
    }

    private companion object {
        const val ANSWERED_ELSEWHERE = "Answered elsewhere. Nothing more is needed here."
        const val LIVE_REFRESH_DEBOUNCE_MS = 400L
        const val OPEN_SHEET_POLL_MS = 15_000L
    }
}
