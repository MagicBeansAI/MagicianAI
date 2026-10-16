package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.TodayDeckAction
import ai.magicbeans.magdroid.today.TodayDeckTab
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TodayMorningEditionUiContractTest {
    @Test fun deck_release_commits_past_the_web_thresholds() {
        assertEquals(TodayDeckAction.Useful, resolveDeckRelease(96f, 0f, 96f, 0f))
        assertEquals(TodayDeckAction.Dismiss, resolveDeckRelease(-96f, 0f, -96f, 0f))
        assertNull(resolveDeckRelease(0f, -300f, 0f, -300f))
        assertNull(resolveDeckRelease(95f, -80f, 95f, -80f))
        assertNull(resolveDeckRelease(40f, 30f, 60f, 50f))
    }

    @Test fun deck_release_commits_on_a_fast_flick_that_is_projected_past_the_threshold() {
        assertEquals(TodayDeckAction.Useful, resolveDeckRelease(30f, 0f, 180f, 0f))
        assertEquals(TodayDeckAction.Dismiss, resolveDeckRelease(-20f, 5f, -140f, 5f))
        assertNull(resolveDeckRelease(0f, -20f, 0f, -600f))
    }

    @Test fun deck_release_prefers_horizontal_over_vertical() {
        assertEquals(TodayDeckAction.Useful, resolveDeckRelease(100f, -120f, 100f, -120f))
        assertEquals(TodayDeckAction.Dismiss, resolveDeckRelease(-10f, -120f, -150f, -200f))
    }

    @Test fun ink_stamps_fade_in_with_the_drag() {
        assertEquals(DeckStampOpacity(0f, 0f, 0f), deckStampOpacity(0f, 0f))
        assertEquals(.5f, deckStampOpacity(62.5f, 0f).useful, .001f)
        assertEquals(1f, deckStampOpacity(-200f, 0f).dismiss, .001f)
        assertEquals(0f, deckStampOpacity(-200f, 0f).useful, .001f)
        assertEquals(0f, deckStampOpacity(0f, -90f).acknowledged, .001f)
        assertEquals(0f, deckStampOpacity(0f, 90f).acknowledged, .001f)
        assertEquals(7f, deckRotationDegrees(100f), .001f)
    }

    @Test fun vertical_drags_on_the_card_are_left_to_the_page_scroll() {
        assertFalse(deckClaimsDrag(overSlopX = 2f, overSlopY = 18f))
        assertFalse(deckClaimsDrag(overSlopX = 2f, overSlopY = -18f))
        assertTrue(deckClaimsDrag(overSlopX = 18f, overSlopY = 4f))
        assertTrue(deckClaimsDrag(overSlopX = -18f, overSlopY = 10f))
        assertFalse(deckClaimsDrag(overSlopX = 8f, overSlopY = -12f))
    }

    @Test fun deck_empty_state_distinguishes_cleared_from_empty() {
        assertEquals("All Dispatches Cleared", deckEmptyTitle(3))
        assertEquals("No Dispatches in This Stack", deckEmptyTitle(0))
        assertEquals("You've triaged all 3 items in this stack.", deckEmptyMessage(TodayDeckTab.All, 3))
        assertEquals("There are currently no For You cards in today's deck.", deckEmptyMessage(TodayDeckTab.ForYou, 0))
        assertEquals("There are currently no Worth a Look cards in today's deck.", deckEmptyMessage(TodayDeckTab.Worth, 0))
    }

    @Test fun impressions_require_half_the_card_inside_the_window() {
        assertTrue(isImpressionVisible(clippedHeight = 100f, clippedWidth = 300f, fullHeight = 200))
        assertFalse(isImpressionVisible(clippedHeight = 99f, clippedWidth = 300f, fullHeight = 200))
        assertFalse(isImpressionVisible(clippedHeight = 0f, clippedWidth = 0f, fullHeight = 200))
        assertFalse(isImpressionVisible(clippedHeight = 0f, clippedWidth = 0f, fullHeight = 0))
    }

    @Test fun column_counts_pluralise_like_web() {
        assertEquals("1 item", forYouCountLabel(1))
        assertEquals("4 items", forYouCountLabel(4))
        assertEquals("1 spark", worthCountLabel(1))
        assertEquals("0 sparks", worthCountLabel(0))
        assertEquals("6", deliverableCountLabel(6))
        assertEquals("66", deliverableCountLabel(66))
    }
}
