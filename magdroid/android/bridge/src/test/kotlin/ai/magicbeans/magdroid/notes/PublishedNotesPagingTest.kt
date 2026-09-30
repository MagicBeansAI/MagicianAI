package ai.magicbeans.magdroid.notes

import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Published Notes paging and actions, at parity with iOS `PublishedTaskNotesViewModel`. */
class PublishedNotesPagingTest {

    @Test
    fun `page sizes match ios and the web`() {
        assertEquals(listOf(5, 10, 20, 50), PUBLISHED_NOTES_PAGE_SIZES)
        assertEquals(5, PUBLISHED_NOTES_DEFAULT_PAGE_SIZE)
    }

    @Test
    fun `range and page labels`() {
        val p = PublishedNotesPager(currentPage = 2, pageSize = 5, offset = 5, itemCount = 5, total = 12, hasMore = true)
        assertEquals(3, p.pageCount)
        assertEquals("6–10 of 12", p.rangeLabel)
        assertEquals("Page 2 of 3", p.pageLabel)
        assertTrue(p.canPrevious)
        assertTrue(p.canNext)

        val last = p.copy(currentPage = 3, offset = 10, itemCount = 2, hasMore = false)
        assertEquals("11–12 of 12", last.rangeLabel)
        assertFalse(last.canNext)
    }

    @Test
    fun `empty collection is one page of nothing`() {
        val p = PublishedNotesPager(1, 5, 0, 0, 0, false)
        assertEquals(1, p.pageCount)
        assertEquals("0–0 of 0", p.rangeLabel)
        assertFalse(p.canPrevious)
        assertFalse(p.canNext)
    }

    @Test
    fun `offset and page count arithmetic`() {
        assertEquals(0, PublishedNotesPager.offsetFor(1, 20))
        assertEquals(40, PublishedNotesPager.offsetFor(3, 20))
        assertEquals(0, PublishedNotesPager.offsetFor(0, 20))
        assertEquals(3, PublishedNotesPager.pageCountFor(101, 50))
        assertEquals(2, PublishedNotesPager.pageCountFor(100, 50))
    }

    @Test
    fun `backfill request is a batch of 25 unpublished`() {
        val body = notesJson.parseToJsonElement(backfillRequestBody(PublishedTaskNotesRepository.BACKFILL_BATCH)).jsonObject
        assertEquals("25", body["limit"]!!.jsonPrimitive.content)
        assertEquals("true", body["only_unpublished"]!!.jsonPrimitive.content)
    }

    @Test
    fun `backfill outcome sentences`() {
        fun receipt(json: String) = notesJson.decodeFromString(PublishedTaskNotesBackfillReceipt.serializer(), json)

        assertEquals(
            "Published 2 completed tasks; more remain." to null,
            backfillOutcome(receipt("""{"published":[{"task_id":"a"},{"task_id":"b"}],"errors":[],"pagination":{"has_more":true}}""")),
        )
        assertEquals(
            "Published 1 completed task." to null,
            backfillOutcome(receipt("""{"published":[{"task_id":"a"}],"pagination":{"has_more":false}}""")),
        )
        val failed = backfillOutcome(receipt("""{"published":[],"errors":[{"task_id":"a","error":"x"}]}"""))
        assertNull(failed.first)
        assertEquals("1 task page could not be published.", failed.second)
        assertEquals("Completed tasks are already published." to null, backfillOutcome(receipt("{}")))
    }

    @Test
    fun `promotion receipt decodes its candidate`() {
        val receipt = notesJson.decodeFromString(
            PublishedTaskNotePromotionReceipt.serializer(),
            """{"candidate":{"id":"c1","state":"pending_review"},"note":{"task_id":"t"}}""",
        )
        assertEquals("pending_review", receipt.candidate.state)
    }
}
