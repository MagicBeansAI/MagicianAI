package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneId
import java.time.ZonedDateTime

/** The chat cards' "Created … · Updated …" line: which times, which parts, which skeleton. */
class CardTimesTest {
    private val firstUpdate = 1_759_300_000_000L
    private val latestUpdate = 1_759_300_065_000L

    private fun task(started: Long? = firstUpdate, updated: Long? = latestUpdate) = TaskStatusCard(
        taskId = "task_abc123", title = "Write the brief", status = "running",
        startedAtMs = started, updatedAtMs = updated,
    )

    // The task card's times

    @Test fun task_created_is_the_record_time_when_fetched_else_the_first_update() {
        assertEquals(firstUpdate, CardTimes.forTask(task()).startedAtMs)
        assertEquals(firstUpdate - 5_000, CardTimes.forTask(task(), storeCreatedAtMs = firstUpdate - 5_000).startedAtMs)
        assertEquals("a zero record time is unknown", firstUpdate, CardTimes.forTask(task(), storeCreatedAtMs = 0).startedAtMs)
    }

    @Test fun task_updated_is_the_later_of_the_record_time_and_the_latest_update() {
        assertEquals(latestUpdate, CardTimes.forTask(task()).updatedAtMs)
        assertEquals(latestUpdate + 9_000, CardTimes.forTask(task(), storeUpdatedAtMs = latestUpdate + 9_000).updatedAtMs)
        assertEquals(latestUpdate, CardTimes.forTask(task(), storeUpdatedAtMs = latestUpdate - 9_000).updatedAtMs)
    }

    @Test fun task_card_times_read_epoch_seconds_and_hide_what_is_unknown() {
        val seconds = CardTimes.forTask(task(started = 1_759_300_000L, updated = 1_759_300_065L))
        assertEquals(CardTimes(firstUpdate, latestUpdate), seconds)
        val none = CardTimes.forTask(task(started = null, updated = null))
        assertTrue(none.isEmpty)
    }

    @Test fun an_update_never_reads_earlier_than_the_creation() {
        val times = CardTimes.forTask(task(started = firstUpdate, updated = firstUpdate - 1_000))
        assertEquals(firstUpdate, times.updatedAtMs)
    }

    // The store record

    @Test fun store_record_updated_is_last_progress_even_when_updated_at_is_later() {
        val record = Json.parseToJsonElement("""{"task": {
            "manifest": {"created_at": "2025-10-01T06:26:40Z", "updated_at": "2025-10-01T07:00:00Z"},
            "state": {"status": "running", "last_progress_at": "2025-10-01T06:27:45Z", "updated_at": "2025-10-01T07:00:00Z"}
        }}""").jsonObject
        val parsed = TaskStoreRecord.parse(record)!!
        assertEquals(1_759_300_000_000L, parsed.createdAtMs)
        assertEquals(1_759_300_065_000L, parsed.updatedAtMs)
    }

    @Test fun store_record_updated_falls_back_to_updated_at_without_progress() {
        val record = Json.parseToJsonElement("""{"task": {
            "state": {"status": "draft", "updated_at": "2025-10-01T06:27:45Z"}
        }}""").jsonObject
        assertEquals(1_759_300_065_000L, TaskStoreRecord.parse(record)!!.updatedAtMs)
    }

    @Test fun store_record_without_times_leaves_them_unknown() {
        val parsed = TaskStoreRecord.parse(Json.parseToJsonElement("""{"task": {"state": {"status": "ready"}}}""").jsonObject)!!
        assertNull(parsed.createdAtMs)
        assertNull(parsed.updatedAtMs)
    }

    // The Steps card's times

    @Test fun activity_started_is_the_earliest_start_and_updated_the_latest_event_or_call_end() {
        val rows = listOf(
            ActivityRow(label = "Thinking", startedAtMs = 2_000_000.0, durationMs = 1_500.0, firstSeenAtMs = 2_000_000.0, updatedAtMs = 2_001_500.0),
            ActivityRow(label = "Calling search", startedAtMs = 2_002_000.0, durationMs = 4_000.0),
            ActivityRow(label = "Context prepared", firstSeenAtMs = 1_999_000.0, updatedAtMs = 2_000_100.0),
        )
        assertEquals(CardTimes(1_999_000L, 2_006_000L), CardTimes.forActivity(rows))
    }

