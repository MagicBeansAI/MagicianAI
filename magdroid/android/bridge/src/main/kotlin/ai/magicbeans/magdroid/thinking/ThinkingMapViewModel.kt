package ai.magicbeans.magdroid.thinking

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.toFailure
import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class ThinkingMapUiState(
    val maps: List<ThinkingMapSummary> = emptyList(),
    val query: String = "",
    val loading: Boolean = false,
    /** A failed action on a map — saving, deleting, promoting. */
    val error: String? = null,
    /**
     * A failed read of the map list itself, classified.
     *
     * Distinct from [error] because it replaces the list rather than annotating
     * it, and "No maps yet." must not be said when the read never landed.
     */
    val failure: Failure? = null,
    /** The map being read, if one is open. */
    val open: ThinkingMap? = null,
    val opening: Boolean = false,
    /** The summary being opened, so the header has a title while it loads. */
    val openingId: String? = null,
    /** Which node Focus is on. Null follows the map's own active node. */
    val focusedNodeId: String? = null,
    /** Focus walks the map; Outline reads it whole. Both, as iOS has both. */
    val reading: ReadingMode = ReadingMode.Focus,
    /** A change is in flight; the map is not editable while it is. */
    val saving: Boolean = false,
    val saveError: String? = null,
    /** A historical projection being browsed, with the live map held aside. */
    val replaying: ThinkingMap? = null,
    val replayLive: ThinkingMap? = null,
    val replaySequence: Long? = null,
    /** What promoting a node would do, before it is confirmed. */
    val promotePreview: String? = null,
    /** What the facilitator is doing right now. */
    val progress: ThinkingProgress = ThinkingProgress.Idle,
    /**
     * How many thoughts the facilitator said it was reading, from the
     * `preparing` stage event. Only meaningful beside [progress]; cleared when
     * the run settles.
     */
    val progressNodeCount: Int? = null,
    /** Why it produced nothing, when it did not. */
    val fallback: ThinkingFallback? = null,
) {
    /**
     * Pinned first, then most recently opened.
     *
     * Archived maps are left out: they are kept for history, and a library that
     * shows them beside live ones makes the live ones harder to find.
     */
    val visible: List<ThinkingMapSummary>
        get() = maps
            .filterNot { it.isArchived }
            .filter { it.matches(query) }
            .sortedWith(
                // The canonical summary has no pin; most recently updated is
                // the order the server can actually support.
                compareByDescending { it.updatedAt },
            )

    /** The map on screen: a replay when browsing history, else the live one. */
    val showing: ThinkingMap? get() = replaying ?: open

    val isReplaying: Boolean get() = replaying != null

    val outline: List<OutlineRow> get() = showing?.let { outline(it) }.orEmpty()

    val graph: GraphLayout
        get() = showing?.let { graphLayout(it) } ?: GraphLayout(emptyList(), emptyList())

    val focus: ThinkingFocus
        get() = showing?.let { focus(it, focusedNodeId) }
            ?: ThinkingFocus(emptyList(), null, emptyList(), emptyList())
}

/** How the open map is being read. */
enum class ReadingMode(val label: String) {
    /** iOS calls this Canvas in the picker and `map` in the model. */
    Canvas("Canvas"),
    Focus("Focus"),
    Outline("Outline"),
}

/**
 * The map library, and one map open for reading.
 *
 * Opening refetches rather than using the listed copy: the list carries whole
 * snapshots, but a map somebody is actively thinking in changes, and reading a
 * stale graph is the one thing this surface must not do.
 */
class ThinkingMapViewModel(app: Application) : AndroidViewModel(app) {

    private val repository = ThinkingMapRepository(app)
    private val realtime = ThinkingMapRealtime(app)
    private val _state = MutableStateFlow(ThinkingMapUiState())

    /** Held so a failed interpretation can be tried again as it was. */
    private var lastInterpret: Pair<String, FrontierIntent>? = null

    /**
     * The utterance id of the interpretation in flight, minted here and sent
     * with the request. Progress events on the bus carry the same id, which is
     * the only thing that separates *this* run's narration from an ambient
     * auto-map or another device thinking on the same board.
     */
    private var inFlightUtteranceId: String? = null
    val state: StateFlow<ThinkingMapUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun search(query: String) {
        _state.value = _state.value.copy(query = query)
    }

