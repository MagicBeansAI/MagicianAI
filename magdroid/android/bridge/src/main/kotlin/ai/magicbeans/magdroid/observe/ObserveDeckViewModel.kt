package ai.magicbeans.magdroid.observe

import ai.magicbeans.magdroid.net.toFailure
import ai.magicbeans.magdroid.voice.RealtimeVoiceCatalog
import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * One independently loaded block: its last good value, whether a load is in
 * flight, and the failure of the last attempt. A failed refresh keeps the last
 * good value on screen next to the error rather than blanking it.
 */
data class DeckLane<T>(
    val value: T? = null,
    val loading: Boolean = false,
    val error: String? = null,
) {
    val loaded: Boolean get() = value != null
    fun started(): DeckLane<T> = copy(loading = true, error = null)
    fun succeeded(next: T): DeckLane<T> = DeckLane(next, loading = false, error = null)
    fun failed(message: String): DeckLane<T> = copy(loading = false, error = message)
}

data class ObserveDeckState(
    val recent: DeckLane<List<RecentMeeting>> = DeckLane(),
    val channels: DeckLane<List<ChannelAssistChannel>> = DeckLane(),
    val calendar: DeckLane<CalendarObserveStatus> = DeckLane(),
    val subscriptions: DeckLane<ObservationSubscriptionPage> = DeckLane(),
    val ambient: DeckLane<AmbientStatus> = DeckLane(),
    val catchUp: DeckLane<CatchUpStatus> = DeckLane(),
    /** The account's selected profile per surface wire name. */
    val audioSelection: DeckLane<Map<String, String>> = DeckLane(),
    val audioCatalog: DeckLane<RealtimeVoiceCatalog> = DeckLane(),
    /** Surface wire currently saving, so its picker disables. */
    val audioSaving: String? = null,
    val audioSaveError: String? = null,
) {
    val sourcesOn: Int
        get() = enabledSourcesCount(
            channels = channels.value,
            calendarEnabled = calendar.value?.enabled,
            ambientEnabled = ambient.value?.enabled,
            enabledSubscriptions = subscriptions.value?.total,
        )
}

/**
 * Recent captures, the view-only web & account sources, and the two
 * transcription-profile pickers of the Observe deck.
 *
 * Meetings (active/upcoming) stay in [ai.magicbeans.magdroid.meetings.MeetingsViewModel]
 * and published notes in their own model; this only adds the lanes the deck
 * introduced.
 */
class ObserveDeckViewModel(app: Application) : AndroidViewModel(app) {

    private val repository = ObserveDeckRepository(app)
    private val _state = MutableStateFlow(ObserveDeckState())
    val state: StateFlow<ObserveDeckState> = _state.asStateFlow()

    /** Everything, each lane on its own. */
    fun refreshAll() {
        loadRecent()
        loadSources()
        loadAudio()
    }

    fun loadRecent() = lane(
        get = { it.recent },
        set = { s, v -> s.copy(recent = v) },
        doing = "recent captures",
    ) { repository.recent() }

    fun loadSources() {
        loadChannels()
        loadCalendar()
        loadSubscriptions()
        loadAmbient()
        loadCatchUp()
    }

    fun loadChannels() = lane({ it.channels }, { s, v -> s.copy(channels = v) }, "mail & chat channels") {
        repository.channels()
    }

    fun loadCalendar() = lane({ it.calendar }, { s, v -> s.copy(calendar = v) }, "calendar observation") {
        repository.calendarStatus()
    }

    fun loadSubscriptions() = lane({ it.subscriptions }, { s, v -> s.copy(subscriptions = v) }, "continuous sources") {
        repository.enabledSubscriptions()
    }

    fun loadAmbient() = lane({ it.ambient }, { s, v -> s.copy(ambient = v) }, "browser tabs") {
        repository.ambientStatus()
    }

    fun loadCatchUp() = lane({ it.catchUp }, { s, v -> s.copy(catchUp = v) }, "startup catch-up") {
        repository.catchUp()
    }

    fun loadAudio() {
        lane({ it.audioSelection }, { s, v -> s.copy(audioSelection = v) }, "audio preferences") {
            repository.surfaceProfiles()
        }
        lane({ it.audioCatalog }, { s, v -> s.copy(audioCatalog = v) }, "audio profiles") {
            repository.audioCatalog()
        }
    }

    /**
     * Choose [profileId] (null = the configured default) for [surface].
     *
     * Optimistic, like the web store: the choice shows at once and is rolled
     * back with the server's reason if the save fails.
     */
    fun selectAudioProfile(surface: ObserveAudioSurface, profileId: String?) {
        val current = _state.value
        if (current.audioSaving != null) return
        val previous = current.audioSelection.value ?: return
        val optimistic = applySurfaceProfile(previous, surface.wire, profileId)
        if (optimistic == previous) return
        _state.update {
            it.copy(
                audioSelection = it.audioSelection.succeeded(optimistic),
                audioSaving = surface.wire,
                audioSaveError = null,
            )
        }
        viewModelScope.launch {
            runCatching { repository.saveSurfaceProfile(surface.wire, profileId) }
                .onSuccess { saved ->
                    _state.update { it.copy(audioSelection = it.audioSelection.succeeded(saved), audioSaving = null) }
                }
                .onFailure { error ->
                    _state.update {
                        it.copy(
                            audioSelection = it.audioSelection.succeeded(previous),
                            audioSaving = null,
                            audioSaveError = "Couldn't save the ${surface.label.lowercase()} profile: " +
                                error.toFailure(getApplication(), "audio preferences").headline,
                        )
                    }
                }
        }
    }

    private fun <T> lane(
        get: (ObserveDeckState) -> DeckLane<T>,
        set: (ObserveDeckState, DeckLane<T>) -> ObserveDeckState,
        doing: String,
        load: suspend () -> T,
    ) {
        if (get(_state.value).loading) return
        _state.update { set(it, get(it).started()) }
        viewModelScope.launch {
            runCatching { load() }
                .onSuccess { value -> _state.update { set(it, get(it).succeeded(value)) } }
                .onFailure { error ->
                    val message = error.toFailure(getApplication(), doing).headline
                    _state.update { set(it, get(it).failed(message)) }
                }
        }
    }

    override fun onCleared() {
        repository.close()
        super.onCleared()
    }
}
