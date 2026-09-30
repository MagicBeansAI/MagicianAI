package ai.magicbeans.magdroid.net

import ai.magicbeans.magdroid.chat.addsSomethingTo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException

/**
 * What the owner is told when a read fails.
 *
 * These pin wording and, more importantly, the three decisions a screen makes
 * from a [Failure]: whether to offer Retry, whether to point at Settings, and
 * whether the cause is this phone or that host. Each was previously unavailable
 * — every failure was a String, so every screen guessed, and most guessed
 * "print it in red and stop".
 */
class FailureTest {

    @Test
    fun `a stopped server behind a live tunnel reads as unreachable, not as a server fault`() {
        // The everyday case: Cloudflare answers, Magician does not. Calling this
        // a server error sends somebody to read logs on a process that is not
        // running.
        listOf(502, 503, 504).forEach { status ->
            val failure = Failures.ofStatus(status, "what needs you")
            assertEquals("HTTP $status", FailureKind.Unreachable, failure.kind)
            assertEquals("Can't reach Magician", failure.headline)
            assertTrue("worth retrying", failure.retryable)
            assertFalse("settings are not the fix", failure.setupRequired)
            assertTrue("names the code for whoever wants it", failure.detail.contains("$status"))
        }
    }

    @Test
    fun `a rejected credential points at Settings and does not offer a pointless retry`() {
        listOf(401, 403).forEach { status ->
            val failure = Failures.ofStatus(status, "what needs you")
            assertEquals(FailureKind.Auth, failure.kind)
            assertTrue(failure.setupRequired)
            // Retrying the same rejected credential produces the same rejection.
            assertFalse("retry cannot help", failure.retryable)
        }
    }

    @Test
    fun `an older Magician without the route is not offered a retry either`() {
        val failure = Failures.ofStatus(404, "your maps")
        assertEquals(FailureKind.NotFound, failure.kind)
        assertFalse(failure.retryable)
        assertTrue(failure.detail.contains("your maps"))
    }

    @Test
    fun `a genuine server error is named as one`() {
        val failure = Failures.ofStatus(500, "your calendar")
        assertEquals(FailureKind.ServerFault, failure.kind)
        assertEquals("Magician hit an error", failure.headline)
        assertTrue(failure.retryable)
    }

    /**
     * The distinction the old strings could not draw. "Could not read what needs
     * you" was printed whether the phone was in a tunnel or the server was off,
     * and those have opposite fixes.
     */
    @Test
    fun `an offline phone is not reported as Magician being down`() {
        val failure = Failures.of(ConnectException("refused"), "what needs you", online = false)
        assertEquals(FailureKind.Offline, failure.kind)
        assertEquals("You're offline", failure.headline)
        // Nothing about the host is asserted, because nothing about it is known.
        assertFalse(failure.detail.contains("Magician"))
    }

    @Test
    fun `a refused connection from an online phone points at the host`() {
        val failure = Failures.of(ConnectException("refused"), "what needs you", online = true)
        assertEquals(FailureKind.Unreachable, failure.kind)
        assertTrue(failure.retryable)
    }

    @Test
    fun `a name that does not resolve is a settings problem, not a waiting problem`() {
        val failure = Failures.of(UnknownHostException("nope"), "what needs you")
        assertEquals(FailureKind.Unreachable, failure.kind)
        assertTrue("the address is what is wrong", failure.setupRequired)
    }

    @Test
    fun `a timeout is distinguished from a refusal`() {
        val failure = Failures.of(SocketTimeoutException("slow"), "your maps")
        assertEquals(FailureKind.Unreachable, failure.kind)
        assertEquals("Magician didn't answer in time", failure.headline)
        assertTrue(failure.detail.contains("your maps"))
    }

    @Test
    fun `any other IO problem still lands somewhere retryable rather than as unknown`() {
        val failure = Failures.of(IOException("socket closed"), "this conversation")
        assertEquals(FailureKind.Unreachable, failure.kind)
        assertTrue(failure.retryable)
    }

    @Test
    fun `an unreadable reply is its own cause`() {
        val failure = Failures.garbled("what needs you")
        assertEquals(FailureKind.Garbled, failure.kind)
        assertTrue(failure.retryable)
    }

    /**
     * A thrower that already classified wins. The repository saw the status
     * code; the ViewModel catching it only sees an exception, and re-deriving
     * from the message would turn a known 401 into an "unknown".
     */
    @Test
    fun `a carried failure survives the throw`() {
        val carried = Failures.ofStatus(403, "what needs you")
        val thrown = object : Exception("something"), CarriesFailure {
            override val failure = carried
        }
        assertEquals(carried, (thrown as CarriesFailure).failure)
        assertEquals(FailureKind.Auth, (thrown as CarriesFailure).failure.kind)
    }

    /** Every failure says something worth reading. An empty pane helps nobody. */
    @Test
    fun `no classification produces an empty headline`() {
        val all = listOf(
            Failures.ofStatus(500, "x"),
            Failures.ofStatus(404, "x"),
            Failures.ofStatus(401, "x"),
            Failures.ofStatus(502, "x"),
            Failures.ofStatus(418, "x"),
            Failures.garbled("x"),
            Failures.offline(),
            Failures.of(IOException(), "x"),
            Failures.of(RuntimeException("boom"), "x"),
        )
        all.forEach { assertTrue("headline is set", it.headline.isNotBlank()) }
    }
}

/**
 * Whether the server's own words are worth repeating.
 *
 * A gateway body is usually the code in prose. Appending it under a sentence
 * that already names the code says "502" three times and explains nothing.
 */
class ResponseBodyEchoTest {

    @org.junit.Test
    fun `a body that only restates the status is dropped`() {
        listOf(
            "error code: 502",
            "502 Bad Gateway",
            "  502  ",
            "HTTP 502 — Service Unavailable",
            "",
        ).forEach {
            org.junit.Assert.assertFalse(
                "\"$it\" adds nothing",
                it.addsSomethingTo(502),
            )
        }
    }

    @org.junit.Test
    fun `a body that explains itself is kept`() {
        listOf(
            "upstream connect timed out after 30s",
            "the workspace is still starting",
            "tunnel host not registered",
        ).forEach {
            org.junit.Assert.assertTrue(
                "\"$it\" is worth showing",
                it.addsSomethingTo(502),
            )
        }
    }
}
