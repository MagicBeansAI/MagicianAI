package ai.magicbeans.magdroid.apps

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

class AppWidgetFreshnessTest {
    private val digest = "blake3:" + "0".repeat(64)
    private val now = Instant.parse("2026-09-28T12:00:00Z")
    private val slot = AppSlotId.forPageRegion("/", "primary")

    @Test
    fun `a due or missing deadline is accepted with a floor instead of refused`() {
        val floor = now.plusMillis(AppWidgetDeadlineFloorMillis)
        assertEquals(floor, acceptedWidgetRefreshDeadline(now, null))
        assertEquals(floor, acceptedWidgetRefreshDeadline(now, now))
        assertEquals(floor, acceptedWidgetRefreshDeadline(now, now.minusSeconds(3)))
        assertEquals(floor, acceptedWidgetRefreshDeadline(now, now.plusMillis(5)))
        assertTrue(AppWidgetDeadlineFloorMillis >= AppSurfacingViewModel.MinimumRefreshMillis)
        assertEquals(now.plusSeconds(30), acceptedWidgetRefreshDeadline(now, now.plusSeconds(30)))
        // The accepted deadline schedules a real wait, never a retry loop.
        assertTrue(
            AppSurfacingViewModel.nextForegroundDelay(now, acceptedWidgetRefreshDeadline(now, now)) >=
                AppSurfacingViewModel.MinimumRefreshMillis,
        )
    }

    @Test
    fun `a 304 keeps the cached bodies and renews every item deadline`() {
        val stale = snapshot(confirmedAt = now.minusSeconds(60), itemRefreshAfter = now.minusSeconds(1))
        val deadline = now.plusSeconds(30)
        val renewed = stale.renewedNotModified(
            AppWidgetPageRefresh.NotModified(stale.assignments, "fp-2", digest, deadline),
            now,
        )
        assertEquals(stale.widgetsBySlot.keys, renewed.widgetsBySlot.keys)
        assertEquals(deadline.toString(), renewed.widgetsBySlot.getValue(slot).refreshAfter)
        assertEquals(deadline, renewed.refreshAfter)
        assertEquals(now, renewed.confirmedAt)
        assertEquals("fp-2", renewed.targetFingerprint)
    }

    @Test
    fun `a transient failure keeps the last good widgets within the default bound`() {
        val kept = snapshot(confirmedAt = now.minusSeconds(60)).retainedAfterFailure(now)
        assertEquals(1, kept.widgetsBySlot.size)
        assertEquals(digest, kept.etag)
        val retry = kept.refreshAfter!!
        assertTrue(retry.isAfter(now))
        assertFalse(retry.isAfter(now.plusMillis(AppSurfacingViewModel.FailureRetryMillis)))
    }

    @Test
    fun `past the staleness bound only the slot shape survives`() {
        val expired = snapshot(confirmedAt = now.minusSeconds(301)).retainedAfterFailure(now)
        assertTrue(expired.widgetsBySlot.isEmpty())
        assertNull(expired.etag)
        assertNull(expired.refreshAfter)
        assertEquals(1, expired.assignments.size)

        val neverConfirmed = snapshot(confirmedAt = null).retainedAfterFailure(now)
        assertTrue(neverConfirmed.widgetsBySlot.isEmpty())
    }

    @Test
    fun `a declared max staleness tightens the bound`() {
        val tight = snapshot(confirmedAt = now.minusSeconds(90), maxStalenessSeconds = 60)
        assertEquals(60_000L, tight.maxStalenessMillis())
        assertTrue(tight.retainedAfterFailure(now).widgetsBySlot.isEmpty())
        val loose = snapshot(confirmedAt = now.minusSeconds(30), maxStalenessSeconds = 60)
        assertEquals(1, loose.retainedAfterFailure(now).widgetsBySlot.size)
        assertEquals(AppSurfacingViewModel.DefaultMaxStalenessMillis, snapshot(now).maxStalenessMillis())
    }

    @Test
    fun `max staleness decodes and is bounded`() {
        val base = item(now.plusSeconds(30), maxStalenessSeconds = 300)
        base.checked()
        runCatching { base.copy(maxStalenessSeconds = 0).checked() }
            .let { assertTrue(it.isFailure) }
    }

    @Test
    fun `a hidden workspace default shows no unavailable card`() {
        val hiddenDefault = hidden(source = "workspace_default", reason = "package_unavailable")
        assertTrue(hiddenDefault.quietlyHiddenDefault)
        assertFalse(hiddenDefault.showsUnavailablePlaceholder(null))

        for (reason in listOf("disabled", "update_pending")) {
            val loud = hidden(source = "workspace_default", reason = reason)
            assertFalse(loud.quietlyHiddenDefault)
            assertTrue(loud.showsUnavailablePlaceholder(null))
        }
        val user = hidden(source = "user", reason = "package_unavailable")
        assertFalse(user.quietlyHiddenDefault)
        assertTrue(user.showsUnavailablePlaceholder(null))

        val empty = AppResolvedSlotAssignmentWire(slot.value, pinnedSystemDefault = false, optedOut = false)
            .checked(slot)
        assertFalse(empty.showsUnavailablePlaceholder(null))
    }

    private fun hidden(source: String, reason: String): AppResolvedSlotAssignment =
        AppResolvedSlotAssignmentWire(
            slotId = slot.value,
            source = source,
            pinnedSystemDefault = false,
            optedOut = false,
            hiddenReason = reason,
        ).checked(slot)

    private fun item(refreshAfter: Instant, maxStalenessSeconds: Long? = null) = AppWidgetRenderItem(
        installationId = "installation-1",
        widgetId = "summary",
        installationGeneration = 2,
        revision = digest,
        renderedAt = refreshAfter.minusSeconds(30).toString(),
        refreshAfter = refreshAfter.toString(),
        state = "ready",
        model = AppWidgetNativeModel(model = "detail", hints = AppWidgetRenderHints(), actions = emptyList()),
        maxStalenessSeconds = maxStalenessSeconds,
    )

    private fun snapshot(
        confirmedAt: Instant?,
        itemRefreshAfter: Instant = now.plusSeconds(30),
        maxStalenessSeconds: Long? = null,
    ) = AppWidgetPageSnapshot(
        assignments = mapOf(slot to hidden(source = "workspace_default", reason = "package_unavailable")),
        widgetsBySlot = mapOf(slot to item(itemRefreshAfter, maxStalenessSeconds)),
        targetFingerprint = "fp-1",
        etag = digest,
        refreshAfter = itemRefreshAfter,
        confirmedAt = confirmedAt,
    )
}
