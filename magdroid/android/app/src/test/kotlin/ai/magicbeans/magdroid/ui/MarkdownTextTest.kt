package ai.magicbeans.magdroid.ui

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
}
