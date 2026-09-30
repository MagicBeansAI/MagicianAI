package ai.magicbeans.magdroid.ui

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp

/**
 * Filled and outlined actions use a restrained Magican silhouette instead of
 * Material 3's capsule. Pills remain reserved for chips and compact labels.
 */
internal const val MAGICAN_BUTTON_CORNER_DP = 8
internal val MagicanButtonShape = RoundedCornerShape(MAGICAN_BUTTON_CORNER_DP.dp)

/**
 * Material-owned surfaces must resolve from the same palette as Magican-owned
 * screens. Without this, dialogs, menus, controls and transient containers stay
 * on Material's light defaults while the selected family changes around them.
 */
internal fun magicanColorScheme(palette: Palette): ColorScheme {
    val base = if (palette.isDark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = palette.accent,
        onPrimary = palette.onAccent,
        primaryContainer = palette.soft,
        onPrimaryContainer = palette.text,
        secondary = palette.info,
        onSecondary = palette.background,
        secondaryContainer = palette.surface,
        onSecondaryContainer = palette.text,
        tertiary = palette.discovery,
        onTertiary = palette.background,
        background = palette.background,
        onBackground = palette.text,
        surface = palette.elevated,
        onSurface = palette.text,
        surfaceVariant = palette.surface,
        surfaceBright = palette.elevated,
        surfaceDim = palette.surface,
        surfaceContainerLowest = palette.background,
        surfaceContainerLow = palette.elevated,
        surfaceContainer = palette.surface,
        surfaceContainerHigh = palette.elevated,
        surfaceContainerHighest = palette.soft,
        onSurfaceVariant = palette.secondaryText,
        outline = palette.controlBorder,
        outlineVariant = palette.cardBorder,
        error = palette.danger,
        onError = palette.background,
        inverseSurface = palette.text,
        inverseOnSurface = palette.background,
        inversePrimary = palette.accentHover,
        surfaceTint = Color.Transparent,
        scrim = Color.Black,
    )
}

/**
 * The theme system, ported from `magios/Magios/ThemeManager.swift`.
 *
 * Eleven families, each with a day and a night variant, and the app's own
 * appearance mode on top: follow the device, or force one. The variant ids match
 * the web's theme ids, so a theme chosen on any surface means the same thing.
 *
 * The palettes are the same seven colours iOS publishes, plus the three tokens
 * it derives per theme rather than globally — the accent's foreground, the soft
 * background, and the primary font. The four-role font contract is defined in
 * `ThemeTypography.kt`. Deriving these from the accent would lose
 * exactly the themes that need them: a soft background computed from the surface
 * makes segmented tracks vanish on the flat themes.
 */
data class Palette(
    val background: Color,
    val surface: Color,
    val elevated: Color,
    val text: Color,
    val secondaryText: Color,
    val accent: Color,
    val accentHover: Color,
    val onAccent: Color,
    val softBackground: Color,
    val softBackgroundAlpha: Float,
    /** The theme's typeface. Empty means the platform default. */
    val font: String,
) {
    /** True when the background is dark, which decides the derived opacities. */
    val isDark: Boolean get() = background.luminance() < 0.5f

    // Derived exactly as iOS derives them, so a theme cannot look different on
    // one client because a border was hard-coded rather than computed.
    val card: Color get() = elevated
    val cardBorder: Color get() = secondaryText.copy(alpha = if (isDark) 0.28f else 0.16f)
    val control: Color get() = surface
    val controlBorder: Color get() = secondaryText.copy(alpha = if (isDark) 0.34f else 0.20f)
    val soft: Color get() = softBackground.copy(alpha = softBackgroundAlpha)

    val success: Color get() = if (isDark) Color(0xFF68D391) else Color(0xFF16784A)
    val warning: Color get() = if (isDark) Color(0xFFF6C453) else Color(0xFF9A5B00)
    val danger: Color get() = if (isDark) Color(0xFFFF7B7B) else Color(0xFFB4232F)
    val info: Color get() = if (isDark) Color(0xFF79B8FF) else Color(0xFF1769AA)
    val discovery: Color get() = if (isDark) Color(0xFFD3A6FF) else Color(0xFF7040A0)
}

