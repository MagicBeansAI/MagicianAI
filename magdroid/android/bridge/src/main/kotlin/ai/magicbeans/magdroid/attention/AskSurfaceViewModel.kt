package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.HitlResponseValue
import ai.magicbeans.magdroid.chat.PendingHitlTracker
import ai.magicbeans.magdroid.tasks.PlanApprovalActions
import ai.magicbeans.magdroid.tasks.PlanApprovalChoice
import ai.magicbeans.magdroid.tasks.PlanApprovalOutcome
import ai.magicbeans.magdroid.tasks.PlanApprovalPost
import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** What the in-place ask surfaces (composer dock, task deep panel) share. */
data class AskSurfaceState(
    val feed: AttentionFeedResponse = AttentionFeedResponse(),
    val loaded: Boolean = false,
    /**
     * Ids of asks answered (or found answered) from this phone. Hidden at once
     * rather than when the next read drops them, so a row the execution panel
     * still lists until its next poll does not offer a settled question.
     */
    val settled: Set<String> = emptySet(),
    /** The ask whose answer is in flight, by [askKey]. */
    val submitting: String? = null,
    /** Why an answer did not land, by [askKey]. */
    val errors: Map<String, String> = emptyMap(),
    /** Said once after an answer: answered elsewhere, or what a plan decision did. */
    val notice: String? = null,
    /** The ask open in the secure sheet (secrets, grants), when one is. */
    val sheet: AttentionItem? = null,
    /** Whether [sheet] came from the feed, so a fresh feed can say it was settled. */
    val sheetFromFeed: Boolean = false,
    /**
     * Which surface opened [sheet] (`dock`, `task`). The chat stays composed
     * under a pushed task detail, so without this both would draw it.
     */
    val sheetOwner: String? = null,
)

/**
 * Answers asks where they are shown instead of only in Attention.
 *
 * Reads the same feed Attention reads (`/feed/attention` plus the pending
 * user-request ledger) and posts through the same `/hitl/{id}/respond`, so an
 * ask answered here clears from Attention, the dock and the deep panel alike:
 * the server emits the resolution, every surface following
 * [PendingHitlTracker] re-reads, and this one also hides the ask immediately.
 *
 * Activity-scoped when obtained with `viewModel()`, so the chat dock and a
 * task detail pushed over it share one feed and one in-flight answer.
 */
class AskSurfaceViewModel(app: Application) : AndroidViewModel(app) {

    private val appContext = app.applicationContext
    private var displayedConnection = ai.magicbeans.magdroid.access.MagicianAccess.connectionSnapshot(appContext)
    private val repository = AttentionRepository(app)
    private val _state = MutableStateFlow(AskSurfaceState())
    val state: StateFlow<AskSurfaceState> = _state.asStateFlow()

    private var loading = false
    private var refreshQueued = false

    suspend fun alignmentDetail(correlationId: String): AlignmentDetailOutcome =
        repository.alignmentDetail(correlationId)

    fun refresh() {
        val connection = ai.magicbeans.magdroid.access.MagicianAccess.connectionSnapshot(appContext)
        if (connection != displayedConnection) {
            displayedConnection = connection
            _state.value = AskSurfaceState()
        }
        if (loading) {
            refreshQueued = true
            return
        }
        loading = true
        viewModelScope.launch {
            runCatching { repository.pendingFeed() }
                .onSuccess { feed ->
                    val current = _state.value
                    val open = current.sheet
                    val settledElsewhere = open != null && current.sheetFromFeed &&
                        current.submitting == null && feed.settledElsewhere(open)
                    _state.value = current.copy(feed = feed, loaded = true).let { next ->
                        if (settledElsewhere) {
                            next.copy(
                                sheet = null,
                                settled = next.settled + open!!.askIds,
                                notice = ANSWERED_ELSEWHERE,
                            )
                        } else next
                    }
                }
            // A failed read keeps what was shown; Attention owns saying the
            // feed is unreachable, and an ask dock is not where to say it.
            loading = false
            if (refreshQueued) {
                refreshQueued = false
                refresh()
            }
        }
    }

    /**
     * Keep the asks current while a surface is visible: re-read on every live
     * HITL frame (debounced), and slowly otherwise — a user-request ask (a
     * Relentless review) is raised without a `HitlRequested` frame, so the
     * socket alone would not bring it in. Run from a lifecycle-bound effect.
     */
    suspend fun followLive() {
        // The dock and a task detail pushed over it can both be visible; they
        // share one live loop rather than each polling.
        followers += 1
        if (followers == 1) {
            liveJob = viewModelScope.launch { liveLoop() }
        } else {
            refresh()
        }
        try {
            awaitCancellation()
        } finally {
            followers -= 1
            if (followers == 0) {
                liveJob?.cancel()
                liveJob = null
            }
        }
    }

