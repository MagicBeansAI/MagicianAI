package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Test

class StructuredBlockDecodeTest {
    private val json = Json { ignoreUnknownKeys = true }

    private val artifacts = """
        {"kind":"artifacts","title":"Files","items":[
          {"name":"report.md","size":2048,"ready":true,"note":null,"meta":{"pages":3}}
        ]}
    """.trimIndent()

    @Test
    fun artifactRowsWithNumericSizeDecodeFromText() {
        val block = json.decodeFromString(StructuredBlock.serializer(), artifacts)
        assertEquals(
            mapOf("name" to "report.md", "size" to "2048", "ready" to "true", "meta" to "{\"pages\":3}"),
            block.items.single(),
        )
    }

    @Test
    fun artifactRowsWithNumericSizeDecodeFromTree() {
        // The tree decoder is the one that rejected an unquoted `size`.
        val block = json.decodeFromJsonElement(StructuredBlock.serializer(), json.parseToJsonElement(artifacts))
        assertEquals("2048", block.items.single()["size"])
    }
}
