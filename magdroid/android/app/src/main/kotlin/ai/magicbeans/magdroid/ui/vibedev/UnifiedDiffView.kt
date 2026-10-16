package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.DiffLine
import ai.magicbeans.magdroid.vibedev.DiffLineKind
import ai.magicbeans.magdroid.vibedev.DiffRow
import ai.magicbeans.magdroid.vibedev.UnifiedDiff
import ai.magicbeans.magdroid.vibedev.UnifiedDiffParser
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Fill behind an added or removed line: the success/danger token at low alpha. */
private const val TINT_ALPHA = 0.12f

/** One row of the diff as the list draws it. */
private sealed class DiffItem {
    abstract val key: String
    data class Header(val hunkIndex: Int, val text: String, val closed: Boolean) : DiffItem() {
        override val key get() = "h:$hunkIndex"
    }
    data class Line(override val key: String, val line: DiffLine) : DiffItem()
    data class Fold(override val key: String, val count: Int) : DiffItem()
}

/**
 * The rows to draw, flat: each hunk's header, then (unless folded shut) its
 * lines, with long unchanged runs folded unless the reader opened them.
 */
private fun diffItems(
    hunkRows: List<List<DiffRow>>,
    diff: UnifiedDiff,
    collapsed: Set<Int>,
    unfolded: Set<String>,
): List<DiffItem> = buildList {
    diff.hunks.forEachIndexed { hunkIndex, hunk ->
        val closed = hunkIndex in collapsed
        add(DiffItem.Header(hunkIndex, hunk.header, closed))
        if (closed) return@forEachIndexed
        hunkRows[hunkIndex].forEachIndexed { rowIndex, row ->
            val key = "$hunkIndex:$rowIndex"
            when (row) {
                is DiffRow.Line -> add(DiffItem.Line("l:$key", row.line))
                is DiffRow.Fold -> if (key in unfolded) {
                    row.lines.forEachIndexed { index, line -> add(DiffItem.Line("l:$key:$index", line)) }
                } else {
                    add(DiffItem.Fold(key, row.count))
                }
            }
        }
    }
}

/**
 * A unified diff: hunks with old and new line numbers, added and removed lines
 * tinted with the theme's success and danger tokens at low alpha and marked in
 * full strength in the gutter, the mono font role, collapsible hunks, long
 * unchanged runs folded, and a wrap toggle (off scrolls sideways).
 *
 * Lazy: a patch can run to thousands of lines, and only the rows on screen
 * are composed. Without wrap every line is as wide as the widest one, which
 * is measured once (a few of the longest lines, truncated) instead of asking
 * every row for its intrinsic width; rows are capped at [MAX_ROW_WIDTH_PX]
 * and a line longer than that wraps inside its row. In both modes a line
 * past [MAX_LINE_CHARS] shows its start and how many characters are left.
 *
 * Reusable: it takes a parsed [UnifiedDiff] and nothing VibeDev-specific.
 */
