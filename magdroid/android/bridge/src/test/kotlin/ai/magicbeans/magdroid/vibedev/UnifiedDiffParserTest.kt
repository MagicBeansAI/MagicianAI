package ai.magicbeans.magdroid.vibedev

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class UnifiedDiffParserTest {

    private val sample = """
        diff --git a/src/a.ts b/src/a.ts
        index 1a2b3c4..5d6e7f8 100644
        --- a/src/a.ts
        +++ b/src/a.ts
        @@ -10,4 +10,5 @@ export function a() {
           const x = 1;
        -  const y = 2;
        +  const y = 3;
        +  const z = 4;
           return x;
         }
        @@ -40 +41,2 @@
        -old
        +new
        +more
        \ No newline at end of file
    """.trimIndent() + "\n"

    @Test
    fun `header lines stay out of the hunks`() {
        val diff = UnifiedDiffParser.parse(sample)
        assertEquals(4, diff.header.size)
        assertEquals("--- a/src/a.ts", diff.header[2])
        assertEquals(2, diff.hunks.size)
    }

    @Test
    fun `lines carry old and new numbers`() {
        val hunk = UnifiedDiffParser.parse(sample).hunks[0]
        assertEquals(10, hunk.oldStart); assertEquals(4, hunk.oldCount)
        assertEquals(10, hunk.newStart); assertEquals(5, hunk.newCount)
        assertEquals("export function a() {", hunk.section)
        val kinds = hunk.lines.map { it.kind }
        assertEquals(
            listOf(DiffLineKind.Context, DiffLineKind.Removed, DiffLineKind.Added, DiffLineKind.Added, DiffLineKind.Context, DiffLineKind.Context),
            kinds,
        )
        assertEquals(DiffLine(DiffLineKind.Context, "  const x = 1;", 10, 10), hunk.lines[0])
        assertEquals(DiffLine(DiffLineKind.Removed, "  const y = 2;", 11, null), hunk.lines[1])
        assertEquals(DiffLine(DiffLineKind.Added, "  const y = 3;", null, 11), hunk.lines[2])
        assertEquals(DiffLine(DiffLineKind.Added, "  const z = 4;", null, 12), hunk.lines[3])
        assertEquals(DiffLine(DiffLineKind.Context, "  return x;", 12, 13), hunk.lines[4])
        assertEquals(DiffLine(DiffLineKind.Context, "}", 13, 14), hunk.lines[5])
    }

    @Test
    fun `a hunk without counts means one line, and no-newline notes are notes`() {
        val hunk = UnifiedDiffParser.parse(sample).hunks[1]
        assertEquals(40, hunk.oldStart); assertEquals(1, hunk.oldCount)
        assertEquals(41, hunk.newStart); assertEquals(2, hunk.newCount)
        assertEquals(DiffLineKind.Note, hunk.lines.last().kind)
        assertEquals("No newline at end of file", hunk.lines.last().text)
        assertEquals(DiffLine(DiffLineKind.Added, "more", null, 42), hunk.lines[2])
    }

    @Test
    fun `counts add up across hunks`() {
        val diff = UnifiedDiffParser.parse(sample)
        assertEquals(4, diff.additions)
        assertEquals(2, diff.deletions)
    }

    @Test
    fun `an empty or header-only diff has no hunks`() {
        assertTrue(UnifiedDiffParser.parse("").isEmpty)
        assertTrue(UnifiedDiffParser.parse("diff --git a/x b/x\nBinary files differ\n").isEmpty)
    }

    @Test
    fun `several files keep their own hunks`() {
        val two = sample + "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-a\n+b\n"
        val diff = UnifiedDiffParser.parse(two)
        assertEquals(3, diff.hunks.size)
        assertEquals(DiffLine(DiffLineKind.Removed, "a", 1, null), diff.hunks[2].lines[0])
        assertTrue(diff.header.contains("diff --git a/b b/b"))
    }

    @Test
    fun `long unchanged runs fold, keeping context beside each change`() {
        val body = buildString {
            append("@@ -1,20 +1,20 @@\n")
            append("-first\n+FIRST\n")
            (2..19).forEach { append(" line$it\n") }
            append("-last\n+LAST\n")
        }
        val hunk = UnifiedDiffParser.parse(body).hunks.single()
        val rows = UnifiedDiffParser.rows(hunk)
        val fold = rows.filterIsInstance<DiffRow.Fold>().single()
        assertEquals(12, fold.count)
        assertEquals("line5", fold.lines.first().text)
        // Three context lines stay on each side of the fold.
        val before = rows.takeWhile { it !is DiffRow.Fold }.filterIsInstance<DiffRow.Line>().map { it.line.text }
        assertEquals(listOf("first", "FIRST", "line2", "line3", "line4"), before)
        val shown = rows.sumOf { if (it is DiffRow.Fold) it.count else 1 }
        assertEquals(hunk.lines.size, shown)
    }

    @Test
    fun `short unchanged runs and edge context are not folded`() {
        val body = "@@ -1,6 +1,6 @@\n a\n b\n-c\n+C\n d\n e\n"
        val rows = UnifiedDiffParser.rows(UnifiedDiffParser.parse(body).hunks.single())
        assertTrue(rows.none { it is DiffRow.Fold })
    }
}

class VibeDevLiveRunDecodeTest {
    @Test
    fun `a live run without started_at still decodes`() {
        val run = vibeDevJson.decodeFromString(
            VibeDevLiveRun.serializer(),
            """{"run_id":"r","live_task_id":"r","project_id":"p","state":"working","turns":1,"updated_at":"2026-10-13T09:00:00Z"}""",
        )
        org.junit.Assert.assertNull(run.startedAt)
        assertEquals(VibeDevRunState.Working, run.state)
    }
}
