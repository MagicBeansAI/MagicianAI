package ai.magicbeans.magdroid.ui

import java.io.File
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * VibeDev screens read only theme tokens (plan §6): every web theme then
 * renders the same here as on iPhone. No raw colours, filled controls through
 * the Magican colour helpers, and the font roles rather than ad hoc families.
 */
class VibeDevTokenContractTest {
    private val sources: List<File> by lazy {
        val root = File(requireNotNull(System.getProperty("user.dir")))
        val project = generateSequence(root) { it.parentFile }
            .firstOrNull { File(it, "app/src/main/kotlin").isDirectory }
            ?: File(root, "magdroid/android").takeIf { File(it, "app/src/main/kotlin").isDirectory }
            ?: error("could not locate the Android Gradle project from $root")
        File(project, "app/src/main/kotlin/ai/magicbeans/magdroid/ui/vibedev")
            .listFiles { file -> file.extension == "kt" }
            .orEmpty()
            .toList()
    }

    @Test
    fun `vibedev sources exist`() {
        val names = sources.map { it.name }.toSet()
        listOf(
            "VibeDevHomeScreen.kt", "VibeDevProjectScreen.kt", "VibeDevRunScreen.kt", "VibeDevTurnTimeline.kt",
            "VibeDevDiffScreen.kt", "UnifiedDiffView.kt", "VibeDevCrewCard.kt", "VibeDevSheets.kt", "VibeDevUndo.kt",
        ).forEach { assertTrue("$it missing", it in names) }
    }

    @Test
    fun `no raw colours in vibedev screens`() {
        val raw = Regex("""Color\(\s*0x|Color\.(White|Black|Red|Green|Blue|Yellow|Gray|Grey|DarkGray|LightGray|Cyan|Magenta|Transparent)\b|Color\(\s*red\s*=|Color\.hsv|Color\.rgb|android\.graphics\.Color""")
        val offenders = sources.flatMap { file ->
            raw.findAll(file.readText()).map { "${file.name}: ${it.value}" }.toList()
        }
        assertTrue(offenders.joinToString("\n"), offenders.isEmpty())
    }

    @Test
    fun `filled controls pass the magican colours`() {
        val call = Regex("""(?<![A-Za-z0-9_.])(Button|Switch|Checkbox)\(""")
        val offenders = mutableListOf<String>()
        sources.forEach { file ->
            val text = file.readText()
            call.findAll(text).forEach { match ->
                var depth = 1
                var i = match.range.last + 1
                while (depth > 0 && i < text.length) {
                    when (text[i]) { '(' -> depth++; ')' -> depth-- }
                    i++
                }
                val args = text.substring(match.range.last + 1, i - 1)
                if ("magicanButtonColors()" !in args && "magicanSwitchColors()" !in args && "magicanCheckboxColors()" !in args) {
                    offenders += "${file.name}: ${match.value}${args.take(60)}"
                }
            }
        }
        assertTrue(offenders.joinToString("\n"), offenders.isEmpty())
    }

    @Test
    fun `text uses the theme font roles`() {
        val adHoc = Regex("""FontFamily\.(Monospace|Serif|SansSerif|Cursive|Default)|FontFamily\(""")
        val offenders = sources.flatMap { file -> adHoc.findAll(file.readText()).map { "${file.name}: ${it.value}" }.toList() }
        assertTrue(offenders.joinToString("\n"), offenders.isEmpty())
        val diff = sources.first { it.name == "UnifiedDiffView.kt" }.readText()
        assertTrue("the diff uses the mono role", diff.contains("LocalMagicanFontFamilies.current.mono"))
    }
}
