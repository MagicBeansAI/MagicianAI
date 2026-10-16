package ai.magicbeans.magdroid.notification

import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The companion's extractor and the runtime's (`verification_codes::extract`)
 * are one rule set. Both are checked against the runtime's fixture list; a
 * rule that changes on one side fails here until the other follows.
 */
class OtpWatcherFixtureTest {

    private val fixtures = File("../../../magician/src/magician_v2/verification_codes/extraction_fixtures.json")

    @Test
    fun `the runtime's extraction fixtures hold on the phone`() {
        assertTrue("fixture list at ${fixtures.absolutePath}", fixtures.isFile)
        val entries = Json.parseToJsonElement(fixtures.readText()).jsonArray
        assertTrue(entries.size >= 30)
        entries.forEach { entry ->
            val fixture = entry.jsonObject
            val text = fixture.getValue("text").jsonPrimitive.content
            val digits = fixture["digits"]?.jsonPrimitive?.intOrNull
            val expect = fixture["expect"]?.jsonPrimitive?.contentOrNull
            val why = fixture["why"]?.jsonPrimitive?.contentOrNull ?: ""
            val got = OtpWatcher.extractCode(text, digits)
            when (expect) {
                null -> assertEquals("$why: $text", OtpWatcher.Extraction.None, got)
                "ambiguous" -> assertTrue("$why: $text -> $got", got is OtpWatcher.Extraction.Ambiguous)
                else -> assertEquals("$why: $text", OtpWatcher.Extraction.Code(expect), got)
            }
        }
    }
}
