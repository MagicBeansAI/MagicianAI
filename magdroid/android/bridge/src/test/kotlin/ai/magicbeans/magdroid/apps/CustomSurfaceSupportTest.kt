package ai.magicbeans.magdroid.apps

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CustomSurfaceSupportTest {
    @Test
    fun `the capability stays closed on this client`() {
        assertFalse(CustomSurfaceSupport.supported)
        assertEquals(
            "Custom surfaces are not supported on this client.",
            CustomSurfaceSupport.unsupportedNotice
        )
    }

    @Test
    fun `declaring packages render the closed notice and undeclared packages render nothing new`() {
        assertTrue(CustomSurfaceSupport.rendersUnsupportedNotice(declaresCustomSurface = true))
        assertFalse(CustomSurfaceSupport.rendersUnsupportedNotice(declaresCustomSurface = false))
    }
}
