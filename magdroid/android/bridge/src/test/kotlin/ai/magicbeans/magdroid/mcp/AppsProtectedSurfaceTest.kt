package ai.magicbeans.magdroid.mcp

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AppsProtectedSurfaceTest {
    @Test
    fun `unknown window fails closed independently of protected app configuration`() {
        assertEquals(
            "unknown_window",
            appsProtectedSystemSurfaceKind(
                foregroundPackage = "com.example.allowed",
                launcherPackage = "com.example.launcher",
                windowKind = AppsAccessibilityWindowKind.Unknown,
            ),
        )
    }

    @Test
    fun `only an attributed application window is admitted`() {
        assertNull(
            appsProtectedSystemSurfaceKind(
                foregroundPackage = "com.example.allowed",
                launcherPackage = "com.example.launcher",
                windowKind = AppsAccessibilityWindowKind.Application,
            ),
        )
        assertEquals(
            "system_window",
            appsProtectedSystemSurfaceKind(
                foregroundPackage = "com.example.allowed",
                launcherPackage = null,
                windowKind = AppsAccessibilityWindowKind.NonApplication,
            ),
        )
    }

    @Test
    fun `system UI and launcher are denied without relying on a protected list`() {
        assertEquals(
            "system_ui",
            appsProtectedSystemSurfaceKind(
                foregroundPackage = "com.android.systemui",
                launcherPackage = null,
                windowKind = AppsAccessibilityWindowKind.Application,
            ),
        )
        assertEquals(
            "launcher_or_recents",
            appsProtectedSystemSurfaceKind(
                foregroundPackage = "com.example.launcher",
                launcherPackage = "com.example.launcher",
                windowKind = AppsAccessibilityWindowKind.Application,
            ),
        )
    }
}
