package ai.magicbeans.magdroid.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.sp
import ai.magicbeans.magdroid.R

/** The four font roles published by the Web theme contract. */
internal data class ThemeFontNames(
    val brand: String,
    val display: String,
    val body: String,
    val mono: String,
)

internal data class ThemeFontFamilies(
    val brand: FontFamily,
    val display: FontFamily,
    val body: FontFamily,
    val mono: FontFamily,
)

// Outfit, Manrope and Geist Mono ship as static instances cut from their
// variable files (400–800, the weights the UI uses). Loading several weights
// of one variable resource let Android reuse whichever face it cached first,
// and these files default to Thin/ExtraLight, so Bold intermittently rendered
// hairline across the app. Weights outside 400–800 resolve to the nearest.
private fun staticFamily(regular: Int, medium: Int, semibold: Int, bold: Int, extrabold: Int): FontFamily = FontFamily(
    Font(regular, FontWeight.Normal),
    Font(medium, FontWeight.Medium),
    Font(semibold, FontWeight.SemiBold),
    Font(bold, FontWeight.Bold),
    Font(extrabold, FontWeight.ExtraBold),
)

private val outfit = staticFamily(
    R.font.outfit_regular, R.font.outfit_medium, R.font.outfit_semibold, R.font.outfit_bold, R.font.outfit_extrabold,
)
private val manrope = staticFamily(
    R.font.manrope_regular, R.font.manrope_medium, R.font.manrope_semibold, R.font.manrope_bold, R.font.manrope_extrabold,
)
private val geistMono = staticFamily(
    R.font.geist_mono_regular, R.font.geist_mono_medium, R.font.geist_mono_semibold, R.font.geist_mono_bold, R.font.geist_mono_extrabold,
)
private val quicksand = FontFamily(Font(R.font.quicksand))
private val fredoka = FontFamily(Font(R.font.fredoka))
private val jetBrainsMono = FontFamily(Font(R.font.jetbrains_mono))
private val spaceGrotesk = FontFamily(Font(R.font.space_grotesk))
private val ibmPlexMono = FontFamily(Font(R.font.ibm_plex_mono))
private val firaCode = FontFamily(Font(R.font.fira_code))
private val pressStart = FontFamily(Font(R.font.press_start_2p))
private val pixelifySans = FontFamily(Font(R.font.pixelify_sans))
private val bricolageGrotesque = FontFamily(Font(R.font.bricolage_grotesque))
private val specialElite = FontFamily(Font(R.font.special_elite))
private val permanentMarker = FontFamily(Font(R.font.permanent_marker))
private val inter = FontFamily(Font(R.font.inter))
private val lilitaOne = FontFamily(Font(R.font.lilita_one))
private val rajdhani = FontFamily(Font(R.font.rajdhani))

/**
 * The Morning Edition serif (web and iOS Newsreader). Theme-independent: it is
 * the newspaper's voice, not a theme role.
 */
// Static instances cut from the variable Newsreader (wght pinned; opsz 16 for
// text weights, 28–36 for headline weights). Several faces sharing one variable
// resource rendered Bold as the thin default on device, so each weight is its
// own file.
internal val NewsreaderFamily: FontFamily = FontFamily(
    Font(R.font.newsreader_regular, FontWeight.Normal),
    Font(R.font.newsreader_medium, FontWeight.Medium),
    Font(R.font.newsreader_semibold, FontWeight.SemiBold),
    Font(R.font.newsreader_bold, FontWeight.Bold),
    Font(R.font.newsreader_extrabold, FontWeight.ExtraBold),
)

private val familiesByName = mapOf(
    "Outfit" to outfit,
    "Manrope" to manrope,
    "Geist Mono" to geistMono,
    "Quicksand" to quicksand,
    "Fredoka" to fredoka,
    "JetBrains Mono" to jetBrainsMono,
    "Space Grotesk" to spaceGrotesk,
    "IBM Plex Mono" to ibmPlexMono,
    "Fira Code" to firaCode,
    "Press Start 2P" to pressStart,
    "Pixelify Sans" to pixelifySans,
    "Bricolage Grotesque" to bricolageGrotesque,
    "Special Elite" to specialElite,
    "Permanent Marker" to permanentMarker,
    "Inter" to inter,
    "Lilita One" to lilitaOne,
    "Rajdhani" to rajdhani,
)

