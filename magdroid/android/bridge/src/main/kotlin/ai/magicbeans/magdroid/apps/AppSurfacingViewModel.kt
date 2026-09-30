package ai.magicbeans.magdroid.apps

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
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.time.Instant
import java.util.UUID

data class AppWidgetPageUiState(
    val page: String,
    val regions: List<AppSlotRegionSpec>,
    val loading: Boolean = false,
    val snapshot: AppWidgetPageSnapshot? = null,
    val error: String? = null,
)

data class AppActionNotice(
    val installationId: String,
    val actionId: String,
    val message: String,
    val succeeded: Boolean,
)

/**
 * The owner's slot editor for exactly one region of the active page.
 *
 * One editor at a time is not a simplification: a settings read advances the
 * host's write fence, so a second open picker would silently invalidate the
 * first one's ability to write.
 */
data class AppSlotEditorUiState(
    /** The region whose picker or failure this editor belongs to. */
    val region: String? = null,
    val settings: AppSlotSettingsPage? = null,
    val loading: Boolean = false,
    val mutating: Boolean = false,
    /** An ambiguous write is retained for an exact replay, never re-minted. */
    val retryable: Boolean = false,
    val error: String? = null,
) {
    val pickerOpen: Boolean get() = region != null && (loading || settings != null)

    /** While anything is unresolved no other slot control may start a write. */
    val busy: Boolean get() = loading || mutating || retryable
}

/**
 * One app's canonical entity route, open over the shell page.
 *
 * The shell owns this rather than a widget card, because pushing the entity
 * route retires the card's own region: a sheet mounted inside it would be
 * disposed by the very state change that opened it.
 */
data class AppEntitySurface(
    val installationId: String,
    val page: String,
    val title: String,
)

data class AppSurfacingUiState(
    val activePage: AppWidgetPageUiState? = null,
    val entitySurface: AppEntitySurface? = null,
    val pageSnapshots: Map<String, AppWidgetPageSnapshot> = emptyMap(),
    val indicators: List<AppMaterializedIndicator> = emptyList(),
    val indicatorEtag: String? = null,
    val indicatorRefreshAfter: Instant? = null,
    val actionsInFlight: Set<String> = emptySet(),
    val actionNotice: AppActionNotice? = null,
    val slotEditor: AppSlotEditorUiState = AppSlotEditorUiState(),
)

/** Foreground-only owner for slots, batched widget refresh and indicators. */
class AppSurfacingViewModel(app: Application) : AndroidViewModel(app) {
    private val repository = AppSurfacingRepository(app)
    private val refreshMutex = Mutex()
    private val _state = MutableStateFlow(AppSurfacingUiState())
    val state: StateFlow<AppSurfacingUiState> = _state.asStateFlow()

    private var foreground = false
    private var pollJob: Job? = null
    private var immediateRefreshJob: Job? = null
    private val actionIdempotencyKeys = mutableMapOf<String, String>()
    private var appliedAuthorityKey: String? = null
    private var slotEditorJob: Job? = null
    private var pendingSlotMutation: PendingSlotMutation? = null
    private var pickerCursors = mutableSetOf<String>()
    private var entityBacktrack: AppWidgetPageUiState? = null

    fun onForeground() {
        if (foreground) return
        foreground = true
        val authorityKey = repository.authorityKey()
        if (authorityKey != appliedAuthorityKey) {
            resetSlotEditor()
            _state.value = AppSurfacingUiState(
                activePage = retreatedPage()?.copy(loading = true, snapshot = null, error = null),
            )
            appliedAuthorityKey = authorityKey
            actionIdempotencyKeys.clear()
        } else {
            markActivePageRevalidating()
        }
        refreshNow()
    }

    fun onBackground() {
        foreground = false
        pollJob?.cancel()
        pollJob = null
        immediateRefreshJob?.cancel()
        immediateRefreshJob = null
    }

    /** A tab transition always revalidates its slot assignment and ETag. */
    fun selectPage(page: String?, regions: List<String> = emptyList()) {
        entityBacktrack = null
        if (_state.value.entitySurface != null) {
            _state.value = _state.value.copy(entitySurface = null)
        }
        applyPage(page, regions)
    }

    /**
     * Fits the `contextual` region of one app's canonical entity route.
     *
     * The route is derived, never received: a slot assignment is data shared
     * with every other client, so the page a handset fits must be the page the
     * browser fits for the same installation. Returns false when the identity
     * cannot form a bounded static route, and nothing is opened.
     */
    fun openEntityPage(
        installationId: String,
        title: String,
        surfacePath: String? = null,
    ): Boolean {
        val page = appSurfaceSlotPage(installationId, surfacePath) ?: return false
        val previous = _state.value.activePage
        applyPage(page, listOf(AppSlotContextualRegion))
        entityBacktrack = previous
        _state.value = _state.value.copy(
            entitySurface = AppEntitySurface(installationId, page, title),
        )
        return true
    }

