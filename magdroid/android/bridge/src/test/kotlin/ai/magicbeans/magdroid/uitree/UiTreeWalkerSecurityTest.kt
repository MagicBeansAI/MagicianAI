package ai.magicbeans.magdroid.uitree

import org.junit.Assert.assertEquals
import org.junit.Test

class UiTreeWalkerSecurityTest {
    @Test
    fun `password node values are redacted before projection and identity`() {
        assertEquals(
            "" to "",
            accessibilityTextFields(
                redactedAncestor = false,
                isPassword = true,
                isEditable = true,
                text = "4111 1111 1111 1111",
                contentDescription = "payment card 4111 1111 1111 1111"
            )
        )
    }

    @Test
    fun `non-password payment input values are also redacted`() {
        assertEquals(
            "" to "",
            accessibilityTextFields(
                redactedAncestor = false,
                isPassword = false,
                isEditable = true,
                text = "4111 1111 1111 1111",
                contentDescription = "Card number"
            )
        )
    }

    @Test
    fun `ordinary semantic labels remain available`() {
        assertEquals(
            "Continue" to "Submit form",
            accessibilityTextFields(
                redactedAncestor = false,
                isPassword = false,
                isEditable = false,
                text = "Continue",
                contentDescription = "Submit form"
            )
        )
    }

    @Test
    fun `ordinary child of a password or editable container remains redacted`() {
        assertEquals(
            "" to "",
            accessibilityTextFields(
                redactedAncestor = true,
                isPassword = false,
                isEditable = false,
                text = "descendant card number 4111 1111 1111 1111",
                contentDescription = "descendant payment secret"
            )
        )
    }


    @Test
    fun `apps field projection stops on an exact utf8 byte boundary`() {
        val projected = boundedAccessibilityUtf8("😀😀😀|secret".repeat(10_000), 10)
        assertEquals("😀😀", projected)
        assert(projected.toByteArray(Charsets.UTF_8).size <= 10)
    }

    @Test
    fun `foreign ime or overlay package is not attributed to the foreground app`() {
        assert(accessibilityNodeBelongsToPackage("com.example.reviewed", "com.example.reviewed"))
        assert(!accessibilityNodeBelongsToPackage("com.android.inputmethod", "com.example.reviewed"))
        assert(!accessibilityNodeBelongsToPackage(null, "com.example.reviewed"))
    }

    @Test
    fun `clipped accessibility bounds are intersected with the display`() {
        assertEquals(
            AppsVisibleBounds(0, 10, 1080, 1920),
            intersectAccessibilityBounds(-40, 10, 1200, 2100, 1080, 1920),
        )
        assertEquals(null, intersectAccessibilityBounds(-100, 10, -1, 100, 1080, 1920))
    }
}
