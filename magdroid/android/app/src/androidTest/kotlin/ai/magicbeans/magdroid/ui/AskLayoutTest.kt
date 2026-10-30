package ai.magicbeans.magdroid.ui

import android.content.res.Configuration
import ai.magicbeans.magdroid.attention.AlignmentDetailOutcome
import ai.magicbeans.magdroid.attention.AttentionInputSchema
import ai.magicbeans.magdroid.attention.AttentionItem
import ai.magicbeans.magdroid.attention.AttentionMetadata
import ai.magicbeans.magdroid.attention.request
import ai.magicbeans.magdroid.chat.ChatMessage
import ai.magicbeans.magdroid.chat.EscalationCard
import ai.magicbeans.magdroid.chat.EscalationOption
import ai.magicbeans.magdroid.chat.HitlResponseValue
import ai.magicbeans.magdroid.chat.MessageKind
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipeUp
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Synthetic cards only: callbacks never post an approval or contact the backend. */
@RunWith(AndroidJUnit4::class)
class AskLayoutTest {
    @get:Rule val compose = createComposeRule()

    private val prompt = "Approve this action?\n\nArguments:\n" +
        (1..80).joinToString("\n") { "argument_$it: synthetic value" }
    private val options = listOf(
        "allow_once" to "Allow once",
        "session:mcp_tool" to "This session: this action",
        "always:mcp_tool" to "Always: this action",
        "session:capability" to "This session: whole integration with a long account label",
        "always:capability" to "Always: whole integration with a long account label",
        "deny" to "Deny",
    ).map { (id, label) -> EscalationOption(id, label) }

    private fun item(id: String = "one", god: Boolean = false) = AttentionItem(
        id = id, itemType = "request", status = "needs_action", title = "Review MCP call",
        metadata = AttentionMetadata(
            source = "user_request", question = prompt, inputType = "choice", pauseStateId = "request-$id",
            inputSchema = AttentionInputSchema(
                type = "choice", options = options,
                requestType = if (god) "sandbox_override" else "mcp_tool_approval",
                context = buildJsonObject {
                    put("god_mode", god)
                    put("command", "search_products\n$prompt")
                },
            ),
        ),
    )

    @Composable
    private fun Phone(height: Int = 600, content: @Composable () -> Unit) {
        val config = Configuration(LocalConfiguration.current).apply { screenHeightDp = height }
        val density = LocalDensity.current
        CompositionLocalProvider(
            LocalConfiguration provides config,
            LocalDensity provides Density(density.density, fontScale = 1.4f),
        ) { MaterialTheme { content() } }
    }

    private fun assertFullPrompt() {
        val layouts = mutableListOf<TextLayoutResult>()
        compose.onNodeWithText(prompt).performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
        assertTrue(layouts.single().lineCount >= 80)
        assertFalse(layouts.single().isLineEllipsized(layouts.single().lineCount - 1))
    }

    @Test fun feedArgumentsScrollWithoutHidingChoicesOrReviewAndNavigationResetsScroll() {
        val first = item()
        val second = item("two")
        var answered: HitlResponseValue? = null
        var reviewed: String? = null
        compose.setContent { Phone {
            Column(Modifier.width(320.dp).height(480.dp)) {
                ComposerAskDockView(
                    messages = emptyList(), enabled = true, onAnswer = { _, _, _, _ -> }, onOpenAttention = {},
                    feedAsks = listOf(first, second), onAnswerFeed = { _, value -> answered = value },
                    onOpenSheet = { reviewed = it.id }, loadAlignmentDetail = { AlignmentDetailOutcome.Unavailable },
                )
                Text("Composer input")
            }
        } }
        assertFullPrompt()
        compose.onNodeWithText("Composer input").assertIsDisplayed()
        compose.onNodeWithText("Deny").performScrollTo().assertIsDisplayed().performClick()
        compose.runOnIdle { assertEquals(HitlResponseValue.Choice("deny"), answered) }
        compose.onNodeWithTag("composer-ask-review").assertIsDisplayed().performClick()
        compose.runOnIdle { assertEquals(first.id, reviewed) }
        compose.onNodeWithTag("composer-ask-prompt").performTouchInput { swipeUp() }
        assertTrue(compose.onNodeWithTag("composer-ask-prompt").fetchSemanticsNode()
            .config[SemanticsProperties.VerticalScrollAxisRange].value() > 0f)
        compose.onNodeWithContentDescription("Next question").performClick()
        assertEquals(0f, compose.onNodeWithTag("composer-ask-prompt").fetchSemanticsNode()
            .config[SemanticsProperties.VerticalScrollAxisRange].value())
        compose.onNodeWithTag("composer-ask-review").performClick()
        compose.runOnIdle { assertEquals(second.id, reviewed) }
    }

