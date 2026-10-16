package ai.magicbeans.magdroid.glance

import ai.magicbeans.magdroid.today.TodayCounts
import ai.magicbeans.magdroid.today.TodayItem
import ai.magicbeans.magdroid.today.TodayResponse
import ai.magicbeans.magdroid.today.TodaySectionsPayload
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MagicanGlanceSnapshotTest {
    @Test
    fun `needs you wins over active work and copy is lock-screen bounded`() {
        val result = MagicanGlanceSnapshot.reduce(
            TodayResponse(
                generatedAt = 42,
                counts = TodayCounts(needsYou = 2, activeWork = 4),
                sections = TodaySectionsPayload(
                    needsYou = listOf(TodayItem(title = "Important choice ".repeat(12))),
                    activeWork = listOf(
                        TodayItem(
                            title = "Build the release",
                            reason = "Running",
                            taskId = "task-1",
                        ),
                    ),
                ),
            ),
        )

        assertEquals(MagicanGlanceFocus.NeedsYou, result.focus)
        assertEquals("magican://attention", result.destination)
        assertTrue(result.subtitle.length <= 96)
    }

    @Test
    fun `refresh time alone does not cause a visible widget reload`() {
        val first = MagicanGlanceSnapshot(
            generatedAt = 1,
            focus = MagicanGlanceFocus.ActiveWork,
            title = "Build",
            subtitle = "Working",
            activeWorkCount = 1,
            taskId = "task-1",
        )

        assertTrue(first.hasSameVisibleContent(first.copy(generatedAt = 2)))
    }

    @Test
    fun `ready widget opens conversational talk rather than room observation`() {
        assertEquals("magican://talk", MagicanGlanceSnapshot().destination)
    }

    @Test
    fun `a late polling response cannot replace a newer pushed snapshot`() {
        val current = MagicanGlanceSnapshot(generatedAt = 200, title = "New")

        assertFalse(
            shouldReplaceGlanceSnapshot(
                current,
                MagicanGlanceSnapshot(generatedAt = 199, title = "Old"),
            ),
        )
        assertTrue(
            shouldReplaceGlanceSnapshot(
                current,
                MagicanGlanceSnapshot(generatedAt = 201, title = "Newer"),
            ),
        )
    }
}
