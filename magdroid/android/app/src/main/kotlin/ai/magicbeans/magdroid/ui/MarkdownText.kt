package ai.magicbeans.magdroid.ui

import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.fromHtml
import androidx.compose.ui.unit.TextUnit
import org.commonmark.parser.Parser
import org.commonmark.renderer.html.HtmlRenderer

private val markdownParser: Parser = Parser.builder().build()
private val markdownRenderer: HtmlRenderer = HtmlRenderer.builder().build()

/** CommonMark HTML is the stable seam between the parser and Compose's styled text. */
internal fun markdownHtml(markdown: String): String =
    markdownRenderer.render(markdownParser.parse(markdown))

/**
 * Native formatted chat text, matching iOS's Markdown-backed bubbles and cards.
 *
 * Parsing is remembered by content so a normal recomposition does no work;
 * streaming replies are reparsed only when another text token actually arrives.
 */
@Composable
internal fun noteMatchCount(text: String, query: String): Int {
    val needle = query.trim()
    if (needle.isEmpty()) return 0
    val haystack = text.lowercase()
    val look = needle.lowercase()
    var count = 0
    var from = 0
    while (true) {
        val at = haystack.indexOf(look, from)
        if (at < 0) return count
        count += 1
        from = at + look.length
    }
}

@Composable
internal fun MarkdownText(
    markdown: String,
    color: Color,
    fontSize: TextUnit,
    lineHeight: TextUnit,
    modifier: Modifier = Modifier,
    style: TextStyle = TextStyle.Default,
    find: String = "",
) {
    val annotated = remember(markdown, find) {
        val parsed = AnnotatedString.fromHtml(markdownHtml(markdown))
        val needle = find.trim()
        if (needle.isEmpty()) return@remember parsed
        val builder = AnnotatedString.Builder(parsed)
        val haystack = parsed.text.lowercase()
        val look = needle.lowercase()
        var from = 0
        while (true) {
            val at = haystack.indexOf(look, from)
            if (at < 0) break
            builder.addStyle(SpanStyle(background = color.copy(alpha = 0.28f)), at, at + look.length)
            from = at + look.length
        }
        builder.toAnnotatedString()
    }
    Text(
        text = annotated,
        color = color,
        fontSize = fontSize,
        lineHeight = lineHeight,
        modifier = modifier,
        style = style,
    )
}
