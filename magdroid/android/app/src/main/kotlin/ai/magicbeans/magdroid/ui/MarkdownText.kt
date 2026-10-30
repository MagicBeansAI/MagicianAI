package ai.magicbeans.magdroid.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.fromHtml
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import org.commonmark.node.*
import org.commonmark.parser.Parser
import org.commonmark.renderer.html.HtmlRenderer

private val markdownParser: Parser = Parser.builder().build()
private val markdownRenderer: HtmlRenderer = HtmlRenderer.builder().build()

/** HTML handles inline formatting; native layout owns block spacing and lists. */
internal fun markdownHtml(markdown: String): String =
    markdownRenderer.render(markdownParser.parse(markdown))

private fun Node.children(): List<Node> = generateSequence(firstChild) { it.next }.toList()

internal fun markdownBlocks(markdown: String): List<Node> = markdownParser.parse(markdown).children()

/** Render an HTML-only task summary as inert native Markdown, not source code. */
internal fun taskSummaryMarkdown(text: String): String {
    var html = text.trim()
    val options = setOf(RegexOption.IGNORE_CASE, RegexOption.DOT_MATCHES_ALL)
    val fence = Regex("^```(?:html|htm)[ \\t]*\\r?\\n", options).find(html)
    if (fence != null && html.endsWith("```")) {
        html = html.substring(fence.range.last + 1).dropLast(3).trim()
        if (Regex("(?m)^```").containsMatchIn(html)) return text
    } else if (!html.startsWith("<")) return text
    if (!Regex("""^\s*(?:<!doctype\b[^>]*>\s*)?<(?:html|body|aside|article|section|div|h[1-6]|p|ul|ol|table)\b""", options).containsMatchIn(html)) return text
    fun replace(pattern: String, transform: (MatchResult) -> CharSequence) {
        html = Regex(pattern, options).replace(html, transform)
    }
    replace("""<!--.*?-->|<(?:script|style|head)\b[^>]*>.*?</(?:script|style|head)\s*>""") { "" }
    replace("""<h([1-6])\b[^>]*>""") { "\n\n" + "#".repeat(it.groupValues[1].toInt()) + " " }
    replace("""</h[1-6]\s*>|</?(?:p|div|section|article|aside|ul|ol|blockquote|table)\b[^>]*>""") { "\n\n" }
    replace("""<li\b[^>]*>""") { "\n- " }
    replace("""</li\s*>""") { "" }
    replace("""<br\s*/?>|</tr\s*>""") { "  \n" }
    replace("""</?(?:strong|b)\b[^>]*>""") { "**" }
    replace("""</?(?:em|i)\b[^>]*>""") { "*" }
    replace("""</?code\b[^>]*>""") { "`" }
    replace("""<a\b[^>]*href\s*=\s*["'](https?://[^"']+|mailto:[^"']+)["'][^>]*>(.*?)</a\s*>""") { "[${it.groupValues[2]}](${it.groupValues[1]})" }
    replace("""</t[dh]\s*>""") { " · " }
    replace("""<[^>]*>""") { "" }
    val entities = mapOf("amp" to "&", "lt" to "<", "gt" to ">", "quot" to "\"", "apos" to "'", "nbsp" to " ", "mdash" to "—", "ndash" to "–", "hellip" to "…", "rsquo" to "’", "lsquo" to "‘", "rdquo" to "”", "ldquo" to "“", "bull" to "•")
    html = Regex("&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);").replace(html) {
        val entity = it.groupValues[1]
        val point = if (entity.startsWith("#x")) entity.drop(2).toIntOrNull(16) else entity.removePrefix("#").toIntOrNull()
        entities[entity] ?: point?.takeIf { p -> Character.isValidCodePoint(p) && p !in 0xD800..0xDFFF }
            ?.let { p -> String(Character.toChars(p)) } ?: it.value
    }
    return html.replace(Regex("\\n[ \\t]+\\n"), "\n\n").replace(Regex("\\n{3,}"), "\n\n").trim()
}

/**
 * The HTML round trip ends every paragraph and list with line breaks, so a
 * bubble carried a blank line or two under its last line (iOS has none). Trim
 * trailing whitespace and breaks; inner paragraph spacing is untouched.
 */
internal fun trimTrailingBreaks(text: AnnotatedString): AnnotatedString {
    var end = text.length
    while (end > 0 && text[end - 1].isWhitespace()) end -= 1
    return if (end == text.length) text else text.subSequence(0, end)
}

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