    /** Restores the shell page the entity route was opened over. */
    fun closeEntityPage() {
        val previous = entityBacktrack
        entityBacktrack = null
        _state.value = _state.value.copy(entitySurface = null)
        if (previous == null) {
            applyPage(null, emptyList())
            return
        }
        applyPage(previous.page, previous.regions.map(AppSlotRegionSpec::region))
    }

    private fun applyPage(page: String?, regions: List<String>) {
        val next = page?.let { route ->
            require(regions.isNotEmpty() && regions.size <= AppSurfacingLimits.MaximumSlotsPerPage)
            val specs = regions.map { region -> AppSlotRegionSpec(route, region) }
            AppWidgetPageUiState(
                page = route,
                regions = specs,
                loading = true,
                snapshot = _state.value.pageSnapshots[pageCacheKey(route, specs)],
            )
        }
        val current = _state.value.activePage
        // A write fence belongs to the page it was acquired for. Moving to a
        // different binding retires the editor rather than carrying it across.
        if (next == null || current?.page != next.page || current.regions != next.regions) {
            resetSlotEditor()
        }
        _state.value = _state.value.copy(
            activePage = if (next != null && current?.page == next.page && current.regions == next.regions) {
                current.copy(loading = true, error = null)
            } else {
                next
            },
        )
        if (foreground) refreshNow()
    }

    fun refreshNow() {
        if (!foreground) return
        markActivePageRevalidating()
        immediateRefreshJob?.cancel()
        immediateRefreshJob = viewModelScope.launch {
            refresh(force = true)
            if (foreground) scheduleNextPoll()
        }
    }

    fun launchEmptyAction(
        assignment: AppResolvedSlotAssignment,
        item: AppWidgetRenderItem,
        actionId: String,
    ) {
        val authorityKey = repository.authorityKey()
        if (authorityKey != appliedAuthorityKey) {
            actionIdempotencyKeys.clear()
            _state.value = _state.value.copy(
                activePage = _state.value.activePage?.copy(loading = true, snapshot = null),
                indicators = emptyList(),
                indicatorEtag = null,
                indicatorRefreshAfter = null,
            )
            refreshNow()
            return
        }
        val key = actionKey(authorityKey, assignment, item, actionId) ?: return
        val expectedInstallationGeneration = assignment.installationGeneration ?: return
        val expectedPackageRevisionRef = assignment.packageRevisionRef ?: return
        if (key in _state.value.actionsInFlight) return
        _state.value = _state.value.copy(
            actionsInFlight = _state.value.actionsInFlight + key,
            actionNotice = null,
        )
        viewModelScope.launch {
            // Retain one key after an ambiguous transport failure so a retry
            // recovers the same durable run instead of launching it twice.
            if (key !in actionIdempotencyKeys && actionIdempotencyKeys.size >= MaximumRetainedActionKeys) {
                actionIdempotencyKeys.remove(actionIdempotencyKeys.keys.first())
            }
            val idempotencyKey = actionIdempotencyKeys.getOrPut(key) {
                "android-widget-${UUID.randomUUID()}"
            }
            val result = captured {
                repository.launchEmptyAction(
                    item.installationId,
                    actionId,
                    idempotencyKey,
                    expectedInstallationGeneration = expectedInstallationGeneration,
                    expectedPackageRevisionRef = expectedPackageRevisionRef,
                    expectedAuthorityKey = authorityKey,
                )
            }
            if (result.isSuccess) actionIdempotencyKeys.remove(key)
            if (repository.authorityKey() != authorityKey || appliedAuthorityKey != authorityKey) return@launch
            if (!isCurrentActionAuthority(key, authorityKey, actionId)) {
                _state.value = _state.value.copy(actionsInFlight = _state.value.actionsInFlight - key)
                return@launch
            }
            if ((result.exceptionOrNull() as? AppSurfacingApiException)?.status == 409) {
                _state.value = _state.value.copy(actionsInFlight = _state.value.actionsInFlight - key)
                refreshNow()
                return@launch
            }
            val message = result.fold(
                onSuccess = { "${actionId.replace('_', ' ')} started · ${it.runHandle.runRef}" },
                onFailure = { it.message ?: "The app action could not be started." },
            )
            _state.value = _state.value.copy(
                actionsInFlight = _state.value.actionsInFlight - key,
                actionNotice = AppActionNotice(
                    installationId = item.installationId,
                    actionId = actionId,
                    message = message,
                    succeeded = result.isSuccess,
                ),
            )
        }
    }

    fun actionInFlight(
        assignment: AppResolvedSlotAssignment,
        item: AppWidgetRenderItem,
        actionId: String,
    ): Boolean {
        val authorityKey = repository.authorityKey()
        if (authorityKey != appliedAuthorityKey) return false
        return actionKey(authorityKey, assignment, item, actionId) in _state.value.actionsInFlight
    }

