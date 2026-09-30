package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.voice.AudioNoteItem
import ai.magicbeans.magdroid.voice.AudioNoteOutbox
import ai.magicbeans.magdroid.voice.AudioNoteOutboxStatus
import ai.magicbeans.magdroid.voice.AudioNotesRepository
import ai.magicbeans.magdroid.voice.VoiceMediaError
import android.app.Application
import android.media.MediaPlayer
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudUpload
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.PlayCircleOutline
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material.icons.outlined.WarningAmber
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import java.io.File
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.nio.channels.UnresolvedAddressException
import java.text.SimpleDateFormat
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class AudioNotesUiState(
    val items: List<AudioNoteItem> = emptyList(),
    val pending: List<AudioNoteOutboxStatus> = emptyList(),
    val query: String = "",
    val total: Int = 0,
    val nextOffset: Int = 0,
    val hasMore: Boolean = false,
    val loading: Boolean = false,
    val refreshing: Boolean = false,
    val loadingAudioId: String? = null,
    val playingAudioId: String? = null,
    val mutatingId: String? = null,
    val error: String? = null,
)

class AudioNotesViewModel(application: Application) : AndroidViewModel(application) {
    private val repository = AudioNotesRepository(application)
    private val _state = MutableStateFlow(AudioNotesUiState())
    val state: StateFlow<AudioNotesUiState> = _state.asStateFlow()
    private var loadJob: Job? = null
    private var playbackJob: Job? = null
    private var playbackGeneration = 0
    private var player: MediaPlayer? = null
    private var playbackFile: File? = null

    init {
        refresh()
        viewModelScope.launch {
            AudioNoteOutbox.changes.collectLatest {
                loadPending()
                load(reset = true, silent = true)
            }
        }
    }

    fun setQuery(value: String) {
        _state.value = _state.value.copy(query = value)
    }

    fun search() = load(reset = true)

    fun refresh() {
        loadPending()
        load(reset = true)
    }

    fun loadMore() = load(reset = false)

    private fun load(reset: Boolean, silent: Boolean = false) {
        if (!reset && (_state.value.loading || !_state.value.hasMore)) return
        if (reset) loadJob?.cancel()
        val query = _state.value.query.trim()
        val offset = if (reset) 0 else _state.value.nextOffset
        loadJob = viewModelScope.launch {
            _state.value = _state.value.copy(
                loading = !silent,
                refreshing = silent || (_state.value.items.isNotEmpty() && reset),
            )
            runCatching { repository.list(offset = offset, query = query) }
                .onSuccess { page ->
                    // A response for old search text must never overwrite the
                    // query the owner is now looking at.
                    if (_state.value.query.trim() != query) return@onSuccess
                    val merged = if (reset) page.items else {
                        val seen = _state.value.items.map(AudioNoteItem::id).toHashSet()
                        _state.value.items + page.items.filter { seen.add(it.id) }
                    }
                    _state.value = _state.value.copy(
                        items = merged,
                        total = page.total,
                        nextOffset = page.offset + page.items.size,
                        hasMore = page.hasMore,
                        error = null,
                    )
                }
                .onFailure { error ->
                    if (error is CancellationException) return@launch
                    _state.value = _state.value.copy(error = audioNotesUserMessage(error, "Audio Notes could not be loaded."))
                }
            _state.value = _state.value.copy(loading = false, refreshing = false)
        }
    }

    private fun loadPending() {
        viewModelScope.launch {
            runCatching { AudioNoteOutbox.statuses(getApplication()) }
                .onSuccess { _state.value = _state.value.copy(pending = it) }
                .onFailure { _state.value = _state.value.copy(error = audioNotesUserMessage(it, "Pending Audio Notes could not be read.")) }
        }
    }

    fun retry(item: AudioNoteOutboxStatus) {
        viewModelScope.launch {
            _state.value = _state.value.copy(mutatingId = item.record.id)
            runCatching { AudioNoteOutbox.retry(getApplication(), item.record.id) }
                .onFailure { _state.value = _state.value.copy(error = audioNotesUserMessage(it, "The upload could not be retried.")) }
            _state.value = _state.value.copy(mutatingId = null)
            loadPending()
        }
    }

    fun discard(item: AudioNoteOutboxStatus) {
        viewModelScope.launch {
            if (_state.value.playingAudioId == item.record.id) stopPlayback()
            _state.value = _state.value.copy(mutatingId = item.record.id)
            runCatching { AudioNoteOutbox.discard(getApplication(), item.record.id) }
                .onFailure { _state.value = _state.value.copy(error = audioNotesUserMessage(it, "The local recording could not be discarded.")) }
            _state.value = _state.value.copy(mutatingId = null)
            loadPending()
        }
    }

