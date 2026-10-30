package ai.magicbeans.magdroid.ui

import androidx.compose.ui.text.AnnotatedString
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MarkdownTextTest {
    @Test
    fun `HTML-only task summary retains document structure without executing content`() {
        val source = "```html\n<h1>Morning briefing &mdash; incomplete</h1><p><strong>No validated summary.</strong></p><h2>Findings</h2><ul><li>All 9 agents were idle.</li><li>No <code>daily_ops.md</code> state.</li></ul><script>alert('no')</script>\n```"
        val markdown = taskSummaryMarkdown(source)
        val html = markdownHtml(markdown)
        assertTrue(html.contains("<h1>Morning briefing — incomplete</h1>"))
        assertTrue(html.contains("<strong>No validated summary.</strong>"))
        assertTrue(html.contains("<h2>Findings</h2>"))
        assertTrue(html.contains("<li>All 9 agents were idle.</li>"))
        assertTrue(html.contains("<code>daily_ops.md</code>"))
        assertTrue(!markdown.contains("<h1>") && !markdown.contains("alert("))
        val example = "Use this example:\n\n```html\n<h1>Keep this code</h1>\n```"
        assertEquals(example, taskSummaryMarkdown(example))
    }

    @Test
    fun `an unverified HTML answer previews its caveat and text, not markup`() {
        val source = "<aside data-magician-caveat=\"unverified\" role=\"note\" style=\"padding:10px\"><p style=\"margin:0;\">⚠️ <strong>Not fully verified.</strong> Reason.</p></aside><article><h1>Tea</h1><p>Tea began in China.</p></article>"
        val markdown = taskSummaryMarkdown(source)
        assertTrue(markdown.startsWith("⚠️ **Not fully verified.** Reason."))
        assertTrue(markdown.contains("# Tea") && markdown.contains("Tea began in China."))
        assertTrue(!markdown.contains("<aside") && !markdown.contains("style="))
    }

    @Test
    fun `task summaries retain common markdown structure`() {
        val html = markdownHtml(
            """
            **Observed result**

            - URL: `https://example.com/`
            - Title: *Example*
            """.trimIndent(),
        )

        assertTrue(html.contains("<strong>Observed result</strong>"))
        assertTrue(html.contains("<ul>"))
        assertTrue(html.contains("<code>https://example.com/</code>"))
        assertTrue(html.contains("<em>Example</em>"))
    }

    @Test
    fun `a bubble ends at its last line, with no trailing breaks`() {
        assertEquals("Hello there", trimTrailingBreaks(AnnotatedString("Hello there\n\n")).text)
        assertEquals("One\n\nTwo", trimTrailingBreaks(AnnotatedString("One\n\nTwo\n \n")).text)
        assertEquals("Same", trimTrailingBreaks(AnnotatedString("Same")).text)
        assertEquals("", trimTrailingBreaks(AnnotatedString("\n\n")).text)
    }
}