    /**
     * Opens the bounded picker for one empty region.
     *
     * The read is also fence acquisition, so it is refused while any other
     * slot change is unresolved: acquiring a newer fence would strand the
     * older one and lose the owner's in-flight change.
     */
    fun openSlotPicker(region: String) {
        if (_state.value.slotEditor.busy) return
        val context = editorContext(region) ?: return
        slotEditorJob?.cancel()
        pickerCursors = mutableSetOf()
        _state.value = _state.value.copy(
            slotEditor = AppSlotEditorUiState(region = region, loading = true),
        )
        slotEditorJob = viewModelScope.launch {
            val result = captured {
                fetchSettingsForSlot(
                    context.slotId,
                    AppSurfacingLimits.DefaultSlotPickerLimit,
                    context.authorityKey,
                )
            }
            if (!editorIsCurrent(context)) return@launch
            result.fold(
                onSuccess = { settings ->
                    if (!settings.agreesWith(context.slotId, context.assignment)) {
                        failSlotEditor(region, "The slot changed while the picker was opening. Open it again.")
                        return@fold
                    }
                    pickerCursors = mutableSetOf<String>().apply {
                        settings.nextPickerCursor?.let(::add)
                    }
                    _state.value = _state.value.copy(
                        slotEditor = AppSlotEditorUiState(region = region, settings = settings),
                    )
                },
                onFailure = { error ->
                    failSlotEditor(region, error.slotEditorMessage("The widget picker could not be loaded."))
                },
            )
        }
    }

    /** Appends one more bounded page of picker rows under the same inventory. */
    fun loadMorePickerCandidates() {
        val editor = _state.value.slotEditor
        val region = editor.region ?: return
        val previous = editor.settings ?: return
        val cursor = previous.nextPickerCursor ?: return
        if (editor.busy || !previous.pickerTruncated) return
        if (previous.picker.size >= AppSurfacingLimits.MaximumAccumulatedPickerCandidates) return
        val context = editorContext(region) ?: return
        slotEditorJob?.cancel()
        _state.value = _state.value.copy(slotEditor = editor.copy(loading = true, error = null))
        slotEditorJob = viewModelScope.launch {
            val result = captured {
                repository.fetchSlotSettings(
                    AppSlotSettingsQuery(
                        assignmentLimit = 1,
                        pickerLimit = AppSurfacingLimits.DefaultSlotPickerLimit,
                        pickerCursor = cursor,
                    ),
                    context.authorityKey,
                )
            }
            if (!editorIsCurrent(context) || _state.value.slotEditor.settings != previous) return@launch
            result.fold(
                onSuccess = { next ->
                    val merged = previous.mergedPicker(next, cursor, pickerCursors)
                    if (merged == null) {
                        // A failed or inconsistent continuation may still have
                        // advanced the fence, so the prior head is discarded
                        // rather than offered as a stale write.
                        failSlotEditor(region, "The widget picker changed while it was read. Open it again.")
                        return@fold
                    }
                    next.nextPickerCursor?.let(pickerCursors::add)
                    _state.value = _state.value.copy(
                        slotEditor = _state.value.slotEditor.copy(settings = merged, loading = false),
                    )
                },
                onFailure = { error ->
                    failSlotEditor(region, error.slotEditorMessage("More widgets could not be loaded. Open the picker again."))
                },
            )
        }
    }

    /** Closes the picker list. A retained ambiguous write survives it. */
    fun closeSlotPicker() {
        val editor = _state.value.slotEditor
        if (editor.mutating) return
        slotEditorJob?.cancel()
        slotEditorJob = null
        pickerCursors = mutableSetOf()
        _state.value = _state.value.copy(
            slotEditor = if (editor.retryable) {
                AppSlotEditorUiState(region = editor.region, retryable = true, error = editor.error)
            } else {
                AppSlotEditorUiState()
            },
        )
    }

    /** Pins the owner's choice for the open region, bound to the exact row. */
    fun assignPickerCandidate(candidate: AppSlotPickerCandidate) {
        val editor = _state.value.slotEditor
        val region = editor.region ?: return
        val settings = editor.settings ?: return
        if (editor.busy) return
        val context = editorContext(region) ?: return
        val current = settings.picker.firstOrNull { it.candidateKey == candidate.candidateKey }
        if (current == null) {
            failSlotEditor(region, "That widget is no longer in the picker snapshot.")
            return
        }
        if (!settings.agreesWith(context.slotId, context.assignment)) {
            failSlotEditor(region, "The slot changed elsewhere. Open the picker again.")
            refreshNow()
            return
        }
        slotEditorJob?.cancel()
        slotEditorJob = viewModelScope.launch {
            runSlotMutation(
                context,
                AppSlotAssignmentCommand.Assign(context.slotId, current),
                settings.head,
            )
        }
    }

