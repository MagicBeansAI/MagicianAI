package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.TodayAction
import org.junit.Assert.assertEquals
import org.junit.Test

class TodaySwipeActionContractTest {
    @Test fun short_swipes_reveal_the_matching_action_rail() {
        assertEquals(
            TodaySwipeResolution.LeadingOpen,
            resolveTodaySwipeRelease(30f, 40f, 320f, 72f, 72f, 96f),
        )
        assertEquals(
            TodaySwipeResolution.TrailingOpen,
            resolveTodaySwipeRelease(-30f, -60f, 320f, 72f, 144f, 96f),
        )
    }

    @Test fun full_or_fast_swipes_commit_only_the_primary_edge_action() {
        assertEquals(
            TodaySwipeResolution.CommitLeading,
            resolveTodaySwipeRelease(170f, 170f, 320f, 72f, 144f, 96f),
        )
        assertEquals(
            TodaySwipeResolution.CommitTrailing,
            resolveTodaySwipeRelease(-40f, -250f, 320f, 72f, 144f, 96f),
        )
    }

    @Test fun an_edge_without_actions_cannot_open_or_commit() {
        assertEquals(
            TodaySwipeResolution.Closed,
            resolveTodaySwipeRelease(300f, 600f, 320f, 0f, 72f, 96f),
        )
    }

    @Test fun overswipe_is_resisted_and_capped_like_ios() {
        val visible = todaySwipeVisibleOffset(
            rawOffset = -500f,
            cardWidth = 320f,
            leadingRevealWidth = 72f,
            trailingRevealWidth = 144f,
        )
        assertEquals(-294.4f, visible, 0.01f)
    }

    @Test fun only_the_resting_edge_is_exposed_to_accessibility() {
        assertEquals(false, isTodaySwipeRailExposed(0f, leading = true))
        assertEquals(false, isTodaySwipeRailExposed(0f, leading = false))
        assertEquals(true, isTodaySwipeRailExposed(72f, leading = true))
        assertEquals(false, isTodaySwipeRailExposed(72f, leading = false))
        assertEquals(false, isTodaySwipeRailExposed(-72f, leading = true))
        assertEquals(true, isTodaySwipeRailExposed(-72f, leading = false))
    }

    @Test fun action_partition_never_drops_server_actions() {
        val actions = (1..5).map { index ->
            TodayAction(id = "action-$index", label = "Action $index")
        }
        val presentation = partitionTodayActions(actions)
        assertEquals(listOf("action-1", "action-2"), presentation.inline.map(TodayAction::id))
        assertEquals(listOf("action-3", "action-4", "action-5"), presentation.overflow.map(TodayAction::id))
        assertEquals(actions, presentation.inline + presentation.overflow)
    }
}