/** Exact native mirror of Web's brand/display/primary/mono theme table. */
internal val themeFontNamesById: Map<String, ThemeFontNames> = mapOf(
    "longhand" to ThemeFontNames("Outfit", "Outfit", "Manrope", "Geist Mono"),
    "longhand-dark" to ThemeFontNames("Outfit", "Outfit", "Manrope", "Geist Mono"),
    "soft-machine" to ThemeFontNames("Outfit", "Space Grotesk", "Quicksand", "JetBrains Mono"),
    "soft-machine-dark" to ThemeFontNames("Outfit", "Fredoka", "Quicksand", "JetBrains Mono"),
    "arcane-terminal" to ThemeFontNames("Outfit", "Fira Code", "IBM Plex Mono", "Fira Code"),
    "arcane-terminal-light" to ThemeFontNames("Outfit", "Fira Code", "IBM Plex Mono", "Fira Code"),
    "retro-16bit" to ThemeFontNames("Outfit", "IBM Plex Mono", "JetBrains Mono", "IBM Plex Mono"),
    "retro-16bit-light" to ThemeFontNames("Outfit", "IBM Plex Mono", "JetBrains Mono", "JetBrains Mono"),
    "mario-8bit" to ThemeFontNames("Outfit", "Press Start 2P", "Pixelify Sans", "Press Start 2P"),
    "mario-8bit-dark" to ThemeFontNames("Outfit", "Press Start 2P", "Pixelify Sans", "Press Start 2P"),
    "risograph" to ThemeFontNames("Outfit", "Bricolage Grotesque", "Manrope", "JetBrains Mono"),
    "risograph-dark" to ThemeFontNames("Outfit", "Bricolage Grotesque", "Manrope", "JetBrains Mono"),
    "mixtape" to ThemeFontNames("Outfit", "Permanent Marker", "Special Elite", "IBM Plex Mono"),
    "mixtape-dark" to ThemeFontNames("Outfit", "Permanent Marker", "Special Elite", "IBM Plex Mono"),
    "mono" to ThemeFontNames("Outfit", "Space Grotesk", "Inter", "JetBrains Mono"),
    "mono-dark" to ThemeFontNames("Outfit", "Space Grotesk", "Inter", "JetBrains Mono"),
    "cartoon" to ThemeFontNames("Outfit", "Lilita One", "Fredoka", "JetBrains Mono"),
    "cartoon-dark" to ThemeFontNames("Outfit", "Lilita One", "Fredoka", "JetBrains Mono"),
    "bubbly" to ThemeFontNames("Outfit", "Fredoka", "Quicksand", "JetBrains Mono"),
    "bubbly-dark" to ThemeFontNames("Outfit", "Fredoka", "Quicksand", "JetBrains Mono"),
    "jarvis" to ThemeFontNames("Outfit", "Rajdhani", "Manrope", "JetBrains Mono"),
    "jarvis-light" to ThemeFontNames("Outfit", "Rajdhani", "Manrope", "JetBrains Mono"),
)

internal fun themeFontNames(themeId: String): ThemeFontNames =
    themeFontNamesById[themeId] ?: themeFontNamesById.getValue(Themes.DEFAULT)

internal fun themeFontFamilies(themeId: String): ThemeFontFamilies {
    val names = themeFontNames(themeId)
    return ThemeFontFamilies(
        brand = familiesByName.getValue(names.brand),
        display = familiesByName.getValue(names.display),
        body = familiesByName.getValue(names.body),
        mono = familiesByName.getValue(names.mono),
    )
}

internal val LocalMagicanFontFamilies = staticCompositionLocalOf {
    themeFontFamilies(Themes.DEFAULT)
}

// Native iOS uses the face's natural leading. Keeping Material's 24sp body
// line box on a 10sp caption makes compact metrics and widgets twice as tall.
private fun TextStyle.withFamily(family: FontFamily): TextStyle = copy(
    fontFamily = family, lineHeight = TextUnit.Unspecified, letterSpacing = 0.sp,
)

internal fun magicanTypography(themeId: String): Typography {
    val base = Typography()
    val fonts = themeFontFamilies(themeId)
    return Typography(
        displayLarge = base.displayLarge.withFamily(fonts.display),
        displayMedium = base.displayMedium.withFamily(fonts.display),
        displaySmall = base.displaySmall.withFamily(fonts.display),
        headlineLarge = base.headlineLarge.withFamily(fonts.display),
        headlineMedium = base.headlineMedium.withFamily(fonts.display),
        headlineSmall = base.headlineSmall.withFamily(fonts.display),
        titleLarge = base.titleLarge.withFamily(fonts.display),
        titleMedium = base.titleMedium.withFamily(fonts.display),
        titleSmall = base.titleSmall.withFamily(fonts.display),
        bodyLarge = base.bodyLarge.withFamily(fonts.body),
        bodyMedium = base.bodyMedium.withFamily(fonts.body),
        bodySmall = base.bodySmall.withFamily(fonts.body),
        labelLarge = base.labelLarge.withFamily(fonts.body),
        labelMedium = base.labelMedium.withFamily(fonts.body),
        labelSmall = base.labelSmall.withFamily(fonts.body),
    )
}

@Composable
internal fun MagicanTheme(themeId: String, palette: Palette, content: @Composable () -> Unit) {
    CompositionLocalProvider(LocalMagicanFontFamilies provides themeFontFamilies(themeId)) {
        MaterialTheme(
            colorScheme = magicanColorScheme(palette),
            typography = magicanTypography(themeId),
        ) {
            // Own the full page canvas, including transparent screen roots
            // and gaps around cards, independently of individual widgets.
            Box(Modifier.fillMaxSize().background(palette.background)) {
                content()
            }
        }
    }
}
