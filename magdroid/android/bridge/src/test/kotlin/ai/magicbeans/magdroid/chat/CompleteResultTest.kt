package ai.magicbeans.magdroid.chat

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CompleteResultTest {
    @Test
    fun `canonical chat owner wins over delegated task navigation`() {
        val target = resolveCompleteResultReadTarget(
            owner = ActivityResultOwner(kind = "chat", sessionId = "parent session"),
            hostSessionId = "host-session",
            legacyTaskId = "delegated-task",
            legacyExecutionId = "delegated-execution",
        )

        assertEquals(
            "/api/magician/v2/chat/sessions/parent%20session/results/read",
            target?.path,
        )
        assertNull(target?.executionId)
    }

    @Test
    fun `ephemeral voice owner has no durable read target`() {
        assertNull(
            resolveCompleteResultReadTarget(
                owner = ActivityResultOwner(kind = "ephemeral_voice", voiceSessionId = "voice-1"),
                hostSessionId = "host-session",
            ),
        )
    }

    @Test
    fun `task owner binds its execution to every read`() = runTest {
        val target = requireNotNull(
            resolveCompleteResultReadTarget(
                owner = ActivityResultOwner(
                    kind = "task",
                    taskId = "task 7",
                    executionId = "exec-7",
                ),
                hostSessionId = "host-session",
            ),
        )
        var captured: CompleteResultReadRequest? = null
        readCompleteResult(
            resultRef = "task-result",
            expectedContentHash = null,
            readTarget = target,
        ) { request ->
            captured = request
            CompleteResultReadPage(
                contentRef = "task-result",
                contentHash = "blake3:task",
                entries = listOf(
                    buildJsonObject {
                        put("field_path", "")
                        put("value", "done")
                    },
                ),
                pageStart = 0,
                totalEntries = 1,
            )
        }

        assertEquals("/api/magician/v3/tasks/task%207/results/read", target.path)
        assertEquals("exec-7", captured?.executionId)
    }

    @Test
    fun `reader joins every page and reconstructs a legacy array`() = runTest {
        val requests = mutableListOf<CompleteResultReadRequest>()
        val result = readCompleteResult(
            resultRef = "result-7",
            expectedContentHash = null,
            readTarget = CompleteResultReadTarget("/read", null),
        ) { request ->
            requests += request
            if (request.cursor == null) {
                CompleteResultReadPage(
                    contentRef = "result-7",
                    contentHash = "blake3:complete",
                    entries = listOf(
                        buildJsonObject {
                            put("field_path", "")
                            put("source_index", 0)
                            put("value", buildJsonObject { put("id", 1) })
                        },
                    ),
                    pageStart = 0,
                    totalEntries = 2,
                    nextCursor = "cursor-2",
                )
            } else {
                CompleteResultReadPage(
                    contentRef = "result-7",
                    contentHash = "blake3:complete",
                    entries = listOf(
                        buildJsonObject {
                            put("field_path", "")
                            put("source_index", 1)
                            put("value", buildJsonObject { put("id", 2) })
                        },
                    ),
                    pageStart = 1,
                    totalEntries = 2,
                )
            }
        }

        assertEquals(listOf(null, "cursor-2"), requests.map { it.cursor })
        assertEquals("blake3:complete", result.contentHash)
        assertTrue(result.text.contains("\"id\": 1"))
        assertTrue(result.text.contains("\"id\": 2"))
    }

    @Test
    fun `typed containers and utf8 fragments reconstruct without blank content`() = runTest {
        val first = "नम"
        val second = "स्ते"
        val firstBytes = first.toByteArray().size
        val totalBytes = (first + second).toByteArray().size
        val entries = listOf(
            buildJsonObject {
                put("field_path", "")
                put("reconstruction_path", "")
                put("kind", "container")
                put("value", buildJsonObject {})
            },
            buildJsonObject {
                put("field_path", "")
                put("reconstruction_path", "/slash~1key~0")
                put("kind", "string_fragment")
                put("string_fragment", buildJsonObject {
                    put("byte_start", 0)
                    put("byte_end", firstBytes)
                    put("total_bytes", totalBytes)
                })
                put("value", first)
            },
            buildJsonObject {
                put("field_path", "")
                put("reconstruction_path", "/slash~1key~0")
                put("kind", "string_fragment")
                put("string_fragment", buildJsonObject {
                    put("byte_start", firstBytes)
                    put("byte_end", totalBytes)
                    put("total_bytes", totalBytes)
                })
                put("value", second)
            },
            buildJsonObject {
                put("field_path", "")
                put("reconstruction_path", "/status")
                put("kind", "complete_value")
                put("value", "ok")
            },
        )

        val value = reconstructCompleteResult(entries, version = 1)
        val text = value.toString()
        assertFalse(text.isBlank())
        assertTrue(text.contains("नमस्ते"))
        assertTrue(text.contains("slash/key~"))
    }

    @Test
    fun `reader rejects a content hash change`() = runTest {
        var page = 0
        val failure = runCatching {
            readCompleteResult(
                resultRef = "result-7",
                expectedContentHash = "blake3:first",
                readTarget = CompleteResultReadTarget("/read", null),
            ) {
                val first = page++ == 0
                CompleteResultReadPage(
                    contentHash = if (first) "blake3:first" else "blake3:changed",
                    entries = listOf(
                        buildJsonObject {
                            put("field_path", "")
                            put("source_index", if (first) 0 else 1)
                            put("value", JsonPrimitive(if (first) "a" else "b"))
                        },
                    ),
                    pageStart = if (first) 0 else 1,
                    totalEntries = 2,
                    nextCursor = if (first) "next" else null,
                )
            }
        }.exceptionOrNull()

        assertTrue(failure?.message.orEmpty().contains("hash changed"))
    }
}
