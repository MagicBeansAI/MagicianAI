package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.StructuredBlock
import ai.magicbeans.magdroid.chat.StructuredResponse
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.font.FontWeight
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ChatReplyRenderingTest {
    @get:Rule val compose = createComposeRule()

    @Test fun structuredMarkdownRendersFormattingInsteadOfSyntax() {
        compose.setContent {
            MaterialTheme {
                StructuredResponseView(StructuredResponse(blocks = listOf(StructuredBlock(
                    kind = "markdown", text = "**My suggested route:** try [Vendor](https://example.com/).\n\n- Minimum: **50+ MOQ**\n- Contact: **+91 93217 45330**",
                ))))
            }
        }
        val rendered = compose.onNodeWithText("My suggested route:", substring = true)
            .fetchSemanticsNode().config[SemanticsProperties.Text].single()
        assertFalse(rendered.text.contains("**"))
        assertFalse(rendered.text.contains("[Vendor]("))
        assertTrue(rendered.spanStyles.any { it.item.fontWeight == FontWeight.Bold })
        compose.onNodeWithText("+91 93217 45330", substring = true).assertExists()
        compose.onNodeWithText("50+ MOQ", substring = true).assertExists()
        compose.onNodeWithText("**50+ MOQ**", substring = true).assertDoesNotExist()
    }

    @Test fun paragraphsHeadingsAndListItemsHaveDistinctLayout() {
        compose.setContent {
            MaterialTheme {
                StructuredResponseView(StructuredResponse(blocks = listOf(StructuredBlock(
                    kind = "markdown", text = "First paragraph.\n\nSecond paragraph.\n\n### Stickers\n\n- **Printigly** — First supplier.\n- **Printo** — Second supplier.",
                ))))
            }
        }
        val first = compose.onNodeWithText("First paragraph.").fetchSemanticsNode()
        val second = compose.onNodeWithText("Second paragraph.").fetchSemanticsNode()
        val heading = compose.onNodeWithText("Stickers").fetchSemanticsNode()
        val item = compose.onNodeWithText("Printigly", substring = true).fetchSemanticsNode()
        assertTrue(second.boundsInRoot.top > first.boundsInRoot.bottom)
        assertTrue(heading.boundsInRoot.top > second.boundsInRoot.bottom)
        assertTrue(item.boundsInRoot.top > heading.boundsInRoot.bottom)
        assertTrue(heading.config.contains(SemanticsProperties.Heading))
        assertTrue(compose.onAllNodesWithText("•").fetchSemanticsNodes().size == 2)

        fun textLayout(text: String): TextLayoutResult {
            val results = mutableListOf<TextLayoutResult>()
            compose.onNodeWithText(text).performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(results) }
            return results.single()
        }
        assertTrue(textLayout("Stickers").layoutInput.style.fontSize.value > textLayout("First paragraph.").layoutInput.style.fontSize.value)
    }

    @Test fun runSummaryShowsOnlyVerificationReasonInPopupAndKeepsMarkdownBody() {
        val reason = "figure 93217, 45330 not found in any tool result"
        compose.setContent {
            MaterialTheme {
                TaskAnswerSummary("> ⚠️ **Not fully verified.** Magician's evidence check could not confirm part of this answer: $reason\n\n```html\n<h2>Suppliers</h2><p>First paragraph.</p><p>Second paragraph.</p><ul><li>Contact: <strong>+91 93217 45330</strong></li></ul>\n```")
            }
        }
        compose.onNodeWithText(reason).assertDoesNotExist()
        val heading = compose.onNodeWithText("Suppliers").fetchSemanticsNode()
        assertTrue(heading.config.contains(SemanticsProperties.Heading))
        val first = compose.onNodeWithText("First paragraph.").fetchSemanticsNode()
        val second = compose.onNodeWithText("Second paragraph.").fetchSemanticsNode()
        assertTrue(second.boundsInRoot.top > first.boundsInRoot.bottom)
        compose.onNodeWithText("+91 93217 45330", substring = true).assertExists()
        compose.onNodeWithContentDescription("Not fully verified — view details").performClick()
        compose.onNodeWithText(reason).assertExists()
        compose.onNodeWithText("Not fully verified").assertExists()
    }

    @Test fun guideSearchShowsMatchingInstructionsAndResetsEmptyResults() {
        compose.setContent { MaterialTheme { HowToUseSettingsPage() } }
        compose.onNode(hasSetTextAction()).performTextInput("thinking")
        compose.onNodeWithText("Set thinking effort").assertExists()
        compose.onNodeWithText("Auto uses that model or engine’s default", substring = true).assertExists()
        compose.onNodeWithContentDescription("Clear search").performClick()
        compose.onNode(hasSetTextAction()).performTextInput("no-such-help-topic")
        compose.onNodeWithText("No matching topics.", substring = true).assertExists()
        compose.onNodeWithText("Show all topics").performClick()
        compose.onNodeWithText("Find your way around").assertExists()
    }
}
