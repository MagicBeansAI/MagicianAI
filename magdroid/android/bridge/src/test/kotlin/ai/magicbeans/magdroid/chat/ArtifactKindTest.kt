package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * What a task's output is, against `ArtifactViewer.swift`.
 *
 * The mime type was decoded here and never read, so every output rendered as
 * the same generic document row — a chart, a recording and a spreadsheet all
 * looked alike in a list whose whole job is telling them apart.
 */
class ArtifactKindTest {

    @Test
    fun `the mime type decides when it is there`() {
        assertEquals(ArtifactKind.Image, ArtifactKind.from("image/png", null))
        assertEquals(ArtifactKind.Pdf, ArtifactKind.from("application/pdf", null))
        assertEquals(ArtifactKind.Html, ArtifactKind.from("text/html", null))
        assertEquals(ArtifactKind.Video, ArtifactKind.from("video/mp4", null))
        assertEquals(ArtifactKind.Audio, ArtifactKind.from("audio/wav", null))
        assertEquals(ArtifactKind.Markdown, ArtifactKind.from("text/markdown", null))
        assertEquals(ArtifactKind.Json, ArtifactKind.from("application/json", null))
        assertEquals(ArtifactKind.Text, ArtifactKind.from("text/plain", null))
    }

    /**
     * The fallback is the half that earns its keep: a served artifact often
     * carries no mime, or `application/octet-stream`, and `chart.png` is an
     * image whatever the header says.
     */
    @Test
    fun `the extension decides when the mime is absent or useless`() {
        assertEquals(ArtifactKind.Image, ArtifactKind.from(null, "chart.png"))
        assertEquals(ArtifactKind.Image, ArtifactKind.from("application/octet-stream", "photo.HEIC"))
        assertEquals(ArtifactKind.Pdf, ArtifactKind.from("", "report.pdf"))
        assertEquals(ArtifactKind.Video, ArtifactKind.from(null, "clip.mov"))
        assertEquals(ArtifactKind.Audio, ArtifactKind.from(null, "note.m4a"))
        assertEquals(ArtifactKind.Markdown, ArtifactKind.from(null, "notes.md"))
        assertEquals(ArtifactKind.Json, ArtifactKind.from(null, "payload.json"))
        assertEquals(ArtifactKind.Text, ArtifactKind.from(null, "rows.csv"))
        assertEquals(ArtifactKind.Html, ArtifactKind.from(null, "page.htm"))
    }

    /** Unknown is a file, not a guess. */
    @Test
    fun `anything unrecognised is a plain file`() {
        assertEquals(ArtifactKind.Other, ArtifactKind.from(null, null))
        assertEquals(ArtifactKind.Other, ArtifactKind.from("application/zip", "bundle.zip"))
        assertEquals(ArtifactKind.Other, ArtifactKind.from(null, "no-extension"))
    }

    /** Case and stray whitespace come off the wire and must not change the answer. */
    @Test
    fun `case and padding do not change the kind`() {
        assertEquals(ArtifactKind.Image, ArtifactKind.from("  IMAGE/PNG  ", null))
        assertEquals(ArtifactKind.Pdf, ArtifactKind.from(null, "REPORT.PDF"))
    }

    /** A dotted name must not read its own directory as an extension. */
    @Test
    fun `a name with dots resolves on its last segment`() {
        assertEquals(ArtifactKind.Json, ArtifactKind.from(null, "run.2026-08-12.json"))
    }
}
