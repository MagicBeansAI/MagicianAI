package ai.magicbeans.magdroid.widget

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MagicanGlanceWorkerTest {
    @Test
    fun `a failed refresh gets one retry rather than an unbounded battery loop`() {
        assertTrue(shouldRetryGlanceRefresh(0))
        assertFalse(shouldRetryGlanceRefresh(1))
        assertFalse(shouldRetryGlanceRefresh(12))
    }
}