    /** Removes the widget from a region for this owner only. */
    fun optOutSlot(region: String) {
        mutateSlotWithFreshHead(
            region,
            permitted = { assignment -> assignment.occupied },
        ) { slotId -> AppSlotAssignmentCommand.OptOut(slotId) }
    }

    /**
     * Drops this owner's customization so the workspace default owns the slot
     * again. Without it an opt-out could only ever be replaced, never undone.
     */
    fun restoreWorkspaceDefaultForSlot(region: String) {
        mutateSlotWithFreshHead(
            region,
            permitted = { assignment -> assignment.optedOut || assignment.source == "user" },
        ) { slotId -> AppSlotAssignmentCommand.RestoreWorkspaceDefault(slotId) }
    }

    /** Replays the exact retained write, never a newly minted one. */
    fun retryPendingSlotMutation() {
        val pending = pendingSlotMutation ?: return
        if (_state.value.slotEditor.mutating) return
        if (!pendingIsCurrent(pending)) return
        slotEditorJob?.cancel()
        slotEditorJob = viewModelScope.launch { applyPendingSlotMutation(pending) }
    }

    private fun mutateSlotWithFreshHead(
        region: String,
        permitted: (AppResolvedSlotAssignment) -> Boolean,
        command: (AppSlotId) -> AppSlotAssignmentCommand,
    ) {
        if (_state.value.slotEditor.busy) return
        val context = editorContext(region) ?: return
        // The command must be meaningful for the state the owner is looking at;
        // a control that cannot prove its precondition does nothing at all.
        if (!permitted(context.assignment)) return
        slotEditorJob?.cancel()
        _state.value = _state.value.copy(
            slotEditor = AppSlotEditorUiState(region = region, loading = true),
        )
        slotEditorJob = viewModelScope.launch {
            // A change without an open picker still needs a fence, and the head
            // it writes under must be read after the card it acts on.
            val result = captured { fetchSettingsForSlot(context.slotId, 1, context.authorityKey) }
            if (!editorIsCurrent(context)) return@launch
            result.fold(
                onSuccess = { settings ->
                    if (!settings.agreesWith(context.slotId, context.assignment)) {
                        failSlotEditor(region, "The slot changed elsewhere. Refresh and try again.")
                        refreshNow()
                        return@fold
                    }
                    runSlotMutation(context, command(context.slotId), settings.head)
                },
                onFailure = { error ->
                    failSlotEditor(region, error.slotEditorMessage("The slot change could not be prepared."))
                },
            )
        }
    }

    private suspend fun runSlotMutation(
        context: SlotEditorContext,
        command: AppSlotAssignmentCommand,
        head: AppSlotWriteHead,
    ) {
        val pending = PendingSlotMutation(
            authorityKey = context.authorityKey,
            pageKey = context.pageKey,
            region = context.region,
            request = AppSlotAssignmentWriteRequest(
                expectedRevision = head.revision,
                writeFence = head.fence,
                mutationId = newAppSlotMutationId(),
                command = command,
            ),
        )
        pendingSlotMutation = pending
        applyPendingSlotMutation(pending)
    }

    private suspend fun applyPendingSlotMutation(pending: PendingSlotMutation) {
        _state.value = _state.value.copy(
            slotEditor = _state.value.slotEditor.copy(
                region = pending.region,
                loading = false,
                mutating = true,
                retryable = false,
                error = null,
            ),
        )
        val result = captured { repository.mutateSlotAssignment(pending.request, pending.authorityKey) }
        if (!pendingIsCurrent(pending)) return
        result.fold(
            onSuccess = {
                pendingSlotMutation = null
                pickerCursors = mutableSetOf()
                _state.value = _state.value.copy(slotEditor = AppSlotEditorUiState())
                refreshNow()
            },
            onFailure = { error ->
                val status = (error as? AppSurfacingApiException)?.status ?: 0
                if (status in 400..499 && status != 408 && status != 425 && status != 429) {
                    // The host refused this exact write, so replaying it cannot
                    // succeed. Drop the fence instead of offering a dead retry.
                    pendingSlotMutation = null
                    pickerCursors = mutableSetOf()
                    _state.value = _state.value.copy(
                        slotEditor = AppSlotEditorUiState(
                            region = pending.region,
                            error = "The slot changed elsewhere. Open the picker again.",
                        ),
                    )
                    refreshNow()
                } else {
                    // The outcome is unknown. Keep the exact fence and mutation
                    // id so a retry resolves this write rather than adding one.
                    pendingSlotMutation = pending
                    _state.value = _state.value.copy(
                        slotEditor = _state.value.slotEditor.copy(
                            mutating = false,
                            retryable = true,
                            error = "The slot change could not be confirmed. Retry the same change.",
                        ),
                    )
                }
            },
        )
    }