    fun delete(item: AudioNoteItem) {
        viewModelScope.launch {
            if (_state.value.playingAudioId == item.id) stopPlayback()
            _state.value = _state.value.copy(mutatingId = item.id)
            runCatching { repository.delete(item.id) }
                .onSuccess {
                    val remained = _state.value.items.filterNot { it.id == item.id }
                    _state.value = _state.value.copy(
                        items = remained,
                        total = (_state.value.total - 1).coerceAtLeast(0),
                        nextOffset = (_state.value.nextOffset - 1).coerceAtLeast(0),
                        error = null,
                    )
                }
                .onFailure { _state.value = _state.value.copy(error = audioNotesUserMessage(it, "The Audio Note could not be deleted.")) }
            _state.value = _state.value.copy(mutatingId = null)
        }
    }

    fun toggle(item: AudioNoteItem) {
        if (_state.value.playingAudioId == item.id) return stopPlayback()
        stopPlayback()
        val generation = playbackGeneration
        playbackJob = viewModelScope.launch {
            beginPlayback(item.id) { destination ->
                val bytes = repository.recording(item.id)
                withContext(Dispatchers.IO) {
                    destination.outputStream().use { it.write(bytes) }
                }
            }
            if (playbackGeneration == generation) playbackJob = null
        }
    }

    fun toggle(item: AudioNoteOutboxStatus) {
        if (_state.value.playingAudioId == item.record.id) return stopPlayback()
        stopPlayback()
        val generation = playbackGeneration
        playbackJob = viewModelScope.launch {
            beginPlayback(item.record.id) { destination ->
                if (!AudioNoteOutbox.copyRecording(getApplication(), item.record.id, destination)) {
                    throw VoiceMediaError("The pending recording is missing or empty.", false)
                }
            }
            if (playbackGeneration == generation) playbackJob = null
        }
    }

    private suspend fun beginPlayback(id: String, prepareFile: suspend (File) -> Unit) {
        _state.value = _state.value.copy(loadingAudioId = id)
        val destination = File.createTempFile("audio-note-$id-", ".wav", getApplication<Application>().cacheDir)
        runCatching {
            prepareFile(destination)
            MediaPlayer().also { next ->
                next.setDataSource(destination.absolutePath)
                next.setOnCompletionListener { stopPlayback() }
                next.setOnErrorListener { _, _, _ ->
                    _state.value = _state.value.copy(error = "The recording could not be played on this phone.")
                    stopPlayback()
                    true
                }
                next.prepare()
                check(next.startAndReport()) { "The recording could not be started." }
                player = next
                playbackFile = destination
            }
        }.onSuccess {
            _state.value = _state.value.copy(loadingAudioId = null, playingAudioId = id, error = null)
        }.onFailure { error ->
            if (error is CancellationException) {
                destination.delete()
                return
            }
            destination.delete()
            _state.value = _state.value.copy(
                loadingAudioId = null,
                playingAudioId = null,
                error = audioNotesUserMessage(error, "The recording could not be played."),
            )
        }
    }

    private fun MediaPlayer.startAndReport(): Boolean = runCatching { start(); isPlaying }.getOrDefault(false)

    fun stopPlayback() {
        playbackGeneration += 1
        playbackJob?.cancel()
        playbackJob = null
        runCatching { player?.stop() }
        player?.release()
        player = null
        playbackFile?.delete()
        playbackFile = null
        _state.value = _state.value.copy(loadingAudioId = null, playingAudioId = null)
    }

    fun clearError() {
        _state.value = _state.value.copy(error = null)
    }

    override fun onCleared() {
        stopPlayback()
        repository.close()
        super.onCleared()
    }
}

internal fun audioNotesUserMessage(error: Throwable, fallback: String): String {
    val causes = generateSequence(error as Throwable?) { it.cause }.take(8).toList()
    return when {
        causes.any { it is UnresolvedAddressException || it is ConnectException || it is UnknownHostException } ->
            "Magician is offline. Start the backend or check this phone's connection in Settings."
        causes.any { it is SocketTimeoutException || it.javaClass.simpleName.contains("Timeout", ignoreCase = true) } ->
            "Magician did not respond in time. The recording is still safe; try again."
        error is VoiceMediaError && !error.message.isNullOrBlank() -> error.message!!
        !error.message.isNullOrBlank() -> "$fallback ${error.message}"
        else -> fallback
    }
}

