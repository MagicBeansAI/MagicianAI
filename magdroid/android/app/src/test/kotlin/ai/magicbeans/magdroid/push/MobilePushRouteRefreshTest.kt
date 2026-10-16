package ai.magicbeans.magdroid.push

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The application push route is re-sent idempotently. Firebase calls
 * `onRegistered` only when the installation's registration changes, so a
 * route the server dropped while the id stayed the same was never sent again.
 */
class MobilePushRouteRefreshTest {
    private val route = "https://host|fid-1"
    private val minute = 60_000L

    @Test
    fun `launch, pairing and an explicit refresh always re-send`() {
        assertTrue(shouldReuploadApplicationRoute(route, 1_000, route, 1_000 + minute, force = true))
    }

    @Test
    fun `a foreground re-sends once the last send is stale`() {
        val sentAt = 10 * minute
        assertFalse(shouldReuploadApplicationRoute(route, sentAt, route, sentAt + minute, force = false))
        assertTrue(
            shouldReuploadApplicationRoute(route, sentAt, route, sentAt + APPLICATION_ROUTE_REFRESH_MS, force = false),
        )
    }

    @Test
    fun `a new id, a new host, no record or a clock that moved back re-sends`() {
        assertTrue(shouldReuploadApplicationRoute(route, 5 * minute, "https://host|fid-2", 6 * minute, force = false))
        assertTrue(shouldReuploadApplicationRoute(route, 5 * minute, "https://other|fid-1", 6 * minute, force = false))
        assertTrue(shouldReuploadApplicationRoute(null, 0, route, 6 * minute, force = false))
        assertTrue(shouldReuploadApplicationRoute(route, 9 * minute, route, 6 * minute, force = false))
    }
}
