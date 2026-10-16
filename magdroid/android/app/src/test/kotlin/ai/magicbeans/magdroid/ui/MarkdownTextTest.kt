package ai.magicbeans.magdroid.ui

import androidx.compose.ui.text.AnnotatedString
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MarkdownTextTest {
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