    /**
     * Reads a settings page guaranteed to describe the slot being edited.
     *
     * The host pages assignments, and the slot the owner is acting on may sit
     * past the first page. One bounded continuation is followed under an
     * unchanged revision and inventory; anything else is a concurrent edit.
     */
    private suspend fun fetchSettingsForSlot(
        slotId: AppSlotId,
        pickerLimit: Int,
        authorityKey: String,
    ): AppSlotSettingsPage {
        val first = repository.fetchSlotSettings(
            AppSlotSettingsQuery(
                assignmentLimit = AppSurfacingLimits.MaximumSlotAssignments,
                pickerLimit = pickerLimit,
            ),
            authorityKey,
        )
        if (first.assignments.containsKey(slotId.value) || !first.assignmentsTruncated) return first
        val cursor = first.nextAssignmentCursor
            ?: throw AppSurfacingApiException("The slot settings pagination response was invalid.")
        val second = repository.fetchSlotSettings(
            AppSlotSettingsQuery(
                assignmentLimit = AppSurfacingLimits.MaximumSlotAssignments,
                assignmentCursor = cursor,
                pickerLimit = pickerLimit,
            ),
            authorityKey,
        )
        if (second.head.revision != first.head.revision || second.head.fence <= first.head.fence ||
            second.inventoryRevision != first.inventoryRevision ||
            second.assignmentsTruncated || second.nextAssignmentCursor != null) {
            throw AppSurfacingApiException("The slot settings changed while they were read. Try again.")
        }
        val assignments = first.assignments + second.assignments
        if (assignments.size != first.assignments.size + second.assignments.size) {
            throw AppSurfacingApiException("The slot settings pagination response repeated a slot.")
        }
        return second.copy(
            assignments = assignments,
            assignmentsTruncated = false,
            nextAssignmentCursor = null,
        )
    }

    /**
     * The editor may only act on a slot the owner can currently see.
     *
     * A card hidden behind a revalidation is not evidence of anything, so a
     * loading page yields no context and every control stays closed.
     */
    private fun editorContext(region: String): SlotEditorContext? {
        val authorityKey = repository.authorityKey()
        if (authorityKey != appliedAuthorityKey) {
            invalidateChangedAuthority()
            return null
        }
        val active = _state.value.activePage ?: return null
        if (active.loading) return null
        val slotId = active.regions.firstOrNull { it.region == region }?.slotId ?: return null
        val assignment = active.snapshot?.assignments?.get(slotId) ?: return null
        return SlotEditorContext(
            authorityKey = authorityKey,
            pageKey = pageCacheKey(active.page, active.regions),
            region = region,
            slotId = slotId,
            assignment = assignment,
        )
    }

    private fun editorIsCurrent(context: SlotEditorContext): Boolean {
        if (repository.authorityKey() != context.authorityKey || appliedAuthorityKey != context.authorityKey) {
            invalidateChangedAuthority()
            return false
        }
        return activePageKey() == context.pageKey && _state.value.slotEditor.region == context.region
    }

    private fun pendingIsCurrent(pending: PendingSlotMutation): Boolean {
        if (repository.authorityKey() != pending.authorityKey || appliedAuthorityKey != pending.authorityKey) {
            pendingSlotMutation = null
            invalidateChangedAuthority()
            return false
        }
        if (activePageKey() != pending.pageKey) {
            // The page that acquired this fence is gone; it cannot be replayed
            // against a different layout.
            pendingSlotMutation = null
            _state.value = _state.value.copy(slotEditor = AppSlotEditorUiState())
            return false
        }
        return true
    }

    private fun activePageKey(): String? =
        _state.value.activePage?.let { pageCacheKey(it.page, it.regions) }

    /**
     * The page to revalidate under a replacement authority.
     *
     * An entity route opened over the shell belongs to the authority that
     * resolved it, so a reset retreats to the page it was opened over rather
     * than stranding the owner on a route the new authority may not resolve.
     */
    private fun retreatedPage(): AppWidgetPageUiState? {
        val retreat = entityBacktrack
        entityBacktrack = null
        return retreat ?: _state.value.activePage
    }

    private fun failSlotEditor(region: String, message: String) {
        // Whatever head this editor held is now untrustworthy: keep only the
        // message and let the owner start again from a fresh read.
        _state.value = _state.value.copy(
            slotEditor = AppSlotEditorUiState(region = region, error = message),
        )
    }

    private fun resetSlotEditor() {
        slotEditorJob?.cancel()
        slotEditorJob = null
        pendingSlotMutation = null
        pickerCursors = mutableSetOf()
        if (_state.value.slotEditor != AppSlotEditorUiState()) {
            _state.value = _state.value.copy(slotEditor = AppSlotEditorUiState())
        }
    }

    private data class SlotEditorContext(
        val authorityKey: String,
        val pageKey: String,
        val region: String,
        val slotId: AppSlotId,
        val assignment: AppResolvedSlotAssignment,
    )

    private data class PendingSlotMutation(
        val authorityKey: String,
        val pageKey: String,
        val region: String,
        val request: AppSlotAssignmentWriteRequest,
    )