/** Rec. 709 luma, the same rule iOS uses to decide a theme is dark. */
private fun Color.luminance(): Float = 0.2126f * red + 0.7152f * green + 0.0722f * blue

/** A family is one identity with two variants; Settings lists families. */
data class ThemeFamily(val name: String, val light: String, val dark: String) {
    /** The light variant is the stable id, matching the web's persisted value. */
    val id: String get() = light
}

/** Follow the device, or force one variant. */
enum class AppearanceMode(val label: String) {
    System("System"),
    Day("Day"),
    Night("Night"),
}

object Themes {

    val families: List<ThemeFamily> = listOf(
        ThemeFamily("Longhand", light = "longhand", dark = "longhand-dark"),
        ThemeFamily("Soft Machine", light = "soft-machine", dark = "soft-machine-dark"),
        ThemeFamily("Arcane Terminal", light = "arcane-terminal-light", dark = "arcane-terminal"),
        ThemeFamily("Retro 16-bit", light = "retro-16bit-light", dark = "retro-16bit"),
        ThemeFamily("Mario 8-bit", light = "mario-8bit", dark = "mario-8bit-dark"),
        ThemeFamily("Risograph", light = "risograph", dark = "risograph-dark"),
        ThemeFamily("Mixtape", light = "mixtape", dark = "mixtape-dark"),
        ThemeFamily("Mono", light = "mono", dark = "mono-dark"),
        ThemeFamily("2D Cartoon", light = "cartoon", dark = "cartoon-dark"),
        ThemeFamily("Bubbly", light = "bubbly", dark = "bubbly-dark"),
        ThemeFamily("Jarvis", light = "jarvis-light", dark = "jarvis"),
    )

