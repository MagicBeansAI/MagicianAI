package ai.magicbeans.magdroid.notes

import ai.magicbeans.magdroid.net.toFailure
import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min

/** The page sizes iOS and the web offer. */
val PUBLISHED_NOTES_PAGE_SIZES: List<Int> = listOf(5, 10, 20, 50)
const val PUBLISHED_NOTES_DEFAULT_PAGE_SIZE = 5
/** Search text is capped where iOS caps it. */
const val PUBLISHED_NOTES_SEARCH_MAX = 120

@Serializable
data class PublishedTaskNotesBackfillFailure(
    @SerialName("task_id") val taskId: String = "",
    val error: String = "",
)

@Serializable
data class PublishedTaskNotesBackfillPagination(
    @SerialName("has_more") val hasMore: Boolean = false,
)

@Serializable
data class PublishedTaskNotesBackfillReceipt(
    val published: List<PublishedTaskNote> = emptyList(),
    val errors: List<PublishedTaskNotesBackfillFailure> = emptyList(),
    val pagination: PublishedTaskNotesBackfillPagination = PublishedTaskNotesBackfillPagination(),
)

@Serializable
data class PublishedTaskNotePromotionCandidate(val id: String = "", val state: String = "")

@Serializable
data class PublishedTaskNotePromotionReceipt(
    val candidate: PublishedTaskNotePromotionCandidate = PublishedTaskNotePromotionCandidate(),
)

/** `{limit, only_unpublished: true}` — the batch the web and iOS send. */
fun backfillRequestBody(limit: Int): String = buildJsonObject {
    put("limit", limit)
    put("only_unpublished", true)
}.toString()

/** (success, failure) sentences for a backfill, worded as iOS words them. */
fun backfillOutcome(receipt: PublishedTaskNotesBackfillReceipt): Pair<String?, String?> = when {
    receipt.published.isNotEmpty() -> {
        val n = receipt.published.size
        "Published $n completed task${if (n == 1) "" else "s"}" +
            (if (receipt.pagination.hasMore) "; more remain." else ".") to null
    }
    receipt.errors.isNotEmpty() -> {
        val n = receipt.errors.size
        null to "$n task page${if (n == 1) "" else "s"} could not be published."
    }
    else -> "Completed tasks are already published." to null
}

/** Paging arithmetic shared by the pager and its tests. */
data class PublishedNotesPager(
    val currentPage: Int,
    val pageSize: Int,
    val offset: Int,
    val itemCount: Int,
    val total: Int,
    val hasMore: Boolean,
) {
    val pageCount: Int get() = pageCountFor(total, pageSize)
    val start: Int get() = if (total == 0) 0 else offset + 1
    val end: Int get() = if (total == 0) 0 else min(total, offset + itemCount)
    val canPrevious: Boolean get() = currentPage > 1
    val canNext: Boolean get() = currentPage < pageCount && hasMore
    val pageLabel: String get() = "Page $currentPage of $pageCount"
    val rangeLabel: String get() = "$start–$end of $total"

    companion object {
        fun pageCountFor(total: Int, pageSize: Int): Int =
            max(1, ceil(total.toDouble() / max(1, pageSize).toDouble()).toInt())

        fun offsetFor(page: Int, pageSize: Int): Int = (max(1, page) - 1) * pageSize
    }
}

data class PublishedNotesUiState(
    val items: List<PublishedTaskNote> = emptyList(),
    val currentPage: Int = 1,
    val pageSize: Int = PUBLISHED_NOTES_DEFAULT_PAGE_SIZE,
    val offset: Int = 0,
    val total: Int = 0,
    val hasMore: Boolean = false,
    val loading: Boolean = false,
    val loaded: Boolean = false,
    val backfilling: Boolean = false,
    val promotingTaskId: String? = null,
    val appliedSearch: String = "",
    val error: String? = null,
    val success: String? = null,
) {
    val pager: PublishedNotesPager
        get() = PublishedNotesPager(currentPage, pageSize, offset, items.size, total, hasMore)
}

/**
 * Published Notes at iOS parity: search, page size, Previous/Next, Publish
 * next 25 and Promote to memory. Mirrors `PublishedTaskNotesViewModel` in
 * `magios/Magios/PublishedTaskNotes.swift`, including the out-of-range page
 * correction and the stale-response guard.
 */
