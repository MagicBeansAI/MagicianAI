package ai.magicbeans.magdroid.identity

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ProductIdentityTest {
    @Test
    fun `generated identity is presentation only`() {
        assertEquals(ProductIdentity.productName.trim(), ProductIdentity.productName)
        assertEquals(ProductIdentity.hostAppName.trim(), ProductIdentity.hostAppName)
        assertEquals(ProductIdentity.assistantFallbackName.trim(), ProductIdentity.assistantFallbackName)
        assertTrue(ProductIdentity.productName.isNotEmpty())
        assertTrue(ProductIdentity.hostAppName.isNotEmpty())
        assertTrue(ProductIdentity.assistantFallbackName.isNotEmpty())
        assertFalse(
            listOf(
                ProductIdentity.productName,
                ProductIdentity.hostAppName,
                ProductIdentity.assistantFallbackName,
            ).joinToString(" ").lowercase().contains("magician"),
        )
    }
}
