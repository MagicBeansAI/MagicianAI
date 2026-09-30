package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.fail
import org.junit.Test

class ChatSessionStartupTest {
    private fun session(
        id: String,
        updatedAt: Long,
        status: String? = "active",
        lane: String? = "personal",
    ) = SessionSummary(
        id = id,
        updatedAt = updatedAt,
        status = status,
        historyLane = lane,
    )

    @Test
    fun `startup fallback picks newest active personal session across threads`() {
        val sessions = listOf(
            session("older", 10),
            session("newest", 30),
            session("middle", 20),
        )

        assertEquals("newest", mostRecentRestorableSession(sessions))
    }

    @Test
    fun `startup fallback does not reopen archived or automated sessions`() {
        val sessions = listOf(
            session("archived", 30, status = "archived"),
            session("automated", 20, lane = "automated"),
            session("personal", 10),
        )

        assertEquals("personal", mostRecentRestorableSession(sessions))
    }

    @Test
    fun `legacy rows without status or lane remain restorable`() {
        assertEquals(
            "legacy",
            mostRecentRestorableSession(listOf(session("legacy", 1, status = null, lane = null))),
        )
    }

    @Test
    fun `empty or unavailable session ids require creation`() {
        assertNull(mostRecentRestorableSession(emptyList()))
        assertNull(mostRecentRestorableSession(listOf(session("", 1))))
    }

    @Test
    fun `remembered session reopens without listing or creating`() = runTest {
        var lists = 0
        var creates = 0
        val resolution = resolveChatSessionStartup(
            forceNew = false,
            currentSessionId = null,
            rememberedSessionId = "remembered",
            findExistingSession = { lists++; null },
            readHistory = { id -> listOf(ChatMessage(id = "message", fromUser = false, text = id)) },
            createSession = { creates++; "created" },
            onRememberedMissing = {},
            onCreated = {},
        )

        assertEquals("remembered", resolution.sessionId)
        assertEquals("remembered", resolution.history.single().text)
        assertEquals(0, lists)
        assertEquals(0, creates)
    }

    @Test
    fun `transient remembered-session failure never creates a duplicate`() = runTest {
        var creates = 0
        try {
            resolveChatSessionStartup(
                forceNew = false,
                currentSessionId = null,
                rememberedSessionId = "remembered",
                findExistingSession = { null },
                readHistory = {
                    throw ChatError(
                        Failure(
                            FailureKind.Unreachable,
                            "Magician is offline",
                            "No response",
                        ),
                    )
                },
                createSession = { creates++; "created" },
                onRememberedMissing = {},
                onCreated = {},
            )
            fail("transient failure should propagate")
        } catch (problem: ChatError) {
            assertEquals(FailureKind.Unreachable, problem.failure.kind)
        }
        assertEquals(0, creates)
    }

    @Test
    fun `confirmed deletion creates once and remembers before reading`() = runTest {
        val events = mutableListOf<String>()
        val resolution = resolveChatSessionStartup(
            forceNew = false,
            currentSessionId = null,
            rememberedSessionId = "deleted",
            findExistingSession = { error("the remembered path must not list") },
            readHistory = { id ->
                events += "read:$id"
                if (id == "deleted") {
                    throw ChatError(
                        Failure(FailureKind.NotFound, "Missing", "Deleted", retryable = false),
                    )
                }
                emptyList()
            },
            createSession = { events += "create"; "replacement" },
            onRememberedMissing = { events += "forget:$it" },
            onCreated = { events += "remember:$it" },
        )

        assertEquals("replacement", resolution.sessionId)
        assertEquals(
            listOf("read:deleted", "forget:deleted", "create", "remember:replacement", "read:replacement"),
            events,
        )
    }

    @Test
    fun `session-list failure without a preference never creates`() = runTest {
        var creates = 0
        try {
            resolveChatSessionStartup(
                forceNew = false,
                currentSessionId = null,
                rememberedSessionId = null,
                findExistingSession = {
                    throw ChatError(Failure(FailureKind.Garbled, "Bad list", "Unreadable"))
                },
                readHistory = { emptyList() },
                createSession = { creates++; "created" },
                onRememberedMissing = {},
                onCreated = {},
            )
            fail("malformed list should propagate")
        } catch (problem: ChatError) {
            assertEquals(FailureKind.Garbled, problem.failure.kind)
        }
        assertEquals(0, creates)
    }
}