    @Test fun activity_rows_without_times_show_nothing() {
        assertTrue(CardTimes.forActivity(listOf(ActivityRow(label = "Done", status = "done"))).isEmpty)
        assertTrue(CardTimes.forActivity(emptyList()).isEmpty)
    }

    @Test fun the_accumulator_stamps_each_row_with_its_first_and_latest_event() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("context.retrieval.started", "phase_id" to JsonPrimitive("p"), "timestamp" to JsonPrimitive(1_700_000_000_000L)))
        sut.ingest(event("context.retrieval.completed", "phase_id" to JsonPrimitive("p"), "timestamp" to JsonPrimitive(1_700_000_004_000L)))
        val row = sut.snapshot().single()
        assertEquals(1_700_000_000_000.0, row.firstSeenAtMs!!, 0.0)
        assertEquals(1_700_000_004_000.0, row.updatedAtMs!!, 0.0)
        assertEquals(CardTimes(1_700_000_000_000L, 1_700_000_004_000L), CardTimes.forActivity(sut.snapshot()))
    }

    @Test fun an_untimed_update_keeps_the_row_times_it_replaces() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("context.retrieval.started", "phase_id" to JsonPrimitive("p"), "timestamp" to JsonPrimitive(1_700_000_000_000L)))
        sut.ingest(event("context.retrieval.completed", "phase_id" to JsonPrimitive("p")))
        val row = sut.snapshot().single()
        assertEquals("done", row.status)
        assertEquals(1_700_000_000_000.0, row.firstSeenAtMs!!, 0.0)
        assertEquals(1_700_000_000_000.0, row.updatedAtMs!!, 0.0)
    }

    // The line

    private val absolute: (Long) -> String = { "abs($it)" }
    private val relative: (Long) -> String = { "rel($it)" }

    @Test fun line_joins_both_parts_and_drops_an_unknown_one() {
        val now = 10_000_000L
        assertEquals(
            "Created abs(1000) · Updated rel(2000)",
            CardTimeText.line(CardTimes(1_000, 2_000), now, "Created", absolute, relative),
        )
        assertEquals("Started abs(1000)", CardTimeText.line(CardTimes(1_000, null), now, "Started", absolute, relative))
        assertEquals("Updated rel(2000)", CardTimeText.line(CardTimes(null, 2_000), now, "Created", absolute, relative))
        assertNull(CardTimeText.line(CardTimes(null, null), now, "Created", absolute, relative))
    }

    @Test fun under_a_minute_or_ahead_of_the_clock_reads_just_now() {
        val now = 10_000_000L
        assertEquals("just now", CardTimeText.relativeOrJustNow(now - 59_000, now, relative))
        assertEquals("just now", CardTimeText.relativeOrJustNow(now + 5_000, now, relative))
        assertEquals("rel(${now - 60_000})", CardTimeText.relativeOrJustNow(now - 60_000, now, relative))
    }

    @Test fun absolute_is_the_time_alone_today_else_month_day_and_time() {
        val zone = ZoneId.of("Asia/Kolkata")
        val now = ZonedDateTime.of(2026, 10, 31, 18, 0, 0, 0, zone).toInstant().toEpochMilli()
        val earlierToday = ZonedDateTime.of(2026, 10, 31, 0, 5, 0, 0, zone).toInstant().toEpochMilli()
        val yesterday = ZonedDateTime.of(2026, 10, 30, 23, 55, 0, 0, zone).toInstant().toEpochMilli()
        assertEquals("Hm", CardTimeText.absoluteSkeleton(earlierToday, now, zone, is24Hour = true))
        assertEquals("hma", CardTimeText.absoluteSkeleton(earlierToday, now, zone, is24Hour = false))
        assertEquals("MMMdHm", CardTimeText.absoluteSkeleton(yesterday, now, zone, is24Hour = true))
        assertEquals("MMMdhma", CardTimeText.absoluteSkeleton(yesterday, now, zone, is24Hour = false))
        assertEquals("yMMMdHms", CardTimeText.exactSkeleton(is24Hour = true))
    }

    private fun event(type: String, vararg payload: Pair<String, JsonElement>) = buildJsonObject {
        put("event_type", type)
        putJsonObject("data") { payload.forEach { (k, v) -> put(k, v) } }
    }
}
