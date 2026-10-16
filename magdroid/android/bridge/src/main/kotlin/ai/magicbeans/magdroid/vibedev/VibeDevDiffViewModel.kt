package ai.magicbeans.magdroid.vibedev

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

data class VibeDevDiffData(
    val files: List<VibeDevChangedFile> = emptyList(),
    val index: Int = 0,
    val patch: VibeDevFilePatch? = null,
    val diff: UnifiedDiff? = null,
    /** Hunks the reader folded shut, by index. */
    val collapsed: Set<Int> = emptySet(),
    /** Folded unchanged runs the reader opened, as "hunk:row". */
    val unfolded: Set<String> = emptySet(),
) {
    val file: VibeDevChangedFile? get() = files.getOrNull(index)
    val hasPrevious: Boolean get() = index > 0
    val hasNext: Boolean get() = index < files.lastIndex
}

/** One file's diff, with previous/next across the run's changed files. */
class VibeDevDiffViewModel(
    val runId: String,
    val turn: String?,
    files: List<VibeDevChangedFile>,
    startPath: String,
    source: () -> VibeDevSource,
    live: Flow<VibeDevSignal>,
    clock: () -> Long = System::currentTimeMillis,
) : VibeDevScreenModel(source, live, clock) {
    private val mutable = MutableStateFlow(
        VibeDevDiffData(files = files, index = files.indexOfFirst { it.path == startPath }.coerceAtLeast(0)),
    )
    val data: StateFlow<VibeDevDiffData> = mutable.asStateFlow()
    private val cache = mutableMapOf<String, VibeDevFilePatch>()

    override suspend fun load() {
        val path = mutable.value.file?.path ?: return
        val changes = source().changes(runId, turn, path)
        // Previous/Next to a cached file does not cancel this read, so the
        // reader may be on another file by now: anchor on the file showing
        // when the read lands, not the one it was asked for.
        val viewing = mutable.value.file?.path
        val listed = changes.files.isEmpty() || changes.files.any { it.path == path }
        if (listed) cache[path] = changes.patch ?: VibeDevFilePatch(path, "") else cache.remove(path)
        if (changes.files.isNotEmpty()) {
            // The list can change under the reader (a new turn, a reverted
            // file): stay on the same file by path, not by position.
            val anchored = changes.files.indexOfFirst { it.path == viewing }
            when {
                anchored >= 0 -> mutable.value = mutable.value.copy(files = changes.files, index = anchored)
                viewing == path -> {
                    mutable.value = mutable.value.copy(
                        files = changes.files,
                        index = mutable.value.index.coerceAtMost(changes.files.lastIndex),
                        patch = null, diff = null, collapsed = emptySet(), unfolded = emptySet(),
                    )
                    throw IllegalStateException(fileGone(path))
                }
                // The file showing may have gone too, but this read was not
                // for it: leave it as it is; its own next read says so.
                else -> return
            }
        }
        if (mutable.value.file?.path == path) cache[path]?.let(::show)
    }

    private fun show(patch: VibeDevFilePatch) {
        mutable.value = mutable.value.copy(
            patch = patch,
            diff = UnifiedDiffParser.parse(patch.unifiedDiff),
        )
    }

    fun step(by: Int) {
        val target = (mutable.value.index + by).coerceIn(0, (mutable.value.files.size - 1).coerceAtLeast(0))
        if (target == mutable.value.index) return
        mutable.value = mutable.value.copy(index = target, patch = null, diff = null, collapsed = emptySet(), unfolded = emptySet())
        val cached = mutable.value.file?.path?.let(cache::get)
        if (cached != null) show(cached) else reload()
    }

    fun toggleHunk(index: Int) {
        val collapsed = mutable.value.collapsed
        mutable.value = mutable.value.copy(collapsed = if (index in collapsed) collapsed - index else collapsed + index)
    }

    fun unfold(key: String) {
        mutable.value = mutable.value.copy(unfolded = mutable.value.unfolded + key)
    }

    companion object {
        internal fun fileGone(path: String) = "${path.substringAfterLast('/')} is no longer among this run's changes."
    }
}