/** Keep paragraphs, headings and list items separate: HTML-to-text loses their layout. */
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
    val blocks = remember(markdown) { markdownBlocks(markdown) }
    MarkdownBlocks(blocks, color, fontSize, lineHeight, style, find, modifier)
}

@Composable
private fun MarkdownBlocks(
    blocks: List<Node>, color: Color, fontSize: TextUnit, lineHeight: TextUnit,
    style: TextStyle, find: String, modifier: Modifier = Modifier,
) {
    Column(modifier) {
        blocks.forEachIndexed { index, block ->
            if (index > 0) {
                Spacer(Modifier.height(when {
                    block is Heading -> 16.dp
                    blocks[index - 1] is Heading -> 8.dp
                    else -> 12.dp
                }))
            }
            when (block) {
                is Heading -> {
                    val scale = when (block.level) { 1 -> 1.6f; 2 -> 1.4f; 3 -> 1.2f; else -> 1.05f }
                    MarkdownInline(
                        block, color, fontSize * scale, lineHeight * scale,
                        style.copy(fontWeight = FontWeight.SemiBold), find,
                        Modifier.semantics { heading() },
                    )
                }
                is BulletList, is OrderedList -> {
                    val start = (block as? OrderedList)?.markerStartNumber ?: 1
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        block.children().forEachIndexed { itemIndex, item ->
                            Row {
                                Text(
                                    if (block is OrderedList) "${start + itemIndex}." else "•",
                                    color = color, fontSize = fontSize, lineHeight = lineHeight,
                                    style = style.copy(platformStyle = PlatformTextStyle(includeFontPadding = false)),
                                    modifier = Modifier.widthIn(min = 18.dp).padding(end = 6.dp),
                                )
                                MarkdownBlocks(item.children(), color, fontSize, lineHeight, style, find, Modifier.weight(1f))
                            }
                        }
                    }
                }
                is BlockQuote -> Row(Modifier.height(IntrinsicSize.Min)) {
                    Box(Modifier.width(3.dp).fillMaxHeight().background(color.copy(alpha = 0.25f)))
                    MarkdownBlocks(block.children(), color, fontSize, lineHeight, style, find, Modifier.padding(start = 10.dp).weight(1f))
                }
                is FencedCodeBlock -> MarkdownCode(block.literal, color, fontSize, lineHeight, style)
                is IndentedCodeBlock -> MarkdownCode(block.literal, color, fontSize, lineHeight, style)
                is ThematicBreak -> HorizontalDivider(color = color.copy(alpha = 0.2f))
                else -> MarkdownInline(block, color, fontSize, lineHeight, style, find)
            }
        }
    }
}

@Composable
private fun MarkdownCode(code: String, color: Color, fontSize: TextUnit, lineHeight: TextUnit, style: TextStyle) {
    Text(
        code.trimEnd('\n'), color = color, fontSize = fontSize, lineHeight = lineHeight,
        style = style.copy(fontFamily = LocalMagicanFontFamilies.current.mono),
        modifier = Modifier.fillMaxWidth().background(color.copy(alpha = 0.06f), RoundedCornerShape(6.dp))
            .horizontalScroll(rememberScrollState()).padding(10.dp),
    )
}

@Composable
private fun MarkdownInline(
    node: Node, color: Color, fontSize: TextUnit, lineHeight: TextUnit,
    style: TextStyle, find: String, modifier: Modifier = Modifier,
) {
    val uriHandler = androidx.compose.ui.platform.LocalUriHandler.current
    val linkColor = Coral
    val annotated = remember(node, find, color, linkColor, uriHandler) {
        // Heading size and margins belong to the block layout, not Android's
        // HTML heading spans. Rendering just the children retains inline styles.
        val html = if (node is Heading) node.children().joinToString("") { markdownRenderer.render(it) }
            else markdownRenderer.render(node)
        val parsed = trimTrailingBreaks(AnnotatedString.fromHtml(
            html.replace("<code>", "<tt>").replace("</code>", "</tt>"),
            linkInteractionListener = androidx.compose.ui.text.LinkInteractionListener { link ->
                if (link is androidx.compose.ui.text.LinkAnnotation.Url) uriHandler.openUri(link.url)
            },
            linkStyles = TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)),
        ))
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
        style = style.copy(platformStyle = PlatformTextStyle(includeFontPadding = false)),
    )
}