    private var followers = 0
    private var liveJob: Job? = null

    private suspend fun liveLoop() = coroutineScope {
        refresh()
        launch {
            ai.magicbeans.magdroid.access.MagicianAccess.activeMachine.collect { refresh() }
        }
        launch {
            PendingHitlTracker.changes.drop(1).collectLatest {
                delay(LIVE_REFRESH_DEBOUNCE_MS)
                refresh()
            }
        }
        while (isActive) {
            delay(POLL_MS)
            if (_state.value.submitting == null) refresh()
        }
    }

    fun openSheet(item: AttentionItem, owner: String) {
        if (!item.isActionable) return
        val fromFeed = AskSurfaces.open(_state.value.feed).any { row -> row.askIds.any(item.askIds::contains) }
        _state.value = _state.value.copy(sheet = item, sheetFromFeed = fromFeed, sheetOwner = owner, notice = null)
    }

    fun closeSheet() {
        if (_state.value.submitting != null) return
        _state.value = _state.value.copy(sheet = null)
    }

    fun clearNotice() {
        _state.value = _state.value.copy(notice = null)
    }

    /**
     * Post one answer. The ask leaves every in-place surface on success, and
     * on "already answered" too — that is the stale-sheet guard: the decision
     * is no longer needed, so the form closes with a notice instead of
     * offering a retry that would be refused again.
     */
    fun answer(item: AttentionItem, value: HitlResponseValue) {
        val key = item.askKey
        if (_state.value.submitting != null || item.askIds.any(_state.value.settled::contains)) return
        _state.value = _state.value.copy(submitting = key, errors = _state.value.errors - key, notice = null)
        val request = item.request(item.metadata)
        viewModelScope.launch {
            val outcome = try {
                repository.respond(request, value, taskId = item.openTaskId)
                null
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: AttentionAlreadyAnswered) {
                ANSWERED_ELSEWHERE
            } catch (problem: Exception) {
                _state.value = _state.value.copy(
                    submitting = null,
                    errors = _state.value.errors + (key to (problem.message ?: "Could not send the answer.")),
                )
                refresh()
                return@launch
            }
            settle(item, notice = outcome)
        }
    }

    /**
     * A plan approval: Reject, Approve, or Approve & Run — the same two calls
     * Attention makes (approve, then `POST /v3/tasks/{id}/execute`).
     */
    fun answerPlan(item: AttentionItem, choice: PlanApprovalChoice) {
        if (choice == PlanApprovalChoice.Review || _state.value.submitting != null) return
        val key = item.askKey
        val request = item.request(item.metadata)
        val taskId = item.openTaskId
        _state.value = _state.value.copy(submitting = key, errors = _state.value.errors - key, notice = null)
        viewModelScope.launch {
            var settledElsewhere = false
            suspend fun post(confirmed: Boolean): PlanApprovalPost = try {
                repository.respond(request, HitlResponseValue.Confirmation(confirmed), taskId = taskId)
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
            when {
                settledElsewhere -> settle(item, ANSWERED_ELSEWHERE)
                outcome is PlanApprovalOutcome.Failed -> _state.value = _state.value.copy(
                    submitting = null,
                    errors = _state.value.errors + (key to outcome.message),
                )
                else -> settle(item, outcome.message)
            }
        }
    }

    private fun settle(item: AttentionItem, notice: String?) {
        if (item.originConnection != null && item.originConnection != ai.magicbeans.magdroid.access.MagicianAccess.connectionSnapshot(appContext)) {
            refresh()
            return
        }
        val ids = item.askIds
        ids.forEach(PendingHitlTracker::drop)
        val current = _state.value
        _state.value = current.copy(
            submitting = null,
            settled = current.settled + ids,
            notice = notice,
            sheet = current.sheet?.takeUnless { open -> open.askIds.any(ids::contains) },
        )
        // Tell every other surface following the tracker (Attention, the
        // badge) to re-read now rather than at the socket's leisure.
        PendingHitlTracker.resync()
        refresh()
    }

    override fun onCleared() {
        repository.close()
        super.onCleared()
    }

    companion object {
        const val ANSWERED_ELSEWHERE = "Answered elsewhere. Nothing more is needed here."
        private const val LIVE_REFRESH_DEBOUNCE_MS = 400L
        private const val POLL_MS = 30_000L
    }
}