    val palettes: Map<String, Palette> = mapOf(
        "arcane-terminal" to Palette(
            background = Color(0xFF0A0A0F),
            surface = Color(0xFF12121A),
            elevated = Color(0xFF1A1A2E),
            text = Color(0xFFE0E0E0),
            secondaryText = Color(0xFFB0B0B0),
            accent = Color(0xFF00D4AA),
            accentHover = Color(0xFF00F5C4),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFF1A1A2E),
            softBackgroundAlpha = 1.0f,
            font = "IBM Plex Mono",
        ),
        "arcane-terminal-light" to Palette(
            background = Color(0xFFF6F8FA),
            surface = Color(0xFFEEF1F4),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF0A0A0F),
            secondaryText = Color(0xFF2A2A35),
            accent = Color(0xFF007A66),
            accentHover = Color(0xFF00604F),
            onAccent = Color(0xFFF6F8FA),
            softBackground = Color(0xFFE2E7EC),
            softBackgroundAlpha = 1.0f,
            font = "IBM Plex Mono",
        ),
        "bubbly" to Palette(
            background = Color(0xFFFDFCF8),
            surface = Color(0xFFFFF8F2),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF2D3436),
            secondaryText = Color(0xFF5F6668),
            accent = Color(0xFFFF6B6B),
            accentHover = Color(0xFFFF5252),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFFF7F3EB),
            softBackgroundAlpha = 1.0f,
            font = "Quicksand",
        ),
        "bubbly-dark" to Palette(
            background = Color(0xFF171B1D),
            surface = Color(0xFF1D2326),
            elevated = Color(0xFF252C30),
            text = Color(0xFFF7F1E8),
            secondaryText = Color(0xFFD8CFC2),
            accent = Color(0xFFFF7B7B),
            accentHover = Color(0xFFFF9A9A),
            onAccent = Color(0xFF171B1D),
            softBackground = Color(0xFF20272A),
            softBackgroundAlpha = 1.0f,
            font = "Quicksand",
        ),
        "cartoon" to Palette(
            background = Color(0xFF9DD5FF),
            surface = Color(0xFF84C8FF),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF0A0A08),
            secondaryText = Color(0xFF1A1A14),
            accent = Color(0xFFFF5499),
            accentHover = Color(0xFFFF3886),
            onAccent = Color(0xFF0A0A08),
            softBackground = Color(0xFF0A0A08),
            softBackgroundAlpha = 0.06f,
            font = "Fredoka",
        ),
        "cartoon-dark" to Palette(
            background = Color(0xFF1A1830),
            surface = Color(0xFF25224A),
            elevated = Color(0xFF2C2956),
            text = Color(0xFFFFF5E1),
            secondaryText = Color(0xFFEDE4CE),
            accent = Color(0xFFFF7EB6),
            accentHover = Color(0xFFFF9EC8),
            onAccent = Color(0xFF1A1830),
            softBackground = Color(0xFFFFF5E1),
            softBackgroundAlpha = 0.06f,
            font = "Fredoka",
        ),
        "jarvis" to Palette(
            background = Color(0xFF050A14),
            surface = Color(0xFF0A1220),
            elevated = Color(0xFF0E1828),
            text = Color(0xFFD8ECFF),
            secondaryText = Color(0xFF98B6D6),
            accent = Color(0xFF00D4FF),
            accentHover = Color(0xFF33DFFF),
            onAccent = Color(0xFF03121F),
            softBackground = Color(0xFF0F1A2E),
            softBackgroundAlpha = 1.0f,
            font = "Manrope",
        ),
        "jarvis-light" to Palette(
            background = Color(0xFFF0F7FF),
            surface = Color(0xFFE6F1FA),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF062035),
            secondaryText = Color(0xFF2D4A6B),
            accent = Color(0xFF0099CC),
            accentHover = Color(0xFF00B3E8),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFFDCEAF5),
            softBackgroundAlpha = 1.0f,
            font = "Manrope",
        ),
        "longhand" to Palette(
            background = Color(0xFFF3EAD6),
            surface = Color(0xFFEDE1C4),
            elevated = Color(0xFFFAF3E0),
            text = Color(0xFF1A1612),
            secondaryText = Color(0xFF3A2F24),
            accent = Color(0xFFA04020),
            accentHover = Color(0xFF732C14),
            onAccent = Color(0xFFFAF3E0),
            softBackground = Color(0xFFE3D3AC),
            softBackgroundAlpha = 1.0f,
            font = "Manrope",
        ),
        "longhand-dark" to Palette(
            background = Color(0xFF1A1612),
            surface = Color(0xFF221D18),
            elevated = Color(0xFF2A241E),
            text = Color(0xFFF3EAD6),
            secondaryText = Color(0xFFD6C8A8),
            accent = Color(0xFFD8602E),
            accentHover = Color(0xFFE87A4A),
            onAccent = Color(0xFF1A1612),
            softBackground = Color(0xFF322A22),
            softBackgroundAlpha = 1.0f,
            font = "Manrope",
        ),
        "mario-8bit" to Palette(
            background = Color(0xFF5C94FC),
            surface = Color(0xFF88B8FC),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF000000),
            secondaryText = Color(0xFF1A1A1A),
            accent = Color(0xFFE40000),
            accentHover = Color(0xFFB80000),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFF000000),
            softBackgroundAlpha = 0.08f,
            font = "Pixelify Sans",
        ),
        "mario-8bit-dark" to Palette(
            background = Color(0xFF000000),
            surface = Color(0xFF181818),
            elevated = Color(0xFF2A2A2A),
            text = Color(0xFFFFFFFF),
            secondaryText = Color(0xFFE0E0E0),
            accent = Color(0xFFFBD000),
            accentHover = Color(0xFFFFE040),
            onAccent = Color(0xFF000000),
            softBackground = Color(0xFFFFFFFF),
            softBackgroundAlpha = 0.08f,
            font = "Pixelify Sans",
        ),
        "mixtape" to Palette(
            background = Color(0xFFE9D59F),
            surface = Color(0xFFDCC692),
            elevated = Color(0xFFF4E4B3),
            text = Color(0xFF2A1A0E),
            secondaryText = Color(0xFF3E2A18),
            accent = Color(0xFFC8362A),
            accentHover = Color(0xFFA82418),
            onAccent = Color(0xFFF4E4B3),
            softBackground = Color(0xFF2A1A0E),
            softBackgroundAlpha = 0.06f,
            font = "Special Elite",
        ),
        "mixtape-dark" to Palette(
            background = Color(0xFF0F0E0C),
            surface = Color(0xFF1A1815),
            elevated = Color(0xFF2A2520),
            text = Color(0xFFE8C66A),
            secondaryText = Color(0xFFD8B358),
            accent = Color(0xFFFF5040),
            accentHover = Color(0xFFFF6E60),
            onAccent = Color(0xFF0F0E0C),
            softBackground = Color(0xFFE8C66A),
            softBackgroundAlpha = 0.08f,
            font = "Special Elite",
        ),
        "mono" to Palette(
            background = Color(0xFFF4F4F5),
            surface = Color(0xFFE4E4E7),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF000000),
            secondaryText = Color(0xFF1A1A1A),
            accent = Color(0xFF000000),
            accentHover = Color(0xFF1A1A1A),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFF000000),
            softBackgroundAlpha = 0.05f,
            font = "Inter",
        ),
        "mono-dark" to Palette(
            background = Color(0xFF000000),
            surface = Color(0xFF0A0A0A),
            elevated = Color(0xFF141414),
            text = Color(0xFFFFFFFF),
            secondaryText = Color(0xFFE5E5E5),
            accent = Color(0xFFFFFFFF),
            accentHover = Color(0xFFE5E5E5),
            onAccent = Color(0xFF000000),
            softBackground = Color(0xFFFFFFFF),
            softBackgroundAlpha = 0.04f,
            font = "Inter",
        ),
        "retro-16bit" to Palette(
            background = Color(0xFF0C0A08),
            surface = Color(0xFF14110D),
            elevated = Color(0xFF1A1610),
            text = Color(0xFFFFB000),
            secondaryText = Color(0xFFFFCC00),
            accent = Color(0xFFFFB000),
            accentHover = Color(0xFFFFD000),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFF14110D),
            softBackgroundAlpha = 1.0f,
            font = "JetBrains Mono",
        ),
        "retro-16bit-light" to Palette(
            background = Color(0xFFF5F5F0),
            surface = Color(0xFFEBEBE6),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF1A1A1A),
            secondaryText = Color(0xFF333333),
            accent = Color(0xFF1A1A1A),
            accentHover = Color(0xFF000000),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFFE0E0DB),
            softBackgroundAlpha = 1.0f,
            font = "JetBrains Mono",
        ),
        "risograph" to Palette(
            background = Color(0xFFF5EFE1),
            surface = Color(0xFFEDE5D2),
            elevated = Color(0xFFFBF6E8),
            text = Color(0xFF1A1612),
            secondaryText = Color(0xFF2D2820),
            accent = Color(0xFFFF48B0),
            accentHover = Color(0xFFE02E90),
            onAccent = Color(0xFFFBF6E8),
            softBackground = Color(0xFF1A1612),
            softBackgroundAlpha = 0.06f,
            font = "Manrope",
        ),
        "risograph-dark" to Palette(
            background = Color(0xFF14110D),
            surface = Color(0xFF1F1A14),
            elevated = Color(0xFF2A2218),
            text = Color(0xFFF4EFE1),
            secondaryText = Color(0xFFD8D2C1),
            accent = Color(0xFFFF48B0),
            accentHover = Color(0xFFFF70C4),
            onAccent = Color(0xFF14110D),
            softBackground = Color(0xFFF4EFE1),
            softBackgroundAlpha = 0.06f,
            font = "Manrope",
        ),
        "soft-machine" to Palette(
            background = Color(0xFFFEFDFB),
            surface = Color(0xFFF8F6F2),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF2D2A26),
            secondaryText = Color(0xFF4A4540),
            accent = Color(0xFFE85D5D),
            accentHover = Color(0xFFD04F4F),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFFF3F0EA),
            softBackgroundAlpha = 1.0f,
            font = "Quicksand",
        ),
        "soft-machine-dark" to Palette(
            background = Color(0xFF171B1D),
            surface = Color(0xFF1D2326),
            elevated = Color(0xFF242B2F),
            text = Color(0xFFF7F1E8),
            secondaryText = Color(0xFFD7CFC2),
            accent = Color(0xFFFF7B7B),
            accentHover = Color(0xFFFF9393),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFF20272A),
            softBackgroundAlpha = 1.0f,
            font = "Quicksand",
        ),
    )

    /** The default, matching iOS's opening palette. */
    const val DEFAULT = "longhand"

    fun palette(id: String): Palette = palettes[id] ?: palettes.getValue(DEFAULT)

    /**
     * The variant to show, given a family, a mode, and the device's appearance.
     *
     * A family plus a mode resolves to exactly one variant, so the persisted
     * choice survives a device flipping to night without becoming a different
     * theme.
     */
    fun resolve(familyId: String, mode: AppearanceMode, systemIsDark: Boolean): String {
        val family = families.firstOrNull { it.id == familyId || it.dark == familyId }
            ?: families.first()
        return when (mode) {
            AppearanceMode.Day -> family.light
            AppearanceMode.Night -> family.dark
            AppearanceMode.System -> if (systemIsDark) family.dark else family.light
        }
    }
}

