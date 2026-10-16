package ai.magicbeans.magdroid.ui

import androidx.compose.ui.graphics.Color
import java.io.File
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Night themes put white text on accent fills (owner decision 2026-10-04).
 * White on the bright night accents fails contrast, so every night palette
 * carries a deeper [Palette.accentFill] of the same hue; day palettes keep
 * their accent. Filled controls must sit on that fill, not on the accent.
 */
class AccentFillContractTest {

    /** WCAG 2.x relative luminance (sRGB, linearised). */
    private fun relativeLuminance(color: Color): Double {
        fun channel(c: Float): Double {
            val v = c.toDouble()
            return if (v <= 0.03928) v / 12.92 else ((v + 0.055) / 1.055).pow(2.4)
        }
        return 0.2126 * channel(color.red) + 0.7152 * channel(color.green) + 0.0722 * channel(color.blue)
    }

    private fun contrast(a: Color, b: Color): Double {
        val la = relativeLuminance(a)
        val lb = relativeLuminance(b)
        return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)
    }

    /** HSL hue and saturation, to check the fill is the accent darkened, not a new colour. */
    private fun hueSaturation(color: Color): Pair<Double, Double> {
        val r = color.red.toDouble(); val g = color.green.toDouble(); val b = color.blue.toDouble()
        val hi = maxOf(r, g, b); val lo = minOf(r, g, b); val d = hi - lo
        val l = (hi + lo) / 2
        if (d == 0.0) return 0.0 to 0.0
        val s = d / (1 - abs(2 * l - 1))
        val h = when (hi) {
            r -> 60 * (((g - b) / d).mod(6.0))
            g -> 60 * ((b - r) / d + 2)
            else -> 60 * ((r - g) / d + 4)
        }
        return h to s
    }

    @Test
    fun `night palettes fill with a deeper accent that white reads on`() {
        val night = Themes.families.map { it.dark }
        night.forEach { id ->
            val palette = Themes.palette(id)
            assertTrue("$id is dark", palette.isDark)
            assertEquals("$id onAccent", Color.White, palette.onAccent)
            val fill = contrast(Color.White, palette.accentFill)
            val pressed = contrast(Color.White, palette.accentFillPressed)
            assertTrue("$id white on accentFill $fill", fill >= 4.5)
            assertTrue("$id white on accentFillPressed $pressed", pressed >= 5.3 && pressed > fill)
            if (palette.accent != Color.White) {
                val (accentHue, accentSat) = hueSaturation(palette.accent)
                val (fillHue, fillSat) = hueSaturation(palette.accentFill)
                assertTrue("$id hue $accentHue vs $fillHue", abs(accentHue - fillHue) < 2.5)
                assertTrue("$id saturation $accentSat vs $fillSat", abs(accentSat - fillSat) < 0.03)
            }
        }
    }

    @Test
    fun `day palettes fill with the accent itself`() {
        Themes.families.map { it.light }.forEach { id ->
            val palette = Themes.palette(id)
            assertTrue("$id is light", !palette.isDark)
            assertEquals("$id accentFill", palette.accent, palette.accentFill)
            assertEquals("$id accentFillPressed", palette.accentHover, palette.accentFillPressed)
        }
    }

    @Test
    fun `material keeps the bright accent as primary for content`() {
        Themes.palettes.forEach { (id, palette) ->
            val scheme = magicanColorScheme(palette)
            assertEquals("$id primary", palette.accent, scheme.primary)
            assertEquals("$id onPrimary", palette.onAccent, scheme.onPrimary)
        }
    }

    /**
     * Because `primary` stays bright, a filled Material control left on its
     * default colours would put white on the bright accent at night. Every
     * Button, Switch and Checkbox therefore passes explicit colours, and none
     * fills with the bright accent.
     */
    @Test
    fun `filled controls never fall back to the bright accent`() {
        val root = File(requireNotNull(System.getProperty("user.dir")))
        val project = generateSequence(root) { it.parentFile }
            .firstOrNull { File(it, "app/src/main/kotlin").isDirectory }
            ?: File(root, "magdroid/android").takeIf { File(it, "app/src/main/kotlin").isDirectory }
            ?: error("could not locate the Android Gradle project from $root")
        val sources = File(project, "app/src/main/kotlin/ai/magicbeans/magdroid/ui")
            .listFiles { file -> file.extension == "kt" }
            .orEmpty()
        assertTrue("ui sources found", sources.isNotEmpty())
        val offenders = mutableListOf<String>()
        val call = Regex("""(?<![A-Za-z0-9_.])(Button|Switch|Checkbox)\(""")
        val brightFill = Regex(
            """buttonColors\(containerColor = Coral[,)]|checkedTrackColor = (Coral|palette\.accent|activePalette\.accent)\b|""" +
                """checkedColor = (Coral|palette\.accent)\b|Badge\(containerColor = Coral\b""",
        )
        sources.forEach { file ->
            val text = file.readText()
            brightFill.findAll(text).forEach { offenders += "${file.name}: ${it.value}" }
            call.findAll(text).forEach { match ->
                if (text.substring(maxOf(0, match.range.first - 4), match.range.first).endsWith("fun ")) return@forEach
                var depth = 1
                var i = match.range.last + 1
                while (depth > 0 && i < text.length) {
                    when (text[i]) { '(' -> depth++; ')' -> depth-- }
                    i++
                }
                val args = text.substring(match.range.last + 1, i - 1)
                if ("colors" !in args) {
                    val line = text.substring(0, match.range.first).count { it == '\n' } + 1
                    offenders += "${file.name}:$line ${match.value} without explicit colors"
                }
            }
        }
        assertTrue(offenders.joinToString("\n"), offenders.isEmpty())
    }
}
