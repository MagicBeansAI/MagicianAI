package ai.magicbeans.magdroid.ui

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ButtonColors
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CheckboxColors
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.SwitchColors
import androidx.compose.material3.SwitchDefaults
import androidx.compose.runtime.Composable
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
/*
 * Material mapping: `primary` stays the bright accent because Material also
 * paints `primary` as *content* on the page background (TextButton,
 * OutlinedButton, progress, radio, focus outlines and the text cursor), and the
 * deeper night fill falls under 4.5:1 there. `onPrimary` is [Palette.onAccent]
 * (white at night), so a filled control left on Material's default
 * `primary` container would put white on the bright accent. Filled controls
 * therefore route through [magicanButtonColors], [magicanSwitchColors] and
 * [magicanCheckboxColors], which fill with [Palette.accentFill];
 * `ButtonColorContractTest` keeps every filled call site on them.
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

/** A filled (primary) button: [Palette.accentFill] with [Palette.onAccent] content. */
@Composable
internal fun magicanButtonColors(): ButtonColors = ButtonDefaults.buttonColors(
    containerColor = activePalette.accentFill,
    contentColor = activePalette.onAccent,
)

/**
 * A checked switch: the on-accent thumb rides an [Palette.accentFill] track.
 * Sites that restyle the unchecked state pass the same two checked colours to
 * `SwitchDefaults.colors` themselves (Material's defaults are tokens, not
 * `Unspecified`, so they cannot be threaded through here).
 */
@Composable
internal fun magicanSwitchColors(): SwitchColors = SwitchDefaults.colors(
    checkedThumbColor = activePalette.onAccent,
    checkedTrackColor = activePalette.accentFill,
)

/** A checked box: [Palette.onAccent] check mark on [Palette.accentFill]. */
@Composable
internal fun magicanCheckboxColors(): CheckboxColors = CheckboxDefaults.colors(
    checkedColor = activePalette.accentFill,
    checkmarkColor = activePalette.onAccent,
)

/**
 * The theme system, ported from `magios/Magios/ThemeManager.swift`.
 *
 * Twelve families, each with a day and a night variant, and the app's own
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
    /**
     * Foreground for content on an accent *fill*. Night variants use white,
     * which is why they also carry a deeper [accentFill].
     */
    val onAccent: Color,
    val softBackground: Color,
    val softBackgroundAlpha: Float,
    /** The theme's typeface. Empty means the platform default. */
    val font: String,
    /** Web `--accent-secondary`; null for themes whose web block predates it. */
    val secondaryAccent: Color? = null,
    /** Web `--status-running` (= the theme's `--color-info`); null falls back to [info]. */
    val statusRunning: Color? = null,
    /** Web `--wash-hero` stops (three tints, then the base); null for older themes. */
    val wash: List<Color>? = null,
    /**
     * The background of every surface whose content is [onAccent]: primary
     * buttons, the owner's chat bubble, selected chips and segments, badges,
     * send. Day variants keep the accent itself; night variants carry the
     * accent's hue and saturation (HSL) with the lightness lowered until white
     * reaches 4.5:1 (WCAG). The bright [accent] stays for accent-tinted text,
     * icons, strokes, outlines, progress and tints.
     */
    val accentFill: Color = accent,
    /** Pressed [accentFill]; on night variants darkened further (about 5.5:1 with white). */
    val accentFillPressed: Color = accentHover,
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

    /** The theme's second accent, as iOS `secondaryAccentColor`. */
    val secondaryAccentColor: Color get() = secondaryAccent ?: discovery

    /** A running task's colour: the theme's blue, never the accent. */
    val running: Color get() = statusRunning ?: info

    /**
     * The fill for a control tinted [color]: the accent becomes [accentFill]
     * so its on-accent label reads; any other colour fills as itself.
     */
    fun fillFor(color: Color): Color = if (color == accent) accentFill else color

    /** Accent actions retain the theme token; semantic fills choose contrasting text. */
    fun foregroundFor(color: Color): Color = if (color == accent) onAccent else chatContrastingTextColor(color)

    /** The background wash stops; a wash is never flat (background → surface). */
    val washStops: List<Color> get() = wash ?: listOf(background, surface)
}