/**
 * The active palette.
 *
 * Snapshot state rather than a composition local: reading it inside composition
 * subscribes for recomposition, and reading it outside one is still legal.
 * Colours get read from plain helpers as well as composables, and a mechanism
 * that forbids that breaks working code to serve itself.
 */
var activePalette: Palette by mutableStateOf(Themes.palette(Themes.DEFAULT))
    private set

/** Switch the whole app's palette. Every screen recomposes from this one write. */
fun applyTheme(familyId: String, mode: AppearanceMode, systemIsDark: Boolean) {
    activePalette = Themes.palette(Themes.resolve(familyId, mode, systemIsDark))
}

/**
 * The chosen family and mode, which outlive the process.
 *
 * Stored locally. iOS also persists a theme to the backend for the web to share;
 * this client reads its own choice only, so a theme set here does not yet follow
 * the account.
 */
class ThemeStore private constructor(context: Context) {

    private val store = context.applicationContext
        .getSharedPreferences("magdroid.theme", Context.MODE_PRIVATE)

    // Observable, not plain getters. Two screens read this — the activity that
    // applies the theme and the settings screen that changes it — and when each
    // remembered its own snapshot they drifted: a family chosen in Settings was
    // reverted by the activity re-applying its stale copy the next time the
    // device flipped light or dark. One source, watched by both.
    private val _familyId = MutableStateFlow(store.getString(KEY_FAMILY, null) ?: Themes.DEFAULT)
    val familyId: StateFlow<String> = _familyId.asStateFlow()

    private val _mode = MutableStateFlow(
        runCatching { AppearanceMode.valueOf(store.getString(KEY_MODE, null) ?: "") }
            .getOrDefault(AppearanceMode.System),
    )
    val mode: StateFlow<AppearanceMode> = _mode.asStateFlow()

    fun setFamily(id: String) {
        _familyId.value = id
        store.edit().putString(KEY_FAMILY, id).apply()
    }

    fun setMode(value: AppearanceMode) {
        _mode.value = value
        store.edit().putString(KEY_MODE, value.name).apply()
    }

    companion object {
        private const val KEY_FAMILY = "family"
        private const val KEY_MODE = "mode"

        @Volatile
        private var instance: ThemeStore? = null

        fun get(context: Context): ThemeStore =
            instance ?: synchronized(this) { instance ?: ThemeStore(context).also { instance = it } }
    }
}