    @Test fun transcriptLongChoicesRemainReachableBesideTheCompleteArguments() {
        val choices = (1..20).map { EscalationOption("option-$it", "Option $it with a longer description to wrap on a phone") }
        val message = ChatMessage(
            id = "message", fromUser = false, text = "", kind = MessageKind.Escalation,
            escalation = EscalationCard(prompt, choices, "transcript-request", null, false, inputType = "choice"),
        )
        var answered: String? = null
        var reviewed: String? = null
        compose.setContent { Phone {
            Column(Modifier.width(320.dp).height(480.dp)) {
                ComposerAskDockView(
                    messages = listOf(message), enabled = true,
                    onAnswer = { _, option, _, _ -> answered = option?.id }, onOpenAttention = { reviewed = it },
                    loadAlignmentDetail = { AlignmentDetailOutcome.Unavailable },
                )
                Text("Composer input")
            }
        } }
        assertFullPrompt()
        compose.onNodeWithText(choices.last().label).performScrollTo().assertIsDisplayed().performClick()
        compose.runOnIdle { assertEquals(choices.last().id, answered) }
        compose.onNodeWithText("Composer input").assertIsDisplayed()
        compose.onNodeWithTag("composer-ask-review").performClick()
        compose.runOnIdle { assertEquals("transcript-request", reviewed) }
    }

    @Test fun focusedChoiceReviewKeepsDecisionsReachableBelowLongArguments() {
        val request = item().let { it.request(it.metadata) }
        var answered: HitlResponseValue? = null
        compose.setContent { Phone {
            AnswerSheet(request, submitting = false, error = null, onDismiss = {},
                onSubmit = { answered = it }, onOpenTask = {})
        } }
        assertFullPrompt()
        compose.onNodeWithText("Deny").performScrollTo().assertIsDisplayed().performClick()
        compose.runOnIdle { assertEquals(HitlResponseValue.Choice("deny"), answered) }
    }

    @Test fun largeFontGodRequestHandsOffToTheFullReview() {
        val row = item(god = true)
        var reviewed: String? = null
        compose.setContent { Phone(height = 480) {
            Column(Modifier.width(320.dp).height(420.dp)) {
                ComposerAskDockView(
                    messages = emptyList(), enabled = true, onAnswer = { _, _, _, _ -> }, onOpenAttention = {},
                    feedAsks = listOf(row), onOpenSheet = { reviewed = it.id },
                    loadAlignmentDetail = { AlignmentDetailOutcome.Unavailable },
                )
                Text("Composer input")
            }
        } }
        compose.onNodeWithText("Composer input").assertIsDisplayed()
        compose.onNodeWithText("Review and decide →").assertIsDisplayed().performClick()
        compose.runOnIdle { assertEquals(row.id, reviewed) }
    }

    @Test fun godReviewRetainsTheWholeToolAlwaysOptionId() {
        val request = item(god = true).let { it.request(it.metadata) }
        var answered: HitlResponseValue? = null
        compose.setContent { Phone {
            AnswerSheet(request, submitting = false, error = null, onDismiss = {},
                onSubmit = { answered = it }, onOpenTask = {})
        } }
        compose.onNodeWithText("Grant scope: This action").performClick()
        compose.onNodeWithText("Whole tool").performClick()
        compose.onNodeWithText("Always").performScrollTo().assertIsDisplayed().performClick()
        compose.runOnIdle { assertEquals(HitlResponseValue.Choice("always:capability"), answered) }
    }
}