@Composable
fun AudioNotesScreen(viewModel: AudioNotesViewModel = viewModel()) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    var deleteTarget by remember { mutableStateOf<AudioNoteDeleteTarget?>(null) }
    DisposableEffect(viewModel) { onDispose(viewModel::stopPlayback) }

    Box(Modifier.fillMaxSize().background(Ground)) {
        LazyColumn(
            Modifier.fillMaxSize(),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    MagicianTextField(
                        value = state.query,
                        onValueChange = viewModel::setQuery,
                        placeholder = { Text("Search transcripts") },
                        leadingIcon = { Icon(Icons.Outlined.Search, null) },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                        keyboardActions = KeyboardActions(onSearch = { viewModel.search() }),
                        modifier = Modifier.weight(1f),
                    )
                    IconButton(onClick = viewModel::refresh, enabled = !state.loading) {
                        Icon(Icons.Outlined.Refresh, "Refresh Audio Notes", tint = Coral)
                    }
                }
            }

            if (state.pending.isNotEmpty()) {
                item { AudioNotesHeader("On this phone", "${state.pending.size} waiting") }
                items(state.pending, key = { "pending-${it.record.id}" }) { item ->
                    AudioNoteSwipeCard(
                        id = "pending-${item.record.id}",
                        enabled = state.mutatingId != item.record.id,
                        onDelete = { deleteTarget = AudioNoteDeleteTarget.Pending(item) },
                    ) {
                        PendingAudioNoteCard(item, state, viewModel)
                    }
                }
            }

            item { AudioNotesHeader("Saved", if (state.total == 1) "1 note" else "${state.total} notes") }
            if (state.items.isEmpty() && !state.loading) {
                item {
                    EmptyAudioNotes(
                        if (state.query.isBlank()) "No saved Audio Notes yet."
                        else "No Audio Notes match “${state.query.trim()}”.",
                    )
                }
            }
            items(state.items, key = { "saved-${it.id}" }) { item ->
                AudioNoteSwipeCard(
                    id = "saved-${item.id}",
                    enabled = state.mutatingId != item.id,
                    onDelete = { deleteTarget = AudioNoteDeleteTarget.Saved(item) },
                ) {
                    SavedAudioNoteCard(item, state, viewModel)
                }
            }
            if (state.hasMore) {
                item {
                    Button(shape = MagicanButtonShape,
                        onClick = viewModel::loadMore,
                        enabled = !state.loading,
                        colors = ButtonDefaults.buttonColors(containerColor = Coral, contentColor = Color.White),
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                    ) { Text(if (state.loading) "Loading…" else "Load more") }
                }
            }
            item { Spacer(Modifier.height(20.dp)) }
        }
        if (state.loading && state.items.isEmpty()) {
            Column(
                Modifier.align(Alignment.Center),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                CircularProgressIndicator(color = Coral, modifier = Modifier.size(28.dp))
                Text("Loading Audio Notes…", color = Muted, fontSize = 12.sp)
            }
        }
    }

    deleteTarget?.let { target ->
        AlertDialog(
            onDismissRequest = { deleteTarget = null },
            title = { Text(if (target is AudioNoteDeleteTarget.Pending) "Discard recording?" else "Delete Audio Note?") },
            text = { Text("This permanently removes the recording and transcript. It cannot be undone.") },
            confirmButton = {
                TextButton(onClick = {
                    deleteTarget = null
                    when (target) {
                        is AudioNoteDeleteTarget.Pending -> viewModel.discard(target.item)
                        is AudioNoteDeleteTarget.Saved -> viewModel.delete(target.item)
                    }
                }) { Text(if (target is AudioNoteDeleteTarget.Pending) "Discard" else "Delete", color = Danger) }
            },
            dismissButton = { TextButton(onClick = { deleteTarget = null }) { Text("Cancel") } },
            containerColor = Panel,
        )
    }
    state.error?.let { message ->
        AlertDialog(
            onDismissRequest = viewModel::clearError,
            title = { Text("Audio Notes") },
            text = { Text(message) },
            confirmButton = { TextButton(onClick = viewModel::clearError) { Text("OK", color = Coral) } },
            containerColor = Panel,
        )
    }
}

private sealed interface AudioNoteDeleteTarget {
    data class Pending(val item: AudioNoteOutboxStatus) : AudioNoteDeleteTarget
    data class Saved(val item: AudioNoteItem) : AudioNoteDeleteTarget
}

@Composable
private fun AudioNoteSwipeCard(id: String, enabled: Boolean, onDelete: () -> Unit, content: @Composable () -> Unit) {
    TodaySwipeActionCard(
        itemId = id,
        leadingActions = emptyList(),
        trailingActions = listOf(TodaySwipeAction("delete", "Delete", Icons.Outlined.Delete, Danger, onDelete)),
        enabled = enabled,
        modifier = Modifier.padding(horizontal = 16.dp),
        content = content,
    )
}

