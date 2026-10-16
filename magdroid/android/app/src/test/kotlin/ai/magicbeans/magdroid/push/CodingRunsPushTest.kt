package ai.magicbeans.magdroid.push

import ai.magicbeans.magdroid.ui.AppShortcutLinks
import ai.magicbeans.magdroid.ui.AppShortcutTarget
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CodingRunsPushTest {
    @Test
    fun `coding runs pushes have their own ordering lane`() {
        assertEquals("coding_runs", pushOrderKey("coding_runs", null))
        assertEquals("coding_runs", pushOrderKey("coding_runs", "task_1"))
    }

    @Test
    fun `the route is removed by kind for every generation`() {
        assertEquals("kind=coding_runs", CODING_RUNS_ROUTE_REMOVAL_QUERY)
    }

    @Test
    fun `the notification's link opens VibeDev`() {
        assertEquals(AppShortcutTarget.OpenVibeDev, AppShortcutLinks.parse("magican://vibe"))
    }

    @Test
    fun `a route left on a machine that is no longer active is retired`() {
        val a = "https://a.example.com"
        val b = "https://b.example.com"
        assertTrue(shouldRetireCodingRunsRoute(routeOrigin = a, activeOrigin = b))
        // Signed out of every machine: the old route still goes.
        assertTrue(shouldRetireCodingRunsRoute(routeOrigin = a, activeOrigin = null))
        // The route is on the machine in use, or none was recorded.
        assertFalse(shouldRetireCodingRunsRoute(routeOrigin = a, activeOrigin = a))
        assertFalse(shouldRetireCodingRunsRoute(routeOrigin = null, activeOrigin = b))
        assertFalse(shouldRetireCodingRunsRoute(routeOrigin = " ", activeOrigin = b))
    }

    @Test
    fun `every machine that may hold a route is retired, not only the last one recorded`() {
        val a = "https://a.example.com"
        val b = "https://b.example.com"
        val c = "https://c.example.com"
        // A route recorded before a PUT a switch cancelled stays findable.
        assertEquals(listOf(a, c), codingRunsRoutesToRetire(setOf(c, b, a), activeOrigin = b))
        assertEquals(listOf(a, b), codingRunsRoutesToRetire(setOf(a, b), activeOrigin = null))
        assertEquals(emptyList<String>(), codingRunsRoutesToRetire(setOf(b, " "), activeOrigin = b))
    }

    @Test
    fun `a removal the machine refuses for good settles the route, a transient one retries`() {
        listOf(200, 204).forEach { assertEquals(CodingRunsRouteRemoval.Done, codingRunsRouteRemoval(it)) }
        // No such route or endpoint, an older server refusing
        // kind=coding_runs: forgotten, not re-sent on every ensure.
        listOf(400, 404, 410, 422).forEach {
            assertEquals("status $it", CodingRunsRouteRemoval.Done, codingRunsRouteRemoval(it))
        }
        // Not authorized may be the other instance behind a shared tunnel:
        // asked again a few times before the record is let go.
        listOf(401, 403).forEach {
            assertEquals("status $it", CodingRunsRouteRemoval.Refused, codingRunsRouteRemoval(it))
        }
        listOf(408, 429, 500, 502, 503).forEach {
            assertEquals("status $it", CodingRunsRouteRemoval.Retry, codingRunsRouteRemoval(it))
        }
        assertEquals(CodingRunsRouteRemoval.Rejected, codingRunsRouteRemoval(302))
    }

    @Test
    fun `a refused PUT is a rejection, not a retry`() {
        val rejected: IllegalStateException = PushRegistrationRejected(401)
        assertEquals(401, (rejected as PushRegistrationRejected).status)
        assertEquals("push registration rejected (401)", rejected.message)
    }

    @Test
    fun `the card starts over only when its machine changed`() {
        val a = "https://a.example.com"
        val b = "https://b.example.com"
        assertTrue(isCodingRunsMachineSwitch(previous = a, active = b))
        // Forgot the last machine: nothing is active.
        assertTrue(isCodingRunsMachineSwitch(previous = a, active = ""))
        // A credential change on the same machine, or the first run with a record.
        assertFalse(isCodingRunsMachineSwitch(previous = a, active = a))
        assertFalse(isCodingRunsMachineSwitch(previous = null, active = a))
    }
}
