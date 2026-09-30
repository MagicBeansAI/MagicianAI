package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.TaskSwipeAction
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class TasksChromeContractTest {
    @Test
    fun task_chrome_stays_compact() {
        assertEquals(52, TASK_HEADER_HEIGHT_DP)
        assertEquals(15, TASK_HEADER_TITLE_FONT_SP)
        assertEquals(42, TASK_SEARCH_HEIGHT_DP)
        assertTrue(TASK_LANE_VERTICAL_PADDING_DP <= 3)
        assertTrue(TASK_LANE_ITEM_VERTICAL_PADDING_DP <= 3)
        assertTrue(TASK_LANE_FONT_SP <= 10)
        assertEquals(4, TASK_CHIP_VERTICAL_PADDING_DP)
        assertTrue(TASK_CHIP_HORIZONTAL_PADDING_DP <= 7)
        assertEquals(11, TASK_CHIP_FONT_SP)
        assertEquals(13, TASK_CHIP_LINE_HEIGHT_SP)
        assertEquals(13, TASK_CHIP_ICON_DP)
        assertTrue(TASK_CHIP_COUNT_FONT_SP <= 9)
        assertTrue(TASK_LEDGER_HORIZONTAL_PADDING_DP <= 6)
        assertTrue(TASK_LEDGER_VERTICAL_PADDING_DP <= 1)
        assertTrue(TASK_LEDGER_FONT_SP <= 9)
        assertTrue(TASK_LEDGER_LINE_HEIGHT_SP <= 10)
        assertTrue(TASK_EMPTY_PROGRESS_DP <= 24)
        assertTrue(TASK_EMPTY_ICON_DP <= 24)
        assertTrue(TASK_EMPTY_TITLE_FONT_SP <= 14)
        assertTrue(TASK_EMPTY_BODY_FONT_SP <= 11)
        assertTrue(TASK_EMPTY_ACTION_HEIGHT_DP <= 36)
        assertEquals(14, TASK_CARD_TITLE_FONT_SP)
        assertEquals(16, TASK_CARD_TITLE_LINE_HEIGHT_SP)
        assertEquals(12, TASK_CARD_BODY_FONT_SP)
        assertEquals(14, TASK_CARD_BODY_LINE_HEIGHT_SP)
        assertTrue(TASK_CARD_PADDING_DP <= 10)
        assertTrue(TASK_CARD_SPACING_DP <= 3)
        assertTrue(TASK_ACTION_HORIZONTAL_PADDING_DP <= 7)
        assertEquals(4, TASK_ACTION_VERTICAL_PADDING_DP)
        assertEquals(10, TASK_ACTION_FONT_SP)
        assertEquals(13, TASK_ACTION_LINE_HEIGHT_SP)
        assertEquals(13, TASK_ACTION_ICON_DP)
        assertTrue(TASK_ACTION_CORNER_DP <= 5)
        assertTrue(TASK_STATUS_FONT_SP <= 9)
        assertTrue(TASK_STATUS_HORIZONTAL_PADDING_DP <= 6)
        assertTrue(TASK_STATUS_VERTICAL_PADDING_DP <= 1)
        assertTrue(TASK_STATUS_LINE_HEIGHT_SP <= 10)
        assertTrue(TASK_STATUS_CORNER_DP <= 4)
    }

    @Test
    fun task_surfaces_and_semantic_statuses_follow_the_selected_palette() {
        val day = Themes.palette("longhand")
        val night = Themes.palette("longhand-dark")

        assertEquals(day.background, taskWorkspaceColor(day))
        assertEquals(day.card, taskCardColor(day))
        assertEquals(day.cardBorder, taskCardBorderColor(day))
        assertEquals(day.control, taskControlColor(day))
        assertEquals(day.success, taskStatusTint("completed", day))
        assertEquals(day.info, taskStatusTint("running", day))
        assertEquals(day.warning, taskStatusTint("paused", day))
        assertEquals(day.danger, taskStatusTint("failed", day))
        assertNotEquals(taskStatusTint("completed", day), taskStatusTint("completed", night))
        assertNotEquals(taskStatusTint("running", day), taskStatusTint("running", night))

        assertEquals("Complete", taskSwipeTitle(TaskSwipeAction.MarkComplete))
        assertEquals("Not done", taskSwipeTitle(TaskSwipeAction.MarkNotDone))
        assertEquals("Cancel", taskSwipeTitle(TaskSwipeAction.Cancel))
        assertEquals("Delete", taskSwipeTitle(TaskSwipeAction.Delete))
        assertEquals(day.success, taskSwipeTint(TaskSwipeAction.MarkComplete, day))
        assertEquals(day.info, taskSwipeTint(TaskSwipeAction.MarkNotDone, day))
        assertEquals(day.warning, taskSwipeTint(TaskSwipeAction.Cancel, day))
        assertEquals(day.danger, taskSwipeTint(TaskSwipeAction.Delete, day))
    }

    @Test
    fun header_actions_drive_the_active_tasks_workspace() {
        val actions = TasksScreenActions()
        var refreshes = 0
        actions.bindRefresh { refreshes += 1 }

        assertFalse(actions.isCreateVisible)
        actions.showCreate()
        assertTrue(actions.isCreateVisible)
        actions.hideCreate()
        assertFalse(actions.isCreateVisible)

        actions.refresh()
        assertEquals(1, refreshes)
        actions.bindRefresh(null)
        actions.refresh()
        assertEquals(1, refreshes)
    }
}
