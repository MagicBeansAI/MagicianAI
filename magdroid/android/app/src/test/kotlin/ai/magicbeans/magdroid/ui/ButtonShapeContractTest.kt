package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertTrue
import org.junit.Test

class ButtonShapeContractTest {
    @Test
    fun standard_actions_remain_softly_squared_instead_of_capsules() {
        assertTrue(MAGICAN_BUTTON_CORNER_DP in 6..10)
    }
}