    private suspend fun refresh(force: Boolean = false) = refreshMutex.withLock {
        if (!foreground) return@withLock
        var before = _state.value
        val authorityKey = repository.authorityKey()
        if (authorityKey != appliedAuthorityKey) {
            // A replacement pairing, owner scope, or host must never inherit
            // conditional bodies from the previous authority, even if package
            // identifiers happen to match.
            resetSlotEditor()
            before = AppSurfacingUiState(
                activePage = retreatedPage()?.copy(
                    loading = true,
                    snapshot = null,
                    error = null,
                ),
            )
            _state.value = before
            appliedAuthorityKey = authorityKey
            actionIdempotencyKeys.clear()
        }
        val now = Instant.now()
        val page = before.activePage?.takeIf { active ->
            force || active.snapshot?.refreshAfter?.isAfter(now) != true
        }
        val indicatorsDue = force || before.indicatorRefreshAfter?.isAfter(now) != true
        if (page == null && !indicatorsDue) return@withLock
        if (page != null) {
            val current = _state.value.activePage
            if (current?.page == page.page && current.regions == page.regions) {
                _state.value = _state.value.copy(activePage = current.copy(loading = true))
            }
        }
        if (indicatorsDue) {
            _state.value = _state.value.copy(
                indicators = _state.value.indicators.filter { it.isLiveAt(now) },
            )
        }
        coroutineScope {
            val pageRefresh = page?.let { active ->
                async {
                    captured {
                        repository.refreshPage(
                            regions = active.regions,
                            previousEtag = active.snapshot?.etag,
                            previousTargetFingerprint = active.snapshot?.targetFingerprint,
                            expectedAuthorityKey = authorityKey,
                        )
                    }
                }
            }
            val indicatorRefresh = if (indicatorsDue) {
                async { captured { repository.refreshIndicators(before.indicatorEtag, authorityKey) } }
            } else {
                null
            }

            val pageResult = pageRefresh?.await()
            val indicatorResult = indicatorRefresh?.await()
            if (!foreground) return@coroutineScope
            if (repository.authorityKey() != authorityKey || appliedAuthorityKey != authorityKey) {
                invalidateChangedAuthority()
                return@coroutineScope
            }
            if (pageResult != null) applyPageRefresh(page, pageResult)
            indicatorResult?.let(::applyIndicatorRefresh)
        }
    }

    private fun applyPageRefresh(expected: AppWidgetPageUiState, result: Result<AppWidgetPageRefresh>) {
        if (!foreground) return
        val current = _state.value.activePage
        if (current?.page != expected.page || current.regions != expected.regions) return
        result.fold(
            onSuccess = { refresh ->
                val snapshot = when (refresh) {
                    is AppWidgetPageRefresh.Modified -> refresh.snapshot
                    is AppWidgetPageRefresh.NotModified -> current.snapshot?.renewedNotModified(
                        refresh,
                        Instant.now(),
                    )
                }
                val snapshots = snapshot?.let {
                    storePageSnapshot(_state.value.pageSnapshots, pageCacheKey(current.page, current.regions), it)
                } ?: _state.value.pageSnapshots
                _state.value = _state.value.copy(
                    activePage = current.copy(loading = false, snapshot = snapshot, error = null),
                    pageSnapshots = snapshots,
                )
            },
            onFailure = { error ->
                // A transient failure keeps the last confirmed bodies on
                // screen within their staleness bound (and retries soon);
                // past it only the slot shape survives, so an assigned slot
                // shows the closed unavailable state.
                val retained = current.snapshot?.retainedAfterFailure(Instant.now())
                val snapshots = retained?.let {
                    storePageSnapshot(_state.value.pageSnapshots, pageCacheKey(current.page, current.regions), it)
                } ?: _state.value.pageSnapshots
                _state.value = _state.value.copy(
                    activePage = current.copy(
                        loading = false,
                        snapshot = retained,
                        error = error.message ?: "App widgets are unavailable.",
                    ),
                    pageSnapshots = snapshots,
                )
            },
        )
    }

    private fun applyIndicatorRefresh(result: Result<AppIndicatorRefresh>) {
        result.fold(
            onSuccess = { refresh ->
                when (refresh) {
                    is AppIndicatorRefresh.Modified -> _state.value = _state.value.copy(
                        indicators = refresh.response.indicators,
                        indicatorEtag = refresh.etag,
                        indicatorRefreshAfter = indicatorRefreshDeadline(Instant.now(), refresh.response.indicators),
                    )
                    is AppIndicatorRefresh.NotModified -> {
                        val now = Instant.now()
                        val live = _state.value.indicators.filter { it.isLiveAt(now) }
                        _state.value = _state.value.copy(
                            indicators = live,
                            indicatorEtag = refresh.etag,
                            indicatorRefreshAfter = indicatorRefreshDeadline(now, live),
                        )
                    }
                }
            },
            onFailure = {
                // Indicator failures hide immediately; stale ambient state is
                // worse than no chip.
                _state.value = _state.value.copy(
                    indicators = emptyList(),
                    indicatorEtag = null,
                    indicatorRefreshAfter = Instant.now().plusMillis(ForegroundRefreshMillis),
                )
            },
        )
    }

