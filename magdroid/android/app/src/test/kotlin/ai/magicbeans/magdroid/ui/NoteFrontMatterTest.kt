package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Test

/** The Notes reader hides a note's YAML front matter and nothing else. */
class NoteFrontMatterTest {
    @Test
    fun `magician front matter is hidden`() {
        val note = "---\nmagician_kind: note\ncreated_at: 2026-10-21T05:01:14.735093+00:00\n---\n# link-check\n\nbody\n"
        assertEquals("# link-check\n\nbody\n", noteBody(note))
    }

    @Test
    fun `a BOM and leading blank lines before the block are allowed`() {
        assertEquals("# Title\n", noteBody("﻿---\nkind: note\n---\n# Title\n"))
        assertEquals("# Title\n", noteBody("\n\n  ---\nkind: note\n---\n# Title\n"))
    }

    @Test
    fun `a dots terminator closes the block`() {
        assertEquals("Body", noteBody("---\nkind: note\n...\nBody"))
    }

    @Test
    fun `CRLF line endings are handled`() {
        assertEquals("# Title\r\nBody\r\n", noteBody("---\r\nkind: note\r\ntags: [a, b]\r\n---\r\n\r\n# Title\r\nBody\r\n"))
    }

    @Test
    fun `trailing blanks on the fences are allowed`() {
        assertEquals("Body", noteBody("--- \t\nkind: note\n---  \nBody"))
    }

    @Test
    fun `lists, nesting, comments and blank lines inside the block are hidden too`() {
        val note = "---\n# written by Magician\n\ntags:\n  - a\n  - b\nmeta:\n  source: chat\n\"quoted key\": 1\n---\nBody"
        assertEquals("Body", noteBody(note))
    }

    @Test
    fun `an empty block is hidden`() {
        assertEquals("Body", noteBody("---\n---\nBody"))
    }

    @Test
    fun `a block at the very end leaves an empty note`() {
        assertEquals("", noteBody("---\nkind: note\n---"))
        assertEquals("", noteBody("---\nkind: note\n---\n"))
    }

    @Test
    fun `a note without front matter is unchanged`() {
        listOf(
            "",
            "# Title\n\nBody",
            "Body\n---\nmore",
            "Intro\n\n---\nkind: note\n---\n",
        ).forEach { assertEquals(it, noteBody(it)) }
    }

    @Test
    fun `an unterminated block is left as is`() {
        val note = "---\nmagician_kind: note\ncreated_at: today\n# Title\nBody"
        assertEquals(note, noteBody(note))
        assertEquals("---", noteBody("---"))
        assertEquals("---\n", noteBody("---\n"))
    }

    @Test
    fun `a leading rule over a paragraph is content, not front matter`() {
        val note = "---\nA paragraph between two rules.\n---\nMore"
        assertEquals(note, noteBody(note))
    }

    @Test
    fun `a longer rule or text on the fence line is not a fence`() {
        listOf(
            "----\nkind: note\n----\nBody",
            "--- kind: note\n---\nBody",
        ).forEach { assertEquals(it, noteBody(it)) }
        // A `----` line inside the block does not close it; the `---` after it does.
        assertEquals("Body", noteBody("---\nkind: note\n----\n---\nBody"))
    }
}
