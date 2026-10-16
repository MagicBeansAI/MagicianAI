package ai.magicbeans.magdroid.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MagicanPushOrderingTest {
    @Test
    fun `an equal or delayed push cannot move a mobile surface backwards`() {
        assertTrue(isNewerPushTimestamp(previous = 100, candidate = 101))
        assertFalse(isNewerPushTimestamp(previous = 100, candidate = 100))
        assertFalse(isNewerPushTimestamp(previous = 100, candidate = 99))
        assertFalse(isNewerPushTimestamp(previous = 100, candidate = 0))
    }

    @Test
    fun `only canonical push kinds can consume an ordering lane`() {
        assertEquals("attention", pushOrderKey("attention_requested", null))
        assertEquals("attention", pushOrderKey("attention_resolved", null))
        assertEquals("task:task-1", pushOrderKey("task_progress", " task-1 "))
        assertNull(pushOrderKey("task_progress", ""))
        assertNull(pushOrderKey("unrelated", "task-1"))
        assertNull(pushOrderKey(null, null))
    }

    @Test
    fun `attention is ordered per ask so a late resolve of one ask is not dropped behind another`() {
        assertEquals("attention:ask-a", pushOrderKey("attention_resolved", null, "ask-a"))
        assertEquals("attention:ask-b", pushOrderKey("attention_requested", null, " ask-b "))
        assertEquals("attention", pushOrderKey("attention_requested", null, ""))
        // Expiry shares its ask's lane; it used to be dropped as unordered.
        assertEquals("attention:ask-a", pushOrderKey("attention_expired", null, "ask-a"))
    }

    @Test
    fun `a followed item is ordered on its own lane and its route removal names it`() {
        assertEquals("work_item:crew:c1", pushOrderKey("work_item", null, null, " crew:c1 "))
        assertNull(pushOrderKey("work_item", "task-1"))
        assertEquals("kind=work_item&task_id=crew%3Ac1", workItemRouteRemovalQuery("crew:c1"))
    }

    @Test
    fun `canonical pushes require a positive server event timestamp`() {
        assertEquals(101L, pushEventTimestamp(mapOf("event_timestamp" to "101")))
        assertNull(pushEventTimestamp(emptyMap()))
        assertNull(pushEventTimestamp(mapOf("event_timestamp" to "0")))
        assertNull(pushEventTimestamp(mapOf("event_timestamp" to "not-a-number")))
    }

    @Test
    fun `launch asks for notifications once, and settings can ask again after a denial`() {
        assertTrue(
            shouldRequestNotificationPermissionOnLaunch(
                granted = false,
                runtimePromptAvailable = true,
                askedBefore = false,
            ),
        )
        assertFalse(
            shouldRequestNotificationPermissionOnLaunch(
                granted = true,
                runtimePromptAvailable = true,
                askedBefore = false,
            ),
        )
        assertFalse(
            shouldRequestNotificationPermissionOnLaunch(
                granted = false,
                runtimePromptAvailable = true,
                askedBefore = true,
            ),
        )
        assertFalse(
            shouldRequestNotificationPermissionOnLaunch(
                granted = false,
                runtimePromptAvailable = false,
                askedBefore = false,
            ),
        )

        assertEquals(
            NotificationPrompt.Request,
            notificationPrompt(
                granted = false,
                runtimePromptAvailable = true,
                askedBefore = false,
                showRationale = false,
            ),
        )
        assertEquals(
            NotificationPrompt.Request,
            notificationPrompt(
                granted = false,
                runtimePromptAvailable = true,
                askedBefore = true,
                showRationale = true,
            ),
        )
        assertEquals(
            NotificationPrompt.OpenSettings,
            notificationPrompt(
                granted = false,
                runtimePromptAvailable = true,
                askedBefore = true,
                showRationale = false,
            ),
        )
        assertEquals(
            NotificationPrompt.OpenSettings,
            notificationPrompt(
                granted = false,
                runtimePromptAvailable = false,
                askedBefore = false,
                showRationale = false,
            ),
        )
        assertEquals(
            NotificationPrompt.Granted,
            notificationPrompt(
                granted = true,
                runtimePromptAvailable = true,
                askedBefore = true,
                showRationale = false,
            ),
        )
    }

    @Test
    fun `missing host provider configuration does not create an endless worker retry`() {
        val missingProvider =
            """{"error":"mobile_push_provider_not_configured"}"""

        assertFalse(shouldRetryPushRegistration(503, missingProvider))
        assertTrue(shouldRetryPushRegistration(503, "temporarily unavailable"))
        assertTrue(shouldRetryPushRegistration(429, "rate limited"))
        assertFalse(shouldRetryPushRegistration(401, "unauthorized"))
    }

    @Test
    fun `only state-invalidating pushes schedule a canonical Today refresh`() {
        assertTrue(shouldRequestCanonicalGlanceRefresh("attention_requested", null))
        assertTrue(shouldRequestCanonicalGlanceRefresh("attention_resolved", null))
        assertTrue(shouldRequestCanonicalGlanceRefresh("task_progress", "true"))
        assertFalse(shouldRequestCanonicalGlanceRefresh("task_progress", "false"))
        assertFalse(shouldRequestCanonicalGlanceRefresh("task_progress", null))
    }

    @Test
    fun `task route teardown carries the exact registered generation`() {
        assertEquals(
            "kind=task_activity&task_id=task+one%2Ftwo&revision=42",
            taskRouteRemovalQuery("task one/two", 42),
        )
    }

    @Test
    fun `late route workers cannot roll back or clear a newer revision`() {
        assertTrue(shouldReplaceTaskRouteRevision(current = 41, incoming = 42))
        assertFalse(shouldReplaceTaskRouteRevision(current = 42, incoming = 41))
        assertFalse(shouldReplaceTaskRouteRevision(current = 42, incoming = 42))
        assertTrue(shouldClearTaskRouteRevision(current = 42, expected = 42))
        assertFalse(shouldClearTaskRouteRevision(current = 43, expected = 42))
    }

    @Test
    fun `an ask notification times out at its expiry`() {
        assertEquals(3_600_000L, attentionTimeoutMillis("1003600000", 1_000_000_000))
        assertEquals(0L, attentionTimeoutMillis("999999000", 1_000_000_000))
        assertNull(attentionTimeoutMillis(null, 1_000_000_000))
        assertNull(attentionTimeoutMillis("soon", 1_000_000_000))
    }

    @Test
    fun `the sweep takes down only notifications whose window has closed`() {
        val stored = mapOf("past" to 999L, "now" to 1_000L, "future" to 1_001L, "unset" to 0L, "" to 5L)
        assertEquals(listOf("now", "past"), expiredAttentionTags(stored, nowMillis = 1_000L))
        assertEquals(emptyList<String>(), expiredAttentionTags(emptyMap(), nowMillis = 1_000L))
    }
}