class PublishedNotesViewModel(app: Application) : AndroidViewModel(app) {

    private val repository = PublishedTaskNotesRepository(app)
    private val _state = MutableStateFlow(PublishedNotesUiState())
    val state: StateFlow<PublishedNotesUiState> = _state.asStateFlow()
    private var generation = 0

    fun loadIfNeeded() {
        val s = _state.value
        if (!s.loaded && !s.loading) load()
    }

    fun reload() {
        _state.update { it.copy(success = null) }
        load()
    }

    fun submitSearch(text: String) {
        val applied = text.take(PUBLISHED_NOTES_SEARCH_MAX).trim()
        _state.update { it.copy(appliedSearch = applied, currentPage = 1, success = null) }
        load()
    }

    fun clearSearch() = submitSearch("")

    fun setPageSize(size: Int) {
        if (size !in PUBLISHED_NOTES_PAGE_SIZES || size == _state.value.pageSize) return
        _state.update { it.copy(pageSize = size, currentPage = 1, success = null) }
        load()
    }

    fun previousPage() {
        val s = _state.value
        if (!s.pager.canPrevious || s.loading) return
        _state.update { it.copy(currentPage = it.currentPage - 1, success = null) }
        load()
    }

    fun nextPage() {
        val s = _state.value
        if (!s.pager.canNext || s.loading) return
        _state.update { it.copy(currentPage = it.currentPage + 1, success = null) }
        load()
    }

    fun promote(note: PublishedTaskNote) {
        if (_state.value.promotingTaskId != null) return
        _state.update { it.copy(promotingTaskId = note.taskId, success = null) }
        viewModelScope.launch {
            runCatching { repository.promoteToMemory(note.taskId) }
                .onSuccess { receipt ->
                    val state = receipt.candidate.state.takeIf(String::isNotBlank)?.let { " ($it)" }.orEmpty()
                    _state.update {
                        it.copy(promotingTaskId = null, error = null, success = "Memory candidate created for review$state.")
                    }
                }
                .onFailure { e ->
                    _state.update {
                        it.copy(promotingTaskId = null, error = e.toFailure(getApplication(), "promoting the note").headline)
                    }
                }
        }
    }

    fun backfillNextBatch() {
        if (_state.value.backfilling) return
        _state.update { it.copy(backfilling = true, success = null, error = null) }
        viewModelScope.launch {
            runCatching { repository.backfill() }
                .onSuccess { receipt ->
                    val (success, failure) = backfillOutcome(receipt)
                    _state.update { it.copy(backfilling = false, currentPage = 1) }
                    load(afterLoad = { s -> if (s.error == null) s.copy(success = success, error = failure) else s })
                }
                .onFailure { e ->
                    _state.update {
                        it.copy(backfilling = false, error = e.toFailure(getApplication(), "publishing notes").headline)
                    }
                }
        }
    }

    private fun load(afterLoad: (PublishedNotesUiState) -> PublishedNotesUiState = { it }) {
        val token = ++generation
        val snapshot = _state.value
        val size = snapshot.pageSize
        val query = snapshot.appliedSearch
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            var target = snapshot.currentPage
            var mayCorrect = true
            while (true) {
                val result = runCatching {
                    repository.page(offset = PublishedNotesPager.offsetFor(target, size), limit = size, query = query)
                }
                if (token != generation) return@launch
                val page = result.getOrElse { e ->
                    _state.update {
                        afterLoad(
                            it.copy(
                                loading = false,
                                error = "Published Notes could not be loaded: " +
                                    e.toFailure(getApplication(), "published notes").headline,
                            ),
                        )
                    }
                    return@launch
                }
                val pages = PublishedNotesPager.pageCountFor(page.total, size)
                if (target > pages && mayCorrect) {
                    target = pages
                    mayCorrect = false
                    continue
                }
                _state.update {
                    afterLoad(
                        it.copy(
                            items = page.items,
                            currentPage = target,
                            offset = page.offset,
                            total = page.total,
                            hasMore = page.hasMore,
                            loading = false,
                            loaded = true,
                        ),
                    )
                }
                return@launch
            }
        }
    }

    override fun onCleared() {
        repository.close()
        super.onCleared()
    }
}
