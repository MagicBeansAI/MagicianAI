package ai.magicbeans.magdroid.attention

import java.io.File
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The HITL respond-handle contract.
 *
 * `magician/tests/fixtures/hitl/attention_rows.json` holds one Attention row per
 * HITL kind exactly as the server serves it (pinned by the Rust test
 * `feed::hitl_respond_fixtures`; the web and iOS tests read the same file).
 * Every row must post the call `expect` names: through its `metadata.respond`
 * handle, and — with the handle stripped, as an older server sends it —
 * through the per-source derivation in [request].
 */
class HitlRespondFixtureTest {

    private fun rows(): List<JsonObject> {
        val root = attentionJson.parseToJsonElement(File(fixtureRoot(), "attention_rows.json").readText())
        return root.jsonObject.getValue("rows").jsonArray.map { it.jsonObject }
    }

    private fun stripHandle(row: JsonObject): JsonObject {
        val metadata = row.getValue("metadata").jsonObject
        val hitl = (metadata["hitl_request"] as? JsonObject)?.let { JsonObject(it - "respond") }
        val stripped = JsonObject(
            (metadata - "respond").let { if (hitl != null) it + ("hitl_request" to hitl) else it },
        )
        return JsonObject(row + ("metadata" to stripped))
    }

    private fun check(fixture: JsonObject, strip: Boolean) {
        val kind = fixture.getValue("kind").jsonPrimitive.content
        val rawRow = fixture.getValue("row").jsonObject
        val row = if (strip) stripHandle(rawRow) else rawRow
        val item = attentionJson.decodeFromJsonElement(AttentionItem.serializer(), row)
        val request = item.request(item.metadata)
        if (strip) assertNull("$kind: stripped", request.respond) else assertNotNull("$kind: handle decodes", request.respond)

        val expect = fixture.getValue("expect").jsonObject
        val body = expect.getValue("body").jsonObject
        val answer = fixture.getValue("answer")
        val call = request.respondCall(answer, taskId = item.openTaskId)
        assertEquals("$kind: url", expect.getValue("url").jsonPrimitive.content, call.path)
        for (key in listOf("source", "input_type", "execution_id", "task_id")) {
            assertEquals(
                "$kind: $key",
                (body[key] as? JsonPrimitive)?.content,
                (call.body[key] as? JsonPrimitive)?.content,
            )
        }
        assertEquals("android", call.body.getValue("channel").jsonPrimitive.content)
        assertEquals(answer, call.body.getValue("value"))
    }

    @Test
    fun `every row posts the call its handle names`() {
        val fixtures = rows()
        assertTrue(fixtures.size >= 10)
        fixtures.forEach { check(it, strip = false) }
    }

    @Test
    fun `rows from an older server resolve the same call`() {
        rows().forEach { check(it, strip = true) }
    }

    @Test
    fun `an off-route handle is ignored`() {
        val handle = HitlRespondHandle.parse(
            attentionJson.parseToJsonElement(
                """{"url":"https://elsewhere.example/x/respond","correlation_id":"x","source":"user_request","input_type":"choice"}""",
            ),
        )
        assertNull(handle)
        assertEquals("vibedev-run-ended%3At%3Ae", encodeHitlPathSegment("vibedev-run-ended:t:e"))
    }

    private fun fixtureRoot(): File {
        var directory: File? = File(System.getProperty("user.dir") ?: ".").absoluteFile
        while (directory != null) {
            val candidate = File(directory, "magician/tests/fixtures/hitl")
            if (candidate.isDirectory) return candidate
            directory = directory.parentFile
        }
        error("could not locate the HITL fixtures from ${System.getProperty("user.dir")}")
    }
}
