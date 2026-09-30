package ai.magicbeans.magdroid.meetings

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.toFailure
import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

data class MeetingsUiState(
    val active: List<ActiveMeeting> = emptyList(),
    val upcoming: List<UpcomingMeeting> = emptyList(),
    val loadingActive: Boolean = false,
    val loadingUpcoming: Boolean = false,
    /**
     * Errors are held per source, not shared.
     *
     * A calendar that cannot be read must not make the meeting Magician is
     * sitting in look like it failed too, and vice versa.
     */
    val activeFailure: Failure? = null,
    val upcomingFailure: Failure? = null,
    /** A failed action on a meeting — ending one — rather than a failed read. */
    val activeError: String? = null,
    val joining: Boolean = false,
    val joinError: String? = null,
    /** Session ids being ended, so a second tap cannot double-send. */
    val stopping: Set<String> = emptySet(),
    /**
     * The transcript of the meeting being followed, oldest first.
     *
     * Held here rather than per-meeting because only one is ever on screen —
     * and keeping a map of them would mean deciding when to evict transcripts
     * for meetings nobody is looking at.
     */
    val transcript: List<TranscriptLine> = emptyList(),
    /** Which session [transcript] belongs to, so a stale poll cannot land. */
    val transcriptSessionId: String? = null,
) {
    val hasNow: Boolean get() = active.isNotEmpty()
    val visibleActive: List<ActiveMeeting> get() = active.filterNot { it.sessionId in stopping }
}

/**
 * Meetings in progress, and the ones coming up.
 *
 * Two sources, loaded and failed independently, because the server keeps them
 * on separate endpoints for exactly that reason — the calendar reaches a CLI
 * that can be slow or unauthorised, and the sessions listing must not wait for
 * it or die with it.
 */
class MeetingsViewModel(app: Application) : AndroidViewModel(app) {

    private val repository = MeetingsRepository(app)
    private val _state = MutableStateFlow(MeetingsUiState())
    val state: StateFlow<MeetingsUiState> = _state.asStateFlow()

    private var transcriptJob: Job? = null
    private var activePollJob: Job? = null

    /**
     * Follow one meeting's transcript while it is on screen.
     *
     * Idempotent for the same session, so a recomposition does not restart the
     * poll and clear what is already shown. Switching sessions clears first —
     * carrying the previous meeting's lines under a new title reads as this
     * meeting having said them.
     */
    fun followTranscript(meeting: ActiveMeeting) {
        val sessionId = meeting.sessionId
        // The transcript is the THREAD's, not the session's: the server's sink
        // posts lines into the thread's chat session. A meeting without a
        // thread has nowhere lines could land, so there is nothing to follow.
        val threadId = meeting.threadId?.takeIf { it.isNotBlank() }
        if (threadId == null) {
            stopFollowingTranscript()
            return
        }
        if (_state.value.transcriptSessionId == sessionId && transcriptJob?.isActive == true) return
        transcriptJob?.cancel()
        _state.value = _state.value.copy(transcript = emptyList(), transcriptSessionId = sessionId)
        transcriptJob = viewModelScope.launch {
            while (isActive) {
                val lines = runCatching { repository.transcript(threadId) }.getOrNull()
                // Only if the reader has not moved on, and only when there is
                // something: a failed poll keeps the lines already on screen
                // rather than blanking a transcript because one request lost.
                if (_state.value.transcriptSessionId != sessionId) break
                if (!lines.isNullOrEmpty()) {
                    _state.value = _state.value.copy(transcript = lines)
                }
                delay(POLL_MS)
            }
        }
    }

    /** Stop following, and drop what was shown. */
    fun stopFollowingTranscript() {
        transcriptJob?.cancel()
        transcriptJob = null
        _state.value = _state.value.copy(transcript = emptyList(), transcriptSessionId = null)
    }

