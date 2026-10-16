package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.vibedev.DiffLine
import ai.magicbeans.magdroid.vibedev.DiffLineKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

class UnifiedDiffLineCapTest {
    @Test
    fun `a line within the cap is shown whole`() {
        assertEquals(ShownLine("abc", 0), shownLine("abc"))
        val atCap = "x".repeat(MAX_LINE_CHARS)
        assertEquals(ShownLine(atCap, 0), shownLine(atCap))
    }

    @Test
    fun `a minified line is cut to the cap and the rest is counted`() {
        val line = "x".repeat(256 * 1024)
        val shown = shownLine(line)
        assertEquals(MAX_LINE_CHARS, shown.text.length)
        assertEquals(256 * 1024 - MAX_LINE_CHARS, shown.hidden)
        assertEquals("… 258,144 more characters", hiddenCharsLabel(shown.hidden))
        assertEquals("… 1 more character", hiddenCharsLabel(1))
    }

    @Test
    fun `the cut never splits a surrogate pair`() {
        // An emoji straddling the cap: its high half would be the last char kept.
        val line = "a".repeat(9) + "😀" + "b"
        val shown = shownLine(line, maxChars = 10)
        assertEquals("a".repeat(9), shown.text)
        assertEquals(3, shown.hidden)
        assertFalse(shown.text.last().isHighSurrogate())
    }

    @Test
    fun `TalkBack reads the shown part, not the whole line`() {
        val line = DiffLine(DiffLineKind.Added, "y".repeat(MAX_LINE_CHARS + 5), oldNumber = null, newNumber = 7)
        val spoken = diffLineDescription(line)
        assertTrue(spoken.startsWith("Added, line 7: "))
        assertTrue(spoken.endsWith("… 5 more characters"))
        assertEquals("Line 3: x", diffLineDescription(DiffLine(DiffLineKind.Context, "x", 3, 3)))
    }

    @Test
    fun `no diff row is sized at its intrinsic height`() {
        // IntrinsicSize.Min pinned a row to its text's height, and a tall
        // wrapped line threw "Can't represent a height of …".
        val root = File(requireNotNull(System.getProperty("user.dir")))
        val project = generateSequence(root) { it.parentFile }
            .firstOrNull { File(it, "app/src/main/kotlin").isDirectory }
            ?: File(root, "magdroid/android").takeIf { File(it, "app/src/main/kotlin").isDirectory }
            ?: error("could not locate the Android Gradle project from $root")
        val source = File(project, "app/src/main/kotlin/ai/magicbeans/magdroid/ui/vibedev/UnifiedDiffView.kt").readText()
        assertFalse(source.contains("IntrinsicSize"))
    }
}
