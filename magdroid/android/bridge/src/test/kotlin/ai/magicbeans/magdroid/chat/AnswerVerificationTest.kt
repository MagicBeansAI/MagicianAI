package ai.magicbeans.magdroid.chat

import org.junit.Assert.*
import org.junit.Test

class AnswerVerificationTest {
    private val reason = "figure 93217, 45330 not found in any tool result"
    private val caveat = "> ⚠️ **Not fully verified.** Magician's evidence check could not confirm part of this answer: $reason"

    @Test fun `moves only the generated caveat and retains answer figures`() {
        val body = "Revenue: 93,217.\n\n| Current | Previous |\n| --- | --- |\n| 93217 | 45330 |"
        val display = splitAnswerVerification("$caveat\n\n$body")
        assertEquals(body, display.body)
        assertEquals(reason, display.verification?.details)
        assertEquals("Total: 93217.", splitAnswerVerification("⚠️ Not fully verified. Total: 93217.").body)
    }

    @Test fun `leaves code quotations ordinary prose and numbers alone`() {
        listOf("Figure 93217; record 45330.", "Not fully verified is a status.", "```\n$caveat\n```", "Quoted below:\n\n$caveat").forEach {
            assertEquals(AnswerVerificationDisplay(it, null), splitAnswerVerification(it))
        }
        assertEquals("Answer.", splitAnswerVerification("$caveat\r\n\r\nAnswer.").body)
    }

    @Test fun `keeps structured content and original response immutable`() {
        val response = StructuredResponse(blocks = listOf(StructuredBlock(kind = "markdown", text = caveat), StructuredBlock(text = "Total: 93217.")))
        assertEquals(listOf(StructuredBlock(text = "Total: 93217.")), withoutAnswerVerification(response).blocks)
        assertEquals(2, response.blocks.size)
    }
}
