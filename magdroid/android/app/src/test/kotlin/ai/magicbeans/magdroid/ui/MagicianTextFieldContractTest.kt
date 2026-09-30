package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertTrue
import org.junit.Test

class MagicianTextFieldContractTest {
    @Test
    fun `shared android fields remain denser than stock material fields`() {
        assertTrue(COMPACT_INPUT_HEIGHT_DP < 56)
        assertTrue(COMPACT_LABELED_INPUT_HEIGHT_DP < 56)
        assertTrue(COMPACT_INPUT_FONT_SP <= 13)
        assertTrue(COMPACT_INPUT_LINE_STEP_DP <= 18)
    }
}