    private suspend fun <T> captured(block: suspend () -> T): Result<T> = try {
        Result.success(block())
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Throwable) {
        Result.failure(error)
    }

    private fun scheduleNextPoll() {
        pollJob?.cancel()
        val delayMillis = nextForegroundDelay(
            now = Instant.now(),
            refreshAfter = _state.value.activePage?.snapshot?.refreshAfter,
            indicatorRefreshAfter = _state.value.indicatorRefreshAfter,
        )
        pollJob = viewModelScope.launch {
            delay(delayMillis)
            refresh()
            if (foreground) {
                pollJob = null
                scheduleNextPoll()
            }
        }
    }

    private fun markActivePageRevalidating() {
        val active = _state.value.activePage ?: return
        _state.value = _state.value.copy(
            activePage = active.copy(loading = true, error = null),
            actionNotice = null,
        )
    }

    private fun isCurrentActionAuthority(key: String, authorityKey: String, actionId: String): Boolean {
        val active = _state.value.activePage ?: return false
        if (active.loading) return false
        val snapshot = active.snapshot ?: return false
        return snapshot.assignments.any { (slotId, assignment) ->
            val item = snapshot.widgetsBySlot[slotId] ?: return@any false
            actionKey(authorityKey, assignment, item, actionId) == key
        }
    }

    private fun invalidateChangedAuthority() {
        val replacement = repository.authorityKey()
        resetSlotEditor()
        _state.value = AppSurfacingUiState(
            activePage = retreatedPage()?.copy(loading = true, snapshot = null, error = null),
        )
        appliedAuthorityKey = replacement
        actionIdempotencyKeys.clear()
        if (foreground) viewModelScope.launch {
            refresh(force = true)
            if (foreground) scheduleNextPoll()
        }
    }

    override fun onCleared() {
        onBackground()
        repository.close()
        super.onCleared()
    }

    companion object {
        const val ForegroundRefreshMillis = 30_000L
        const val MinimumRefreshMillis = 250L

        /** Staleness bound when no widget declares `max_staleness_seconds`. */
        const val DefaultMaxStalenessMillis = 5 * 60_000L

        /** Retry cadence while stale bodies are being kept through a failure. */
        const val FailureRetryMillis = 10_000L
        internal fun actionKey(
            authorityKey: String,
            assignment: AppResolvedSlotAssignment,
            item: AppWidgetRenderItem,
            actionId: String,
        ): String? {
            val generation = item.installationGeneration ?: return null
            val target = assignment.target ?: return null
            if (target.installationId != item.installationId || target.widgetId != item.widgetId ||
                assignment.installationGeneration != generation || assignment.packageRevisionRef == null ||
                assignment.packageContentDigest == null || item.state != "ready" ||
                item.model?.actions?.none { it.actionId == actionId } != false) return null
            return listOf(
                authorityKey,
                item.installationId,
                item.widgetId,
                generation.toString(),
                assignment.packageRevisionRef,
                assignment.packageContentDigest,
                item.revision,
                actionId,
            ).joinToString("\u0000")
        }

        internal fun nextForegroundDelay(
            now: Instant,
            refreshAfter: Instant?,
            indicatorRefreshAfter: Instant? = null,
        ): Long {
            val deadline = listOfNotNull(refreshAfter, indicatorRefreshAfter).minOrNull()
            val dueDelay = deadline?.let { due ->
                java.time.Duration.between(now, due).toMillis().coerceAtLeast(MinimumRefreshMillis)
            }
            return minOf(ForegroundRefreshMillis, dueDelay ?: ForegroundRefreshMillis)
        }

        internal fun indicatorRefreshDeadline(
            now: Instant,
            indicators: List<AppMaterializedIndicator>,
        ): Instant = minOf(
            now.plusMillis(ForegroundRefreshMillis),
            indicators.minOfOrNull { Instant.parse(it.expiresAt) } ?: now.plusMillis(ForegroundRefreshMillis),
        )

        private const val MaximumCachedPages = 8
        private const val MaximumRetainedActionKeys = 128

        internal fun pageCacheKey(page: String, regions: List<AppSlotRegionSpec>): String =
            page + regions.joinToString(separator = "\u0000", prefix = "\u0001") { it.slotId.value }

        private fun storePageSnapshot(
            existing: Map<String, AppWidgetPageSnapshot>,
            key: String,
            snapshot: AppWidgetPageSnapshot,
        ): Map<String, AppWidgetPageSnapshot> {
            val next = LinkedHashMap(existing)
            next.remove(key)
            while (next.size >= MaximumCachedPages) next.remove(next.keys.first())
            next[key] = snapshot
            return next
        }
    }
}

