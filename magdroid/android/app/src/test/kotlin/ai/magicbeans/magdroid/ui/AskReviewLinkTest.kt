package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.WorkSection
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * An ask's review button (a Relentless review's "Review draft") routes its
 * server-relative link in-app through Work deep-link routing rather than
 * handing a path with no host to the browser.
 */
class AskReviewLinkTest {
    @Test
    fun `a Relentless review link opens the Work Quality run`() {
        val target = WorkDeepLinks.parse("/work?view=quality&run=run-7&task=task-1") as WorkLinkTarget.Library
        assertEquals(WorkSection.Quality, target.section)
        assertEquals("quality-run", target.route?.kind)
        assertEquals("run-7", target.route?.id)
    }

    @Test
    fun `a link Work does not route is left to the browser`() {
        assertNull(WorkDeepLinks.parse("https://example.test/diff/1"))
    }
}