/**
 * Rec. 601 luma (0.299 / 0.587 / 0.114), the exact weights iOS's
 * `ThemeManager.luminance(of:)` uses to decide a theme is dark — the two
 * clients must classify a palette identically or their derived colours
 * (borders, light/dark semantic variants) drift apart.
 */
private fun Color.luminance(): Float = 0.299f * red + 0.587f * green + 0.114f * blue

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

    /** The pastel families lead the picker; Sorbet is the default. */
    val families: List<ThemeFamily> = listOf(
        ThemeFamily("Sorbet", light = "sorbet", dark = "sorbet-dark"),
        ThemeFamily("Sky", light = "sky", dark = "sky-dark"),
        ThemeFamily("Candy", light = "candy", dark = "candy-dark"),
        ThemeFamily("Longhand", light = "longhand", dark = "longhand-dark"),
        ThemeFamily("Arcane Terminal", light = "arcane-terminal-light", dark = "arcane-terminal"),
        ThemeFamily("Retro 16-bit", light = "retro-16bit-light", dark = "retro-16bit"),
        ThemeFamily("Mario 8-bit", light = "mario-8bit", dark = "mario-8bit-dark"),
        ThemeFamily("Risograph", light = "risograph", dark = "risograph-dark"),
        ThemeFamily("Mixtape", light = "mixtape", dark = "mixtape-dark"),
        ThemeFamily("Mono", light = "mono", dark = "mono-dark"),
        ThemeFamily("2D Cartoon", light = "cartoon", dark = "cartoon-dark"),
        ThemeFamily("Jarvis", light = "jarvis-light", dark = "jarvis"),
    )

    val palettes: Map<String, Palette> = mapOf(
        "sorbet" to Palette(
            background = Color(0xFFFFF9F4),
            surface = Color(0xFFFFF4EC),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF2B2230),
            secondaryText = Color(0xFF5A4E60),
            accent = Color(0xFFE8505F),
            accentHover = Color(0xFFD63E4E),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFFFBEEE6),
            softBackgroundAlpha = 1.0f,
            font = "Nunito",
            secondaryAccent = Color(0xFF7A5CF0),
            statusRunning = Color(0xFF4A78E0),
            wash = listOf(Color(0xFFFFE1E4), Color(0xFFECE5FF), Color(0xFFDCF5EA), Color(0xFFFFF9F4)),
        ),
        "sorbet-dark" to Palette(
            background = Color(0xFF1D1622),
            surface = Color(0xFF251C2B),
            elevated = Color(0xFF332839),
            text = Color(0xFFF6ECF4),
            secondaryText = Color(0xFFD6C6D6),
            accent = Color(0xFFFF8A94),
            accentHover = Color(0xFFFF9FA8),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFFED0014),
            accentFillPressed = Color(0xFFD40012),
            softBackground = Color(0xFF2F2436),
            softBackgroundAlpha = 1.0f,
            font = "Nunito",
            secondaryAccent = Color(0xFFB9A3FF),
            statusRunning = Color(0xFF8FB0FF),
            wash = listOf(Color(0xFF4A2530), Color(0xFF36295C), Color(0xFF1D4235), Color(0xFF1D1622)),
        ),
        "sky" to Palette(
            background = Color(0xFFF7F9FF),
            surface = Color(0xFFEEF3FF),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF1E2440),
            secondaryText = Color(0xFF454D6E),
            accent = Color(0xFF5A68F0),
            accentHover = Color(0xFF4A57E0),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFFE6EDFB),
            softBackgroundAlpha = 1.0f,
            font = "Nunito",
            secondaryAccent = Color(0xFF3D8FE0),
            statusRunning = Color(0xFF3D7FE0),
            wash = listOf(Color(0xFFE4E8FF), Color(0xFFDCECFF), Color(0xFFFFE3EF), Color(0xFFF7F9FF)),
        ),
        "sky-dark" to Palette(
            background = Color(0xFF0F1424),
            surface = Color(0xFF161D33),
            elevated = Color(0xFF222B4A),
            text = Color(0xFFEEF1FF),
            secondaryText = Color(0xFFC3CAE6),
            accent = Color(0xFF8E9AFF),
            accentHover = Color(0xFFA3ADFF),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFF5163FF),
            accentFillPressed = Color(0xFF3C51FF),
            softBackground = Color(0xFF19203A),
            softBackgroundAlpha = 1.0f,
            font = "Nunito",
            secondaryAccent = Color(0xFF8CC4FF),
            statusRunning = Color(0xFF8CC4FF),
            wash = listOf(Color(0xFF262F63), Color(0xFF1C3558), Color(0xFF4A2438), Color(0xFF0F1424)),
        ),
        "candy" to Palette(
            background = Color(0xFFFFFDF8),
            surface = Color(0xFFFFF6FB),
            elevated = Color(0xFFFFFFFF),
            text = Color(0xFF2A1A2E),
            secondaryText = Color(0xFF5A4560),
            accent = Color(0xFFE0418F),
            accentHover = Color(0xFFCC307D),
            onAccent = Color(0xFFFFFFFF),
            softBackground = Color(0xFFFDEEF5),
            softBackgroundAlpha = 1.0f,
            font = "Nunito",
            secondaryAccent = Color(0xFF8A5CF0),
            statusRunning = Color(0xFF4A78E0),
            wash = listOf(Color(0xFFFFD6EA), Color(0xFFCDF5E4), Color(0xFFFFF3B8), Color(0xFFFFFDF8)),
        ),
        "candy-dark" to Palette(
            background = Color(0xFF1A1020),
            surface = Color(0xFF23152B),
            elevated = Color(0xFF331F3E),
            text = Color(0xFFFBEEF6),
            secondaryText = Color(0xFFDCC6D8),
            accent = Color(0xFFFF8FC7),
            accentHover = Color(0xFFFFA6D3),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFFE60073),
            accentFillPressed = Color(0xFFCD0067),
            softBackground = Color(0xFF2A1A33),
            softBackgroundAlpha = 1.0f,
            font = "Nunito",
            secondaryAccent = Color(0xFFC3A8FF),
            statusRunning = Color(0xFF8FB0FF),
            wash = listOf(Color(0xFF4D1F36), Color(0xFF17443A), Color(0xFF4A4214), Color(0xFF1A1020)),
        ),
        "arcane-terminal" to Palette(
            background = Color(0xFF0A0A0F),
            surface = Color(0xFF12121A),
            elevated = Color(0xFF1A1A2E),
            text = Color(0xFFE0E0E0),
            secondaryText = Color(0xFFB0B0B0),
            accent = Color(0xFF00D4AA),
            accentHover = Color(0xFF00F5C4),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFF00856B),
            accentFillPressed = Color(0xFF007760),
            softBackground = Color(0xFF1A1A2E),
            softBackgroundAlpha = 1.0f,
            font = "IBM Plex Mono",
            statusRunning = Color(0xFF00D4FF),
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
            statusRunning = Color(0xFF0891B2),
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
            statusRunning = Color(0xFF4EC0FF),
        ),
        "cartoon-dark" to Palette(
            background = Color(0xFF1A1830),
            surface = Color(0xFF25224A),
            elevated = Color(0xFF2C2956),
            text = Color(0xFFFFF5E1),
            secondaryText = Color(0xFFEDE4CE),
            accent = Color(0xFFFF7EB6),
            accentHover = Color(0xFFFF9EC8),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFFE90065),
            accentFillPressed = Color(0xFFCF005A),
            softBackground = Color(0xFFFFF5E1),
            softBackgroundAlpha = 0.06f,
            font = "Fredoka",
            statusRunning = Color(0xFF8FDCFF),
        ),
        "jarvis" to Palette(
            background = Color(0xFF050A14),
            surface = Color(0xFF0A1220),
            elevated = Color(0xFF0E1828),
            text = Color(0xFFD8ECFF),
            secondaryText = Color(0xFF98B6D6),
            accent = Color(0xFF00D4FF),
            accentHover = Color(0xFF33DFFF),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFF00819C),
            accentFillPressed = Color(0xFF00728A),
            softBackground = Color(0xFF0F1A2E),
            softBackgroundAlpha = 1.0f,
            font = "Manrope",
            statusRunning = Color(0xFF00D4FF),
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
            statusRunning = Color(0xFF0099CC),
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
            statusRunning = Color(0xFF4A6A8A),
        ),
        "longhand-dark" to Palette(
            background = Color(0xFF1A1612),
            surface = Color(0xFF221D18),
            elevated = Color(0xFF2A241E),
            text = Color(0xFFF3EAD6),
            secondaryText = Color(0xFFD6C8A8),
            accent = Color(0xFFD8602E),
            accentHover = Color(0xFFE87A4A),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFFC35324),
            accentFillPressed = Color(0xFFAE4A21),
            softBackground = Color(0xFF322A22),
            softBackgroundAlpha = 1.0f,
            font = "Manrope",
            statusRunning = Color(0xFF6DB1BF),
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
            statusRunning = Color(0xFF3399FF),
        ),
        "mario-8bit-dark" to Palette(
            background = Color(0xFF000000),
            surface = Color(0xFF181818),
            elevated = Color(0xFF2A2A2A),
            text = Color(0xFFFFFFFF),
            secondaryText = Color(0xFFE0E0E0),
            accent = Color(0xFFFBD000),
            accentHover = Color(0xFFFFE040),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFF8B7300),
            accentFillPressed = Color(0xFF7D6700),
            softBackground = Color(0xFFFFFFFF),
            softBackgroundAlpha = 0.08f,
            font = "Pixelify Sans",
            statusRunning = Color(0xFF5599FF),
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
            statusRunning = Color(0xFF3D6E8A),
        ),
        "mixtape-dark" to Palette(
            background = Color(0xFF0F0E0C),
            surface = Color(0xFF1A1815),
            elevated = Color(0xFF2A2520),
            text = Color(0xFFE8C66A),
            secondaryText = Color(0xFFD8B358),
            accent = Color(0xFFFF5040),
            accentHover = Color(0xFFFF6E60),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFFEB1400),
            accentFillPressed = Color(0xFFD11200),
            softBackground = Color(0xFFE8C66A),
            softBackgroundAlpha = 0.08f,
            font = "Special Elite",
            statusRunning = Color(0xFF6DA3C8),
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
            statusRunning = Color(0xFF4A4A4A),
        ),
        "mono-dark" to Palette(
            background = Color(0xFF000000),
            surface = Color(0xFF0A0A0A),
            elevated = Color(0xFF141414),
            text = Color(0xFFFFFFFF),
            secondaryText = Color(0xFFE5E5E5),
            accent = Color(0xFFFFFFFF),
            accentHover = Color(0xFFE5E5E5),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFF757575),
            accentFillPressed = Color(0xFF686868),
            softBackground = Color(0xFFFFFFFF),
            softBackgroundAlpha = 0.04f,
            font = "Inter",
            statusRunning = Color(0xFFCCCCCC),
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
            accentFill = Color(0xFF9E6D00),
            accentFillPressed = Color(0xFF8C6000),
            softBackground = Color(0xFF14110D),
            softBackgroundAlpha = 1.0f,
            font = "JetBrains Mono",
            statusRunning = Color(0xFFFFB000),
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
            statusRunning = Color(0xFF1A1A1A),
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
            statusRunning = Color(0xFF3D5588),
        ),
        "risograph-dark" to Palette(
            background = Color(0xFF14110D),
            surface = Color(0xFF1F1A14),
            elevated = Color(0xFF2A2218),
            text = Color(0xFFF4EFE1),
            secondaryText = Color(0xFFD8D2C1),
            accent = Color(0xFFFF48B0),
            accentHover = Color(0xFFFF70C4),
            onAccent = Color(0xFFFFFFFF),
            accentFill = Color(0xFFE40081),
            accentFillPressed = Color(0xFFCB0074),
            softBackground = Color(0xFFF4EFE1),
            softBackgroundAlpha = 0.06f,
            font = "Manrope",
            statusRunning = Color(0xFF6BA6D8),
        ),
    )

    /**
     * The theme a reader with no saved choice gets, as iOS `defaultThemeID`.
     * Appearance stays System, so a dark device resolves it to `sorbet-dark`.
     */
    const val DEFAULT = "sorbet"

    /**
     * Retired theme ids and the Sorbet variant each now resolves to (the
     * day/night side is kept). Bubbly and Soft Machine were removed, as on iOS.
     */
    private val retiredIds: Map<String, String> = mapOf(
        "bubbly" to "sorbet",
        "bubbly-dark" to "sorbet-dark",
        "soft-machine" to "sorbet",
        "soft-machine-dark" to "sorbet-dark",
    )

    /** The current id for a possibly retired one; live ids pass through. */
    fun migratedId(id: String): String = retiredIds[id] ?: id

    /**
     * The family to start from given what was saved: a saved choice (Longhand
     * included) is kept, a retired one migrates, and nothing saved means Sorbet.
     */
    fun initialFamily(saved: String?): String = saved?.let(::migratedId) ?: DEFAULT

    fun palette(id: String): Palette = palettes[migratedId(id)] ?: palettes.getValue(DEFAULT)

    /**
     * The variant to show, given a family, a mode, and the device's appearance.
     *
     * A family plus a mode resolves to exactly one variant, so the persisted
     * choice survives a device flipping to night without becoming a different
     * theme.
     */
    fun resolve(familyId: String, mode: AppearanceMode, systemIsDark: Boolean): String {
        val id = migratedId(familyId)
        val family = families.firstOrNull { it.id == id || it.dark == id }
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
    // A retired saved id (Bubbly, Soft Machine) migrates to Sorbet and the
    // migrated id is written back, so it is resolved once rather than forever.
    private val _familyId = MutableStateFlow(
        store.getString(KEY_FAMILY, null).let { saved ->
            Themes.initialFamily(saved).also { id ->
                if (saved != null && saved != id) store.edit().putString(KEY_FAMILY, id).apply()
            }
        },
    )
    val familyId: StateFlow<String> = _familyId.asStateFlow()

    private val _mode = MutableStateFlow(
        runCatching { AppearanceMode.valueOf(store.getString(KEY_MODE, null) ?: "") }
            .getOrDefault(AppearanceMode.System),
    )
    val mode: StateFlow<AppearanceMode> = _mode.asStateFlow()

    /** Whether this install has met the account's theme record once already
     * (see [ThemeSync]): the first encounter may declare the local choice;
     * afterwards the account's stored choice always wins. */
    private val _accountSeeded = MutableStateFlow(store.getBoolean(KEY_ACCOUNT_SEEDED, false))
    val accountSeeded: Boolean get() = _accountSeeded.value

    fun markAccountSeeded() {
        _accountSeeded.value = true
        store.edit().putBoolean(KEY_ACCOUNT_SEEDED, true).apply()
    }

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
        private const val KEY_ACCOUNT_SEEDED = "account_seeded"

        @Volatile
        private var instance: ThemeStore? = null

        fun get(context: Context): ThemeStore =
            instance ?: synchronized(this) { instance ?: ThemeStore(context).also { instance = it } }
    }
}