/**
 * How long this snapshot's bodies may outlive their last confirmation: the
 * tightest `max_staleness_seconds` any widget declares, else five minutes.
 */
internal fun AppWidgetPageSnapshot.maxStalenessMillis(): Long =
    widgetsBySlot.values.mapNotNull(AppWidgetRenderItem::maxStalenessSeconds).minOrNull()
        ?.let { it * 1_000L }
        ?: AppSurfacingViewModel.DefaultMaxStalenessMillis

/**
 * A 304 says the cached bodies are current until the new deadline, so every
 * cached item's own deadline is renewed with the batch's, not just the batch.
 */
internal fun AppWidgetPageSnapshot.renewedNotModified(
    refresh: AppWidgetPageRefresh.NotModified,
    now: Instant,
): AppWidgetPageSnapshot {
    val deadline = refresh.refreshAfter.toString()
    return copy(
        assignments = refresh.assignments,
        targetFingerprint = refresh.targetFingerprint,
        etag = refresh.etag,
        refreshAfter = refresh.refreshAfter,
        confirmedAt = now,
        widgetsBySlot = widgetsBySlot.mapValues { (_, item) ->
            if (Instant.parse(item.refreshAfter).isBefore(refresh.refreshAfter)) {
                item.copy(refreshAfter = deadline)
            } else {
                item
            }
        },
    )
}

/**
 * The snapshot to keep after a failed refresh.
 *
 * Within the staleness bound the last confirmed bodies, ETag and fingerprint
 * stay (so a recovered host can answer 304) and the next attempt is brought
 * forward. Past it — or with nothing ever confirmed — only the slot shape is
 * kept, which draws the closed unavailable state.
 */
internal fun AppWidgetPageSnapshot.retainedAfterFailure(now: Instant): AppWidgetPageSnapshot {
    val confirmed = confirmedAt
    val expiresAt = confirmed?.plusMillis(maxStalenessMillis())
    if (confirmed == null || widgetsBySlot.isEmpty() || expiresAt == null || !now.isBefore(expiresAt)) {
        return copy(widgetsBySlot = emptyMap(), etag = null, refreshAfter = null, confirmedAt = null)
    }
    val retry = minOf(now.plusMillis(AppSurfacingViewModel.FailureRetryMillis), expiresAt)
    return copy(refreshAfter = acceptedWidgetRefreshDeadline(now, retry))
}

/**
 * True when a settings snapshot describes the same slot authority the visible
 * card was drawn from.
 *
 * The host omits a slot nobody has touched, so an absent entry is the empty
 * assignment rather than a mismatch — and every other difference, including
 * ones this client never renders, refuses the write.
 */
internal fun AppSlotSettingsPage.agreesWith(
    slotId: AppSlotId,
    resolved: AppResolvedSlotAssignment,
): Boolean {
    val recorded = assignments[slotId.value]
        ?: AppResolvedSlotAssignmentWire(slotId.value, pinnedSystemDefault = false, optedOut = false)
    return recorded == resolved.wire
}

/**
 * Appends one continuation page of picker rows, or null when the continuation
 * cannot be trusted to belong to the same read.
 *
 * A cursor that repeats itself or one already seen is a loop, and a truncated
 * page carrying no rows can never terminate — both are refused rather than
 * paged forever.
 */
internal fun AppSlotSettingsPage.mergedPicker(
    next: AppSlotSettingsPage,
    requestedCursor: String,
    seenCursors: Set<String>,
): AppSlotSettingsPage? {
    if (next.head.revision != head.revision || next.head.fence <= head.fence) return null
    if (next.inventoryRevision != inventoryRevision) return null
    val combined = picker + next.picker
    if (combined.size > AppSurfacingLimits.MaximumAccumulatedPickerCandidates) return null
    if (combined.map(AppSlotPickerCandidate::targetKey).toSet().size != combined.size) return null
    if (next.pickerTruncated) {
        val cursor = next.nextPickerCursor ?: return null
        if (next.picker.isEmpty() ||
            combined.size >= AppSurfacingLimits.MaximumAccumulatedPickerCandidates ||
            cursor == requestedCursor || cursor in seenCursors) {
            return null
        }
    }
    return copy(
        head = next.head,
        inventoryRevision = next.inventoryRevision,
        picker = combined,
        nextPickerCursor = next.nextPickerCursor,
        pickerTruncated = next.pickerTruncated,
    )
}

/** Picker rows suggested for this slot come first, then title order. */
fun List<AppSlotPickerCandidate>.orderedForSlot(slotId: AppSlotId): List<AppSlotPickerCandidate> =
    sortedWith(
        compareBy(
            { if (slotId.value in it.suggestedSlotIds) 0 else 1 },
            { it.title.lowercase() },
            { it.candidateKey },
        ),
    )

private fun Throwable.slotEditorMessage(fallback: String): String =
    message?.takeIf(String::isNotBlank) ?: fallback
