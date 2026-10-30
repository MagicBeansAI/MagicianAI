package ai.magicbeans.magdroid.ui

/**
 * A note's Markdown as the reader sees it: without the YAML front matter
 * Magician writes at the top of its notes (`magician_kind`, `created_at`, …).
 *
 * Only the rendered view uses this; editing and saving keep the whole file,
 * front matter included.
 *
 * The block is recognised when, after an optional BOM and leading blank
 * space, the first line is exactly `---` and a later line is exactly `---` or
 * `...` (trailing spaces allowed; LF or CRLF). Its first non-blank line must
 * be a `key:` mapping, so a note that merely opens with a `---` rule and a
 * paragraph keeps both. Anything else — no front matter, or a block that is
 * never closed — comes back unchanged.
 */
internal fun noteBody(markdown: String): String {
    var start = 0
    if (markdown.startsWith('﻿')) start = 1
    while (start < markdown.length && markdown[start].isWhitespace()) start++

    var at = start
    var first = true
    var sawContent = false
    while (at < markdown.length) {
        val newline = markdown.indexOf('\n', at)
        val end = if (newline < 0) markdown.length else newline
        val line = markdown.substring(at, end).removeSuffix("\r")
        val next = if (newline < 0) markdown.length else newline + 1
        if (first) {
            if (line.trim() != "---" || newline < 0) return markdown
            first = false
        } else {
            val fence = line.trimEnd(' ', '\t')
            if (fence == "---" || fence == "...") {
                return markdown.substring(next).trimStart('\r', '\n')
            }
            if (!sawContent && line.isNotBlank() && !line.trimStart().startsWith('#')) {
                if (!YAML_KEY.containsMatchIn(line)) return markdown
                sawContent = true
            }
        }
        at = next
    }
    return markdown
}

/** A top-level mapping key: `name:`, `"quoted name":` or `'quoted':`. */
private val YAML_KEY = Regex("""^(?:[A-Za-z0-9_][\w .\-]*|"[^"]*"|'[^']*')[ \t]*:(?:[ \t]|$)""")
