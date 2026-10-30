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
 * paragraph keeps both. Anything else (no front matter, or a block that is
 * never closed) comes back unchanged. Same rules as the iOS and Android readers.
 */
export function noteBody(markdown: string): string {
	let start = markdown.startsWith('﻿') ? 1 : 0;
	while (start < markdown.length && /\s/.test(markdown[start] ?? '')) start++;

	let at = start;
	let first = true;
	let sawContent = false;
	while (at < markdown.length) {
		const newline = markdown.indexOf('\n', at);
		const end = newline < 0 ? markdown.length : newline;
		const line = markdown.slice(at, end).replace(/\r$/, '');
		const next = newline < 0 ? markdown.length : newline + 1;
		if (first) {
			if (line.trim() !== '---' || newline < 0) return markdown;
			first = false;
		} else {
			const fence = line.replace(/[ \t]+$/, '');
			if (fence === '---' || fence === '...') {
				return markdown.slice(next).replace(/^[\r\n]+/, '');
			}
			if (!sawContent && line.trim() !== '' && !line.trimStart().startsWith('#')) {
				if (!YAML_KEY.test(line)) return markdown;
				sawContent = true;
			}
		}
		at = next;
	}
	return markdown;
}

/** A top-level mapping key: `name:`, `"quoted name":` or `'quoted':`. */
const YAML_KEY = /^(?:[A-Za-z0-9_][A-Za-z0-9_ .-]*|"[^"]*"|'[^']*')[ \t]*:(?:[ \t]|$)/;