    fun refresh() {
        if (_state.value.loading) return
        _state.value = _state.value.copy(loading = true, error = null, failure = null)
        viewModelScope.launch {
            runCatching { repository.list() }
                .onSuccess {
                    _state.value = _state.value.copy(maps = it, loading = false, failure = null)
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        loading = false,
                        failure = it.toFailure(getApplication(), "your maps"),
                    )
                }
        }
    }

    fun open(map: ThinkingMapSummary) {
        // Shown from the listed copy at once so the screen is never blank, then
        // replaced by the fetched one. A map that takes a moment to load should
        // still read immediately.
        // The cursor resets to the map's own active node rather than keeping
        // where the last map was read: they are different maps.
        //
        // The facilitator state resets too: the strip, its count, the fallback
        // and the last save error all describe the PREVIOUS map's runs, and a
        // fresh board opening under "Exploring…" — or under an old
        // FACILITATOR UNAVAILABLE banner — reads as this map failing.
        _state.value = _state.value.copy(
            open = null, opening = true, error = null, focusedNodeId = null,
            openingId = map.id,
            progress = ThinkingProgress.Idle,
            progressNodeCount = null,
            fallback = null,
            saveError = null,
        )
        viewModelScope.launch {
            runCatching { repository.map(map.id) }
                .onSuccess { fresh ->
                    // Only if the reader has not moved on in the meantime.
                    if (_state.value.openingId == fresh.id) {
                        _state.value = _state.value.copy(open = fresh, opening = false)
                    }
                }
                .onFailure { problem ->
                    // A summary carries no graph, so there is nothing to fall
                    // back to — unlike the old shape, where a listed copy could
                    // stand in. Say so rather than showing an empty map.
                    _state.value = _state.value.copy(
                        opening = false,
                        openingId = null,
                        error = problem.message ?: "Could not open that map.",
                    )
                }
        }
        listen(map.id)
    }

    /**
     * Follow the open map on the realtime bus.
     *
     * This client read `/realtime/ws` on four other surfaces and not on the one
     * a background agent actually writes to, so a map that was being worked on
     * sat still. The notice carries no payload: it says the map moved and this
     * re-reads it, which keeps one authoritative fetch path.
     */
    private fun listen(mapId: String) {
        realtime.follow(
            viewModelScope,
            mapId,
            onNotice = { notice ->
                val current = _state.value.open
                // Gated on revision because our own writes come back as notices —
                // every captured thought, and every focus broadcast. Re-fetching on
                // those would mean a request per tap, answering with what we just
                // sent.
                if (current == null || current.id != notice.mapId) return@follow
                if (notice.revision <= current.revision) return@follow
                if (_state.value.saving) return@follow
                viewModelScope.launch {
                    runCatching { repository.map(notice.mapId) }.onSuccess { fresh ->
                        // Not while replaying: the reader is looking at history on
                        // purpose, and swapping it for the live map underneath them
                        // would lose their place.
                        if (_state.value.open?.id == fresh.id && _state.value.replaying == null) {
                            _state.value = _state.value.copy(open = fresh)
                        }
                    }
                }
            },
            onProgress = { event ->
                // Only the run this screen started, and never the terminal
                // idle: settling is the HTTP response's job — it also owns the
                // failure classification — and a realtime idle can outrun the
                // response body. Letting it clear the strip early would show
                // "Ready" over a request still being applied.
                if (!interpretProgressApplies(event, _state.value.open?.id, inFlightUtteranceId)) {
                    return@follow
                }
                _state.value = _state.value.copy(
                    progress = event.stage ?: return@follow,
                    progressNodeCount = event.nodeCount ?: _state.value.progressNodeCount,
                )
            },
        )
    }

    fun close() {
        _state.value = _state.value.copy(
            open = null, opening = false, openingId = null, focusedNodeId = null,
        )
        viewModelScope.launch { realtime.stop() }
    }


    /**
     * Move Focus to another node, and say so.
     *
     * The move is local first so the UI never waits on a round trip, then
     * broadcast: a map can be watched by more than the person driving it, and a
     * focus that stayed on this handset would leave everyone else reading a
     * different part of the same thought. Fire and forget — a failed broadcast
     * costs the others a stale highlight, not this owner their navigation.
     */
    fun focusNode(nodeId: String) {
        _state.value = _state.value.copy(focusedNodeId = nodeId)
        val map = _state.value.open ?: return
        viewModelScope.launch {
            // Deliberately not through `apply`: that raises the saving spinner
            // and drops a call while another is in flight, and neither belongs
            // to moving a cursor. The returned map is kept so the revision
            // stays current and the next real edit does not open on a conflict.
            runCatching {
                repository.applyOperations(
                    mapId = map.id,
                    operations = listOf(ThinkingOp.setSharedView(nodeId)),
                    baseRevision = map.revision,
                    idempotencyKey = java.util.UUID.randomUUID().toString(),
                )
            }.onSuccess { updated ->
                if (_state.value.open?.id == updated.id && !_state.value.saving) {
                    _state.value = _state.value.copy(open = updated)
                }
            }
        }
    }

    fun setReading(mode: ReadingMode) {
        _state.value = _state.value.copy(reading = mode)
    }

    // ── Writing ──────────────────────────────────────────────────────────────

    /**
     * Apply a sequence, refetching and retrying once on a conflict.
     *
     * One retry, not a loop: a second conflict means somebody else is actively
     * editing, and quietly winning a race against them is worse than saying so.
     * The base revision is re-read from the fresh map so the retry is against
     * what is actually there.
     */
    private fun apply(build: (ThinkingMap) -> List<kotlinx.serialization.json.JsonObject>) {
        val map = _state.value.open ?: return
        if (_state.value.saving) return
        _state.value = _state.value.copy(saving = true, saveError = null)
        // One key per attempt-set, so the retry replays rather than duplicating.
        val key = java.util.UUID.randomUUID().toString()
        viewModelScope.launch {
            val ops = build(map)
            if (ops.isEmpty()) {
                _state.value = _state.value.copy(saving = false)
                return@launch
            }
            val applied = runCatching {
                repository.applyOperations(map.id, ops, map.revision, key)
            }.recoverCatching { problem ->
                if (problem !is ThinkingMapConflict) throw problem
                val fresh = repository.map(map.id)
                repository.applyOperations(fresh.id, build(fresh), fresh.revision, key)
            }
            applied
                .onSuccess { _state.value = _state.value.copy(open = it, saving = false) }
                .onFailure {
                    _state.value = _state.value.copy(
                        saving = false,
                        saveError = it.message ?: "Could not save that change.",
                    )
                }
        }
    }

    /** Capture a thought, under whatever is in focus. */
    fun addThought(text: String, kind: ThinkingNodeKind = ThinkingNodeKind.Idea) {
        val trimmed = text.trim()
        if (trimmed.isEmpty()) return
        val newId = java.util.UUID.randomUUID().toString()
        apply { map ->
            ThinkingOps.addThought(
                nodeId = newId,
                text = trimmed,
                kind = kind,
                // Rebuilt per attempt: on a retry the focused node may itself
                // have been removed, and parenting to a ghost would fail again.
                activeId = _state.value.focusedNodeId?.takeIf { map.nodes.containsKey(it) }
                    ?: map.activeNodeId?.takeIf { map.nodes.containsKey(it) },
            )
        }
        // The new node becomes the cursor, matching iOS: capture advances.
        _state.value = _state.value.copy(focusedNodeId = newId)
    }

    fun editNode(nodeId: String, title: String, detail: String) {
        apply { map ->
            val node = map.nodes[nodeId] ?: return@apply emptyList()
            ThinkingOps.editNode(
                nodeId = nodeId,
                title = title.trim(),
                detail = detail.trim(),
                wasProvisional = EpistemicState.from(node.epistemicState) == EpistemicState.Provisional,
            )
        }
    }

    fun acceptSuggestion(nodeId: String) = apply { ThinkingOps.acceptSuggestion(nodeId) }

    fun rejectSuggestion(nodeId: String) = apply { ThinkingOps.rejectSuggestion(nodeId) }

    fun setKind(nodeId: String, kind: ThinkingNodeKind) =
        apply { listOf(ThinkingOp.setNodeKind(nodeId, kind)) }

    /**
     * Remove a node.
     *
     * The cursor moves to its parent first: focusing a tombstoned node would
     * leave the reader looking at nothing with no way back.
     */
    fun deleteNode(nodeId: String) {
        val parent = _state.value.open?.nodes?.get(nodeId)?.parentId
        _state.value = _state.value.copy(
            focusedNodeId = parent.takeIf { it != nodeId },
        )
        apply { ThinkingOps.deleteNode(nodeId) }
    }

    fun connect(from: String, to: String) =
        apply { ThinkingOps.connect(java.util.UUID.randomUUID().toString(), from, to) }

    /**
     * Remove a link.
     *
     * [connect] shipped without this, so a wrong connection could only be undone
     * by deleting a node that was not the problem.
     */
    fun disconnect(edgeId: String) = apply { ThinkingOps.disconnect(edgeId) }

    /**
     * Remove the link between two nodes, whichever way round it was drawn.
     *
     * The two-node form because that is what a reader has: Focus shows the
     * nodes something is connected to, not the edge ids joining them. Matches
     * iOS, which resolves the edge the same way.
     */
    fun disconnectNodes(nodeId: String, otherId: String) {
        val map = _state.value.open ?: return
        val edge = map.edgeList.firstOrNull { edge ->
            (edge.from == nodeId && edge.to == otherId) ||
                (edge.from == otherId && edge.to == nodeId)
        } ?: return
        disconnect(edge.id)
    }

    /** Answer a question the agent asked about a node. */
    fun answerClarification(clarificationId: String, answer: String) {
        val text = answer.trim()
        if (text.isEmpty()) return
        apply { ThinkingOps.answerClarification(clarificationId, text) }
    }

    /** Put a question aside; it stays on the record and can come back. */
    fun deferClarification(clarificationId: String) =
        apply { ThinkingOps.deferClarification(clarificationId) }

    /** Say the question does not apply. */
    fun dismissClarification(clarificationId: String) =
        apply { ThinkingOps.dismissClarification(clarificationId) }

    /** Ask the interpreter to carry on, or to break the thought open. */
    fun interpret(text: String, intent: FrontierIntent = FrontierIntent.ContinueThinking) {
        val map = _state.value.open ?: return
        val trimmed = text.trim()
        if (trimmed.isEmpty() || _state.value.saving) return
        // Minted here, sent with the request, and matched against the bus:
        // the server narrates each stage as a `ThinkingMapInterpretProgress`
        // event carrying this id, which is what keeps another device's run —
        // or an ambient auto-map — from driving this screen's strip.
        val utteranceId = java.util.UUID.randomUUID().toString()
        inFlightUtteranceId = utteranceId
        _state.value = _state.value.copy(
            saving = true,
            saveError = null,
            fallback = null,
            // Facilitating up front, not Preparing: this line is also the
            // whole story when the socket is down, and "Exploring…" is the
            // honest summary of a run we cannot hear. Stage events refine it
            // within the first ~100ms when the socket is up.
            progress = ThinkingProgress.Facilitating,
            progressNodeCount = null,
        )
        lastInterpret = trimmed to intent
        viewModelScope.launch {
            runCatching {
                repository.interpret(
                    map.id,
                    trimmed,
                    intent,
                    _state.value.focusedNodeId,
                    utteranceId,
                )
            }
                .onSuccess {
                    // Interpretation lands as operations applied server-side, so
                    // the map is refetched rather than patched here. The new
                    // moves arrive as provisional nodes, which is why nothing
                    // here builds prompts: the moves *are* the nodes.
                    val settled = runCatching { repository.map(map.id) }.getOrNull()
                    if (inFlightUtteranceId == utteranceId) inFlightUtteranceId = null
                    _state.value = _state.value.copy(
                        // Adopted only if this map is still the one on screen.
                        // The unconditional write used to resurrect a map the
                        // owner had closed — or overwrite the one they had
                        // switched to — because an interpretation they started
                        // minutes of attention ago finally came home.
                        open = if (_state.value.open?.id == map.id) {
                            settled ?: _state.value.open
                        } else {
                            _state.value.open
                        },
                        // The lock and the strip clear regardless: `saving`
                        // gates the whole screen, and holding it for a map
                        // that is no longer showing blocks the one that is.
                        saving = false,
                        progress = ThinkingProgress.Idle,
                        progressNodeCount = null,
                    )
                }
                .onFailure { problem ->
                    if (inFlightUtteranceId == utteranceId) inFlightUtteranceId = null
                    val stillShowing = _state.value.open?.id == map.id
                    _state.value = _state.value.copy(
                        saving = false,
                        progress = ThinkingProgress.Idle,
                        progressNodeCount = null,
                        // A sentence the owner can act on, and whether acting
                        // again is worth their time — but only on the map that
                        // failed. A banner about a map the owner already left
                        // reads as the current one failing.
                        fallback = if (stillShowing) {
                            classifyThinkingFailure(problem)
                        } else {
                            _state.value.fallback
                        },
                    )
                }
        }
    }

    /**
     * Try the last interpretation again.
     *
     * Only offered when the fallback said it was worth it — a rejected request
     * will be rejected the same way, and a retry button that cannot work is the
     * kind of promise this client keeps removing.
     */
    fun retryInterpret() {
        val (text, intent) = lastInterpret ?: return
        if (_state.value.fallback?.canRetry != true) return
        interpret(text, intent)
    }

    fun dismissFallback() {
        _state.value = _state.value.copy(fallback = null)
    }

    fun decideProposal(proposalId: String, decision: ProposalDecision) {
        val map = _state.value.open ?: return
        viewModelScope.launch {
            runCatching { repository.decideProposal(map.id, proposalId, decision) }
                .onSuccess {
                    runCatching { repository.map(map.id) }
                        .onSuccess { _state.value = _state.value.copy(open = it) }
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        saveError = it.message ?: "Could not record that decision.",
                    )
                }
        }
    }

    /**
     * Start a map, seeded by the first thought.
     *
     * A bare seed opens an empty map, which is the zero-ceremony path
     * `@brainstorm` with no trailing text takes.
     */
    fun startMap(seed: String, onOpened: (String) -> Unit = {}) {
        val trimmed = seed.trim()
        // iOS titles a fresh map from its first thought; an empty seed gets the
        // same placeholder rather than an untitled row nobody can find again.
        val title = trimmed.take(60).ifBlank { "New idea" }
        viewModelScope.launch {
            runCatching { repository.create(title) }
                .onSuccess { created ->
                    _state.value = _state.value.copy(open = created, focusedNodeId = null)
                    if (trimmed.isNotEmpty()) addThought(trimmed)
                    refresh()
                    onOpened(created.id)
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        error = it.message ?: "Could not start that map.",
                    )
                }
        }
    }

    // ── Map lifecycle and history ────────────────────────────────────────────

    fun rename(title: String) {
        val map = _state.value.open ?: return
        viewModelScope.launch {
            runCatching { repository.patch(map.id, title = title) }
                .onSuccess { reopen(map.id) }
                .onFailure { _state.value = _state.value.copy(saveError = it.message) }
        }
    }

    /**
     * Delete a map for good, and close it if it was open.
     *
     * The reversible `deleted` lifecycle is what [setLifecycle] does and what a
     * UI should offer first; this is the permanent one. The endpoint existed on
     * the server and nothing here ever called it, so a map could be archived but
     * never actually got rid of.
     */
    fun deleteMap(mapId: String, onDeleted: () -> Unit = {}) {
        viewModelScope.launch {
            runCatching { repository.delete(mapId) }
                .onSuccess {
                    if (_state.value.open?.id == mapId) close()
                    refresh()
                    onDeleted()
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        saveError = it.message ?: "Could not delete that map.",
                    )
                }
        }
    }

    /**
     * Copy a map into a new one.
     *
     * A restore at the current revision, which is how the server spells "the
     * same map from a point in its history" — there is no duplicate endpoint,
     * and inventing one client-side by replaying operations would produce a copy
     * that drifts from what the server would have made.
     */
    fun duplicateMap(mapId: String, onCopied: (String) -> Unit = {}) {
        viewModelScope.launch {
            val source = runCatching { repository.map(mapId) }.getOrNull()
            if (source == null) {
                _state.value = _state.value.copy(saveError = "Could not read that map to copy it.")
                return@launch
            }
            val copyId = java.util.UUID.randomUUID().toString()
            runCatching {
                repository.restore(
                    mapId = mapId,
                    atSequence = source.revision,
                    newMapId = copyId,
                    newTitle = "${source.displayTitle} copy",
                )
            }
                .onSuccess {
                    refresh()
                    onCopied(it.id)
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        saveError = it.message ?: "Could not copy that map.",
                    )
                }
        }
    }

    /**
     * Archive or restore a map.
     *
     * Archiving closes it: the library hides archived maps, so leaving it open
     * would show something the list says is gone.
     */
    fun setLifecycle(lifecycle: MapLifecycle) {
        val map = _state.value.open ?: return
        viewModelScope.launch {
            runCatching { repository.patch(map.id, lifecycle = lifecycle) }
                .onSuccess {
                    if (lifecycle == MapLifecycle.Active) reopen(map.id) else close()
                    refresh()
                }
                .onFailure { _state.value = _state.value.copy(saveError = it.message) }
        }
    }

    fun consolidate() {
        val map = _state.value.open ?: return
        if (_state.value.saving) return
        _state.value = _state.value.copy(saving = true, saveError = null)
        viewModelScope.launch {
            runCatching { repository.consolidate(map.id) }
                .onSuccess { _state.value = _state.value.copy(open = it, saving = false) }
                .onFailure {
                    _state.value = _state.value.copy(
                        saving = false,
                        saveError = it.message ?: "Could not consolidate that map.",
                    )
                }
        }
    }

    /**
     * Look at an earlier state without leaving it there.
     *
     * The live map is kept so backing out is free — browsing history should not
     * cost the reader their place in the present one.
     */
    fun replay(atSequence: Long) {
        val map = _state.value.open ?: return
        viewModelScope.launch {
            runCatching { repository.replay(map.id, atSequence) }
                .onSuccess {
                    _state.value = _state.value.copy(
                        replaying = it,
                        replayLive = _state.value.open,
                        // Held so restore knows which point was being read.
                        replaySequence = atSequence,
                        focusedNodeId = null,
                    )
                }
                .onFailure { _state.value = _state.value.copy(saveError = it.message) }
        }
    }

    fun leaveReplay() {
        _state.value = _state.value.copy(
            replaying = null, replayLive = null, replaySequence = null, focusedNodeId = null,
        )
    }

    /** Fork the replayed point into a map of its own. */
    fun restoreReplay(title: String) {
        val map = _state.value.open ?: return
        val at = _state.value.replaySequence ?: return
        viewModelScope.launch {
            runCatching {
                repository.restore(
                    mapId = map.id,
                    atSequence = at,
                    newMapId = java.util.UUID.randomUUID().toString(),
                    newTitle = title.trim().ifEmpty { "${map.displayTitle} (restored)" },
                )
            }
                .onSuccess { forked ->
                    _state.value = _state.value.copy(
                        open = forked, replaying = null, replayLive = null,
                        replaySequence = null, focusedNodeId = null,
                    )
                    refresh()
                }
                .onFailure { _state.value = _state.value.copy(saveError = it.message) }
        }
    }

    fun export(onReady: (String) -> Unit) {
        val map = _state.value.open ?: return
        viewModelScope.launch {
            runCatching { repository.exportMarkdown(map.id) }
                .onSuccess(onReady)
                .onFailure { _state.value = _state.value.copy(saveError = it.message) }
        }
    }

    /**
     * Promote a node into something that outlives the map.
     *
     * Two phases: the unconfirmed call reports what would happen, and only a
     * confirmed one commits. The preview is surfaced rather than swallowed,
     * because promoting is how a thought becomes a task somebody is then held
     * to.
     */
    fun promote(nodeId: String, target: String, confirm: Boolean = false) {
        val map = _state.value.open ?: return
        viewModelScope.launch {
            runCatching { repository.promoteNode(map.id, nodeId, target, confirm) }
                .onSuccess { outcome ->
                    if (confirm) {
                        _state.value = _state.value.copy(promotePreview = null)
                        reopen(map.id)
                    } else {
                        _state.value = _state.value.copy(promotePreview = outcome)
                    }
                }
                .onFailure { _state.value = _state.value.copy(saveError = it.message) }
        }
    }

    fun dismissPromotePreview() {
        _state.value = _state.value.copy(promotePreview = null)
    }

    fun attachSession(sessionId: String) {
        val map = _state.value.open ?: return
        viewModelScope.launch {
            runCatching { repository.attachSession(map.id, sessionId) }
                .onFailure { _state.value = _state.value.copy(saveError = it.message) }
        }
    }

    fun detachSession(sessionId: String) {
        val map = _state.value.open ?: return
        viewModelScope.launch {
            runCatching { repository.detachSession(map.id, sessionId) }
                .onFailure { _state.value = _state.value.copy(saveError = it.message) }
        }
    }

    private fun reopen(mapId: String) {
        viewModelScope.launch {
            runCatching { repository.map(mapId) }
                .onSuccess { _state.value = _state.value.copy(open = it) }
        }
    }

    override fun onCleared() {
        repository.close()
        // The socket would outlive the screen otherwise: a reconnect loop left
        // running holds one open for a map nobody is reading.
        viewModelScope.launch { realtime.stop() }
        super.onCleared()
    }
}