@Composable
private fun PendingAudioNoteCard(item: AudioNoteOutboxStatus, state: AudioNotesUiState, viewModel: AudioNotesViewModel) {
    val failed = item.record.failedPermanently
    AudioNoteCard {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            AudioPlayButton(
                id = item.record.id,
                state = state,
                onClick = { viewModel.toggle(item) },
            )
            Icon(
                if (failed) Icons.Outlined.WarningAmber else Icons.Outlined.CloudUpload,
                null, tint = if (failed) Danger else Coral, modifier = Modifier.size(17.dp),
            )
            Text(
                if (failed) "Needs attention" else if (item.record.attemptCount > 0) "Waiting to retry" else "Waiting to upload",
                color = if (failed) Danger else Coral,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            if (failed) {
                Text(
                    "Retry", color = Coral, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable { viewModel.retry(item) }.padding(5.dp),
                )
            }
        }
        Text(
            item.record.transcript ?: "Audio-only note",
            color = Ink, fontSize = 13.sp, lineHeight = 17.sp, maxLines = 4,
            overflow = TextOverflow.Ellipsis,
        )
        item.record.lastError?.let { Text(it, color = Muted, fontSize = 11.sp, lineHeight = 14.sp) }
        Text(
            "${formatAudioDate(item.record.capturedAt)} · ${formatDuration(item.record.durationMs)} · ${formatBytes(item.bytes)}",
            color = Muted, fontSize = 10.sp,
        )
    }
}

@Composable
private fun SavedAudioNoteCard(item: AudioNoteItem, state: AudioNotesUiState, viewModel: AudioNotesViewModel) {
    AudioNoteCard {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            AudioPlayButton(id = item.id, state = state, onClick = { viewModel.toggle(item) })
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(formatAudioDate(item.capturedAt), color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    item.provider.replace('_', ' ') + if (item.usedFallback) " · fallback" else "",
                    color = Muted, fontSize = 11.sp,
                )
            }
            Text(formatDuration(item.durationMs), color = Muted, fontSize = 11.sp)
        }
        Text(
            item.transcript ?: "No transcript was available.",
            color = Ink, fontSize = 13.sp, lineHeight = 17.sp, maxLines = 5,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun AudioNoteCard(content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(13.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            Modifier.fillMaxWidth().padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            content = content,
        )
    }
}

@Composable
private fun AudioPlayButton(id: String, state: AudioNotesUiState, onClick: () -> Unit) {
    Box(Modifier.size(36.dp), contentAlignment = Alignment.Center) {
        if (state.loadingAudioId == id) {
            CircularProgressIndicator(color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(24.dp))
        } else {
            IconButton(onClick = onClick) {
                Icon(
                    if (state.playingAudioId == id) Icons.Outlined.StopCircle else Icons.Outlined.PlayCircleOutline,
                    if (state.playingAudioId == id) "Stop recording" else "Play recording",
                    tint = Coral, modifier = Modifier.size(28.dp),
                )
            }
        }
    }
}

@Composable
private fun AudioNotesHeader(title: String, detail: String) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 18.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.weight(1f))
        Text(detail, color = Muted, fontSize = 11.sp)
    }
}

@Composable
private fun EmptyAudioNotes(message: String) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(13.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
    ) {
        Column(
            Modifier.fillMaxWidth().padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Icon(Icons.Outlined.GraphicEq, null, tint = Muted, modifier = Modifier.size(30.dp))
            Text(message, color = Muted, fontSize = 13.sp)
        }
    }
}

private fun formatAudioDate(value: String): String {
    val parsers = listOf(
        SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'", Locale.US),
        SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss'Z'", Locale.US),
    ).onEach { it.timeZone = java.util.TimeZone.getTimeZone("UTC") }
    val date = parsers.firstNotNullOfOrNull { runCatching { it.parse(value) }.getOrNull() } ?: return value
    return SimpleDateFormat("MMM d, yyyy · h:mm a", Locale.getDefault()).format(date)
}

private fun formatDuration(value: Long?): String {
    val seconds = (value ?: return "—") / 1_000
    return "%d:%02d".format(Locale.US, seconds / 60, seconds % 60)
}

private fun formatBytes(bytes: Long): String = when {
    bytes >= 1024 * 1024 -> "%.1f MB".format(Locale.US, bytes / 1024.0 / 1024.0)
    bytes >= 1024 -> "%.0f KB".format(Locale.US, bytes / 1024.0)
    else -> "$bytes B"
}