    /** Poll the fast active-session rail only while Observe is visible. */
    fun start() {
        if (activePollJob?.isActive == true) return
        // Re-entering Observe may reuse this Activity-scoped ViewModel after a
        // long time away. Refresh both independent lanes before polling only
        // the active one.
        refresh()
        // iOS light-polls the active rail while Observe is visible. This
        // keeps summaries, remote stops, and a just-opened local session moving
        // without touching the slower cached calendar lane.
        activePollJob = viewModelScope.launch {
            while (isActive) {
                delay(ACTIVE_POLL_MS)
                loadActive()
            }
        }
    }

    /** Stop screen-owned polling when Observe leaves composition. */
    fun stop() {
        activePollJob?.cancel()
        activePollJob = null
    }

    /** Both halves, each on its own. */
    fun refresh(refreshCalendar: Boolean = false) {
        loadActive()
        loadUpcoming(refreshCalendar)
    }

    /** Refresh only the live rail; used by active-session retry controls. */
    fun refreshActive() = loadActive()

    private fun loadActive() {
        if (_state.value.loadingActive) return
        _state.value = _state.value.copy(loadingActive = true, activeFailure = null)
        viewModelScope.launch {
            runCatching { repository.active() }
                .onSuccess {
                    _state.value = _state.value.copy(
                        active = it, loadingActive = false, stopping = emptySet(),
                    )
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        loadingActive = false,
                        activeFailure = it.toFailure(getApplication(), "active meetings"),
                    )
                }
        }
    }

    private fun loadUpcoming(refreshCalendar: Boolean) {
        if (_state.value.loadingUpcoming) return
        _state.value = _state.value.copy(loadingUpcoming = true, upcomingFailure = null)
        viewModelScope.launch {
            runCatching { repository.upcoming(refresh = refreshCalendar) }
                .onSuccess { response ->
                    _state.value = _state.value.copy(
                        upcoming = response.all(),
                        loadingUpcoming = false,
                        // The server states its own calendar failure in-band,
                        // with an empty list. Carried through rather than shown
                        // as "nothing on", which is a different fact. It is the
                        // server's own sentence, so it is reported as given
                        // rather than classified into a cause we did not observe.
                        upcomingFailure = response.errorSummary()?.let { stated ->
                            Failure(FailureKind.Unknown, stated, "", retryable = true)
                        },
                    )
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        loadingUpcoming = false,
                        upcomingFailure = it.toFailure(getApplication(), "your calendar"),
                    )
                }
        }
    }

    /** Send the attendee bot into a call. */
    fun join(url: String, title: String? = null) {
        val trimmed = url.trim()
        if (trimmed.isEmpty() || _state.value.joining) return
        _state.value = _state.value.copy(joining = true, joinError = null)
        viewModelScope.launch {
            runCatching { repository.join(trimmed, title) }
                .onSuccess {
                    _state.value = _state.value.copy(joining = false)
                    // Only the sessions half: joining does not change what the
                    // calendar says, and refetching it would spend a slow call
                    // for nothing.
                    loadActive()
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        joining = false,
                        joinError = it.message ?: "Magician could not join that meeting.",
                    )
                }
        }
    }

    /**
     * End a meeting.
     *
     * The row leaves at once and the id guards against a second tap, the same
     * shape the Attention dismissal uses. A meeting that lingers after "end"
     * reads as a button that did not work, and ending twice is worth avoiding.
     */
    fun stop(meeting: ActiveMeeting) {
        if (meeting.sessionId in _state.value.stopping) return
        _state.value = _state.value.copy(stopping = _state.value.stopping + meeting.sessionId)
        viewModelScope.launch {
            runCatching { repository.stop(meeting.sessionId) }
                .onSuccess { loadActive() }
                .onFailure {
                    _state.value = _state.value.copy(
                        stopping = _state.value.stopping - meeting.sessionId,
                        activeError = it.message ?: "Could not end that meeting.",
                    )
                }
        }
    }

    override fun onCleared() {
        stop()
        stopFollowingTranscript()
        repository.close()
        super.onCleared()
    }

    private companion object {
        /** Four seconds, as iOS polls. Fast enough to read along, slow
         *  enough not to be a request per spoken sentence. */
        const val POLL_MS = 4_000L
        /** Same light active-session cadence as iOS. */
        const val ACTIVE_POLL_MS = 15_000L
    }
}