@Composable
fun UnifiedDiffView(
    diff: UnifiedDiff,
    modifier: Modifier = Modifier,
    wrap: Boolean = true,
    collapsed: Set<Int> = emptySet(),
    unfolded: Set<String> = emptySet(),
    onToggleHunk: (Int) -> Unit = {},
    onUnfold: (String) -> Unit = {},
) {
    val mono = LocalMagicanFontFamilies.current.mono
    val density = LocalDensity.current
    val digits = remember(diff) {
        diff.hunks.maxOfOrNull { maxOf(it.oldStart + it.oldCount, it.newStart + it.newCount) }?.toString()?.length ?: 1
    }
    // Line numbers are text: their column grows with the font scale.
    val numberWidth = (digits * 7 * density.fontScale + 8).dp
    val hunkRows = remember(diff) { diff.hunks.map { UnifiedDiffParser.rows(it) } }
    val rows = remember(diff, collapsed, unfolded) { diffItems(hunkRows, diff, collapsed, unfolded) }
    val measurer = rememberTextMeasurer()
    // Without wrap: the widest line's text, measured once per diff, and how
    // many characters fit the capped row. Nothing is measured with wrap on.
    val noWrap = remember(diff, mono, density, wrap, numberWidth) {
        if (wrap) return@remember null
        val style = TextStyle(fontFamily = mono, fontSize = 12.sp, fontWeight = FontWeight.Medium)
        fun width(text: String) = measurer.measure(text, style = style, softWrap = false, maxLines = 1).size.width
        val gutterPx = with(density) { (3.dp + numberWidth * 2 + 14.dp).toPx() }
        val textCapPx = (MAX_ROW_WIDTH_PX - gutterPx).coerceAtLeast(0f)
        // Mono: one character's width says how many fit.
        val charPx = (width("0".repeat(10)) / 10f).coerceAtLeast(1f)
        val maxChars = (textCapPx / charPx).toInt().coerceAtLeast(MIN_NO_WRAP_CHARS)
        // Truncated before measuring: a minified line can run to megabytes,
        // and anything past the cap wraps inside the row anyway.
        val widest = diff.hunks.asSequence().flatMap { it.lines.asSequence() }
            .map { it to noWrapChars(it.text) }
            .sortedByDescending { it.second }
            .take(WIDEST_MEASURED)
            .maxOfOrNull { (line, _) -> width(" ${line.text.take(maxChars)}") } ?: 0
        NoWrapLayout(with(density) { minOf(widest.toFloat(), textCapPx).toDp() }, maxChars)
    }
    val maxRowWidth = with(density) { MAX_ROW_WIDTH_PX.toDp() }
    val horizontal = rememberScrollState()
    BoxWithConstraints(modifier) {
        // Gutter bar, two number columns, the text's padding, and the text,
        // capped: Compose cannot represent a width past 262,143 px.
        val rowWidth = minOf(maxOf(maxWidth, 360.dp, 3.dp + numberWidth * 2 + 14.dp + (noWrap?.textWidth ?: 0.dp)), maxOf(maxWidth, maxRowWidth))
        LazyColumn(Modifier.fillMaxSize()) {
            items(rows, key = { it.key }, contentType = { it::class }) { item ->
                when (item) {
                    is DiffItem.Header -> Text(
                        (if (item.closed) "▸ " else "▾ ") + item.text,
                        Modifier.fillMaxWidth()
                            .heightIn(min = 48.dp)
                            .background(activePalette.soft)
                            .clickable(role = Role.Button, onClickLabel = if (item.closed) "Expand hunk" else "Collapse hunk") { onToggleHunk(item.hunkIndex) }
                            .wrapContentHeight()
                            .semantics { stateDescription = if (item.closed) "Collapsed" else "Expanded" }
                            .padding(horizontal = 10.dp, vertical = 8.dp),
                        color = activePalette.secondaryText, fontSize = 12.sp, fontFamily = mono,
                        maxLines = if (wrap) 3 else 1,
                    )
                    is DiffItem.Line -> {
                        val shown = remember(item.line.text) { shownLine(item.line.text) }
                        if (noWrap == null) {
                            DiffLineRow(item.line, shown, numberWidth, wrap = true, Modifier.fillMaxWidth())
                        } else {
                            // Every row shares one sideways scroll, so the lines move together.
                            // A line longer than the capped row wraps inside it.
                            val tooLong = noWrapChars(shown.text) > noWrap.maxChars
                            Box(Modifier.fillMaxWidth().horizontalScroll(horizontal)) {
                                DiffLineRow(item.line, shown, numberWidth, wrap = tooLong, Modifier.width(rowWidth))
                            }
                        }
                    }
                    is DiffItem.Fold -> Text(
                        "${item.count} unchanged lines",
                        Modifier.fillMaxWidth()
                            .heightIn(min = 48.dp)
                            .clickable(role = Role.Button, onClickLabel = "Show unchanged lines") { onUnfold(item.key) }
                            .wrapContentHeight()
                            .padding(vertical = 6.dp),
                        color = activePalette.accent, fontSize = 12.sp, fontFamily = mono,
                        textAlign = TextAlign.Center,
                    )
                }
            }
        }
    }
}

/** How many of the longest lines (by characters) are measured for the no-wrap width. */
private const val WIDEST_MEASURED = 4

/**
 * The widest a no-wrap row gets, in px. Compose throws past 262,143 px (about
 * 12k characters of mono text); nothing wider than this reads sideways anyway.
 */
private const val MAX_ROW_WIDTH_PX = 16_000f

/** The fewest characters a capped no-wrap row holds before a line wraps. */
private const val MIN_NO_WRAP_CHARS = 80

/** The no-wrap row's text width and how many characters fit it. */
private data class NoWrapLayout(val textWidth: androidx.compose.ui.unit.Dp, val maxChars: Int)

/** A line's width in mono characters, a tab counted as four. */
internal fun noWrapChars(text: String): Int = text.length + text.count { it == '\t' } * 3

/**
 * The most characters of one line a row draws. A minified file's single line
 * can run to the whole 256 KB patch; wrapped, it is taller than the 262,143 px
 * Compose can represent and the screen throws. 4,000 characters stay under a
 * fifth of that at the narrowest phone, the largest font scale and the
 * densest screen, and the rest is counted instead of drawn.
 */
