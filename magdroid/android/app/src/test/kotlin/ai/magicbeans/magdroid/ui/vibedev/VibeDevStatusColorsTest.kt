package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.Themes
import ai.magicbeans.magdroid.vibedev.VibeDevHealthLevel
import org.junit.Assert.assertEquals
import org.junit.Test

class VibeDevStatusColorsTest {
    @Test fun project_health_uses_danger_for_errors_and_warning_otherwise() {
        val day = Themes.palette("longhand")
        assertEquals(day.danger, VibeDevHealthLevel.Error.color(day))
        assertEquals(day.warning, VibeDevHealthLevel.Warning.color(day))
    }
}
