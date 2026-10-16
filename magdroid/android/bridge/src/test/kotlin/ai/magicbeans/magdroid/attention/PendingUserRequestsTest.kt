package ai.magicbeans.magdroid.attention

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PendingUserRequestsTest {
    private val choice = """{"requests":[{"id":"choice-1","question":"Choose the test color","options":[{"id":"blue","label":"Blue"},{"id":"red","label":"Red"}],"context":{"input_type":"choice","input_schema":{"input_type":"choice","options":[{"id":"blue","label":"Blue"},{"id":"red","label":"Red"}]}},"created_at":42}]}"""

    @Test fun `a canonical chat question is answerable when the feed is empty`() {
        val pending = attentionJson.decodeFromString(PendingUserRequests.serializer(), choice)
        val feed = AttentionFeedResponse().withPendingUserRequests(pending.requests)
        val item = feed.requests.single()
        val request = item.request(item.metadata)
        assertEquals("user_request", request.source)
        assertEquals("choice-1", request.correlationId)
        assertEquals("choice", request.inputType)
        assertEquals(listOf("blue", "red"), request.options.map { it.id })
        assertTrue(item.isActionable)
        assertEquals(1L, feed.laneCount(AttentionLane.All))
        assertEquals(1L, feed.counts.needsAction)
    }

    @Test fun `an existing feed projection is not duplicated under another item id`() {
        val pending = attentionJson.decodeFromString(PendingUserRequests.serializer(), choice).requests
        val projected = pending.single().attentionItem().copy(id = "feed-row-94")
        val feed = AttentionFeedResponse(
            requests = listOf(projected),
            counts = AttentionLaneCounts(requests = 1, needsAction = 1),
            totals = AttentionLaneTotals(requests = 1),
        ).withPendingUserRequests(pending + pending)
        assertEquals(listOf("feed-row-94"), feed.requests.map { it.id })
        assertEquals(1L, feed.laneCount(AttentionLane.All))
    }

    @Test fun `a form keeps every field for the existing attention form renderer`() {
        val payload = """{"requests":[{"id":"form-1","question":"Connection details","context":{"input_type":"form","input_schema":{"questions":[{"id":"color","question":"Color","input_type":"choice","options":[{"id":"blue","label":"Blue"}]},{"id":"label","question":"Label","input_type":"text"}]}}}]}"""
        val item = attentionJson.decodeFromString(PendingUserRequests.serializer(), payload)
            .requests.single().attentionItem()
        val request = item.request(item.metadata)
        assertEquals("form", request.inputType)
        assertEquals(listOf("color", "label"), request.formQuestions.map { it.id })
    }
}
