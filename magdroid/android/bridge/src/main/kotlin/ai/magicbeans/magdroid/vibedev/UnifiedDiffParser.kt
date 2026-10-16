package ai.magicbeans.magdroid.vibedev

/**
 * A unified diff, parsed for display: hunks of lines with their old and new
 * line numbers. Pure, so the diff view (and MUIJ's `DiffViewer` later) draws
 * from tested data rather than from string slicing inside a composable.
 */
data class UnifiedDiff(
    /** `diff --git`, `index`, `---`, `+++` and similar lines before the first hunk. */
    val header: List<String>,
    val hunks: List<DiffHunk>,
) {
    val additions: Int get() = hunks.sumOf { hunk -> hunk.lines.count { it.kind == DiffLineKind.Added } }
    val deletions: Int get() = hunks.sumOf { hunk -> hunk.lines.count { it.kind == DiffLineKind.Removed } }
    val isEmpty: Boolean get() = hunks.isEmpty()
}

data class DiffHunk(
    /** The whole `@@ -a,b +c,d @@ section` line. */
    val header: String,
    val oldStart: Int,
    val oldCount: Int,
    val newStart: Int,
    val newCount: Int,
    /** Text after the second `@@`, usually the enclosing function. */
    val section: String,
    val lines: List<DiffLine>,
)

enum class DiffLineKind { Context, Added, Removed, Note }

data class DiffLine(
    val kind: DiffLineKind,
    /** The line without its `+`/`-`/space marker. */
    val text: String,
    val oldNumber: Int?,
    val newNumber: Int?,
)

/** A row of a hunk as drawn: a line, or a run of unchanged lines folded away. */
sealed class DiffRow {
    data class Line(val line: DiffLine) : DiffRow()
    data class Fold(val count: Int, val lines: List<DiffLine>) : DiffRow()
}

object UnifiedDiffParser {
    private val HUNK = Regex("""^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@ ?(.*)$""")

    fun parse(text: String): UnifiedDiff {
        val header = mutableListOf<String>()
        val hunks = mutableListOf<DiffHunk>()
        var current: MutableList<DiffLine>? = null
        var start: MatchResult? = null
        var oldLine = 0
        var newLine = 0

        fun close() {
            val match = start ?: return
            val lines = current ?: return
            hunks += DiffHunk(
                header = match.value,
                oldStart = match.groupValues[1].toInt(),
                oldCount = match.groupValues[2].ifEmpty { "1" }.toInt(),
                newStart = match.groupValues[3].toInt(),
                newCount = match.groupValues[4].ifEmpty { "1" }.toInt(),
                section = match.groupValues[5].trim(),
                lines = lines.toList(),
            )
            start = null
            current = null
        }

        val rows = text.replace("\r\n", "\n").split('\n')
        // A diff ending in a newline splits into a trailing empty string,
        // which is not a context line.
        val count = if (rows.lastOrNull() == "") rows.size - 1 else rows.size
        for (index in 0 until count) {
            val raw = rows[index]
            val hunk = HUNK.find(raw)
            if (raw.startsWith("@@") && hunk != null) {
                close()
                start = hunk
                current = mutableListOf()
                oldLine = hunk.groupValues[1].toInt()
                newLine = hunk.groupValues[3].toInt()
                continue
            }
            val lines = current
            if (lines == null || raw.startsWith("diff --git")) {
                if (raw.startsWith("diff --git")) close()
                header += raw
                continue
            }
            when {
                raw.startsWith("+") -> { lines += DiffLine(DiffLineKind.Added, raw.substring(1), null, newLine); newLine++ }
                raw.startsWith("-") -> { lines += DiffLine(DiffLineKind.Removed, raw.substring(1), oldLine, null); oldLine++ }
                raw.startsWith("\\") -> lines += DiffLine(DiffLineKind.Note, raw.removePrefix("\\").trim(), null, null)
                else -> {
                    lines += DiffLine(DiffLineKind.Context, raw.removePrefix(" "), oldLine, newLine)
                    oldLine++; newLine++
                }
            }
        }
        close()
        return UnifiedDiff(header, hunks)
    }

    /**
     * A hunk's rows with long unchanged stretches folded: [keep] context lines
     * stay beside each change and anything longer between them becomes one
     * "N unchanged lines" row.
     */
    fun rows(hunk: DiffHunk, keep: Int = 3, minimumFold: Int = 4): List<DiffRow> {
        val lines = hunk.lines
        val rows = mutableListOf<DiffRow>()
        var index = 0
        while (index < lines.size) {
            if (lines[index].kind != DiffLineKind.Context) {
                rows += DiffRow.Line(lines[index]); index++; continue
            }
            var end = index
            while (end < lines.size && lines[end].kind == DiffLineKind.Context) end++
            val run = lines.subList(index, end)
            val leading = if (index == 0) 0 else keep
            val trailing = if (end == lines.size) 0 else keep
            val hidden = run.size - leading - trailing
            if (hidden >= minimumFold) {
                run.take(leading).forEach { rows += DiffRow.Line(it) }
                rows += DiffRow.Fold(hidden, run.subList(leading, leading + hidden).toList())
                run.takeLast(trailing).forEach { rows += DiffRow.Line(it) }
            } else {
                run.forEach { rows += DiffRow.Line(it) }
            }
            index = end
        }
        return rows
    }
}