internal const val MAX_LINE_CHARS = 4_000

/** A line as a row draws it: its start, and how many characters are left off. */
internal data class ShownLine(val text: String, val hidden: Int)

/** [text] cut to [maxChars], never between the two halves of a surrogate pair. */
internal fun shownLine(text: String, maxChars: Int = MAX_LINE_CHARS): ShownLine {
    if (text.length <= maxChars) return ShownLine(text, 0)
    val end = if (maxChars > 0 && text[maxChars - 1].isHighSurrogate()) maxChars - 1 else maxChars
    return ShownLine(text.substring(0, end), text.length - end)
}

/** What a row says in place of the characters it leaves off. */
internal fun hiddenCharsLabel(hidden: Int): String =
    "… ${"%,d".format(java.util.Locale.ROOT, hidden)} more character${if (hidden == 1) "" else "s"}"

@Composable
private fun DiffLineRow(
    line: DiffLine,
    shown: ShownLine,
    numberWidth: androidx.compose.ui.unit.Dp,
    wrap: Boolean,
    modifier: Modifier,
) {
    val mono = LocalMagicanFontFamilies.current.mono
    val mark = when (line.kind) {
        DiffLineKind.Added -> activePalette.success
        DiffLineKind.Removed -> activePalette.danger
        else -> null
    }
    val marker = when (line.kind) {
        DiffLineKind.Added -> "+"
        DiffLineKind.Removed -> "−"
        DiffLineKind.Note -> "\\"
        DiffLineKind.Context -> " "
    }
    val secondary = activePalette.secondaryText
    val text = remember(marker, shown, secondary) {
        buildAnnotatedString {
            append(marker)
            append(shown.text)
            if (shown.hidden > 0) withStyle(SpanStyle(color = secondary)) { append("\n" + hiddenCharsLabel(shown.hidden)) }
        }
    }
    Row(
        // The tint and gutter bar are drawn behind the row, not laid out at
        // the row's intrinsic height: that pinned the row to a fixed height
        // taken from the text, and a tall enough wrapped line threw.
        modifier.heightIn(min = 18.dp)
            // One spoken item per line: what changed, where, and the text.
            .clearAndSetSemantics { contentDescription = diffLineDescription(line, shown) }
            .then(
                if (mark != null) Modifier.drawBehind {
                    drawRect(mark.copy(alpha = TINT_ALPHA))
                    val bar = 3.dp.toPx()
                    val x = if (layoutDirection == LayoutDirection.Rtl) size.width - bar else 0f
                    drawRect(mark, topLeft = Offset(x, 0f), size = Size(bar, size.height))
                } else Modifier,
            ),
    ) {
        Spacer(Modifier.width(3.dp))
        LineNumber(line.oldNumber, numberWidth)
        LineNumber(line.newNumber, numberWidth)
        Text(
            text,
            Modifier.padding(start = 6.dp, end = 8.dp).then(if (wrap) Modifier.weight(1f) else Modifier),
            color = if (line.kind == DiffLineKind.Note) activePalette.secondaryText else activePalette.text,
            fontSize = 12.sp, lineHeight = 17.sp, fontFamily = mono,
            fontWeight = if (mark != null) FontWeight.Medium else FontWeight.Normal,
            softWrap = wrap,
        )
    }
}

@Composable
private fun LineNumber(number: Int?, width: androidx.compose.ui.unit.Dp) {
    Text(
        number?.toString().orEmpty(),
        Modifier.width(width).padding(end = 4.dp),
        color = activePalette.secondaryText.copy(alpha = 0.75f),
        fontSize = 11.sp, lineHeight = 17.sp,
        fontFamily = LocalMagicanFontFamilies.current.mono,
        textAlign = TextAlign.End,
    )
}

/** What TalkBack says for a diff line: the shown part, and how much is left off. */
internal fun diffLineDescription(line: DiffLine, shown: ShownLine = shownLine(line.text)): String {
    val text = if (shown.hidden > 0) "${shown.text} ${hiddenCharsLabel(shown.hidden)}" else shown.text
    return when (line.kind) {
        DiffLineKind.Added -> "Added, line ${line.newNumber ?: ""}: $text"
        DiffLineKind.Removed -> "Removed, line ${line.oldNumber ?: ""}: $text"
        DiffLineKind.Context -> "Line ${line.newNumber ?: line.oldNumber ?: ""}: $text"
        DiffLineKind.Note -> text
    }
}
