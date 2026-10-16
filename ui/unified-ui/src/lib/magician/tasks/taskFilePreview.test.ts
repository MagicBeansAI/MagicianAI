import { describe, expect, it } from 'vitest';

import {
	bytesIfKnown,
	delimiterOf,
	parseDelimited,
	prettyJson,
	previewKindOf,
	tooLargeLine,
	PREVIEW_MAX_BYTES,
	PREVIEW_MAX_ROWS,
	type PreviewKind
} from './taskFilePreview';

describe('previewKindOf — the mime decides, except when it says nothing', () => {
	/**
	 * Every kind reachable from a mime, and the ones that are reachable from more
	 * than one spelling. The path is a name the extension **disagrees** with
	 * wherever the mime is authoritative, so a function that ignored the mime and
	 * read the name would answer something else on every row here.
	 */
	const BY_MIME: Array<[string, string, PreviewKind | null]> = [
		['text/markdown', 'a.bin', 'markdown'],
		['text/x-markdown', 'a.bin', 'markdown'],
		['image/png', 'a.bin', 'image'],
		['image/svg+xml', 'a.bin', 'image'],
		['application/json', 'a.bin', 'json'],
		['text/json', 'a.bin', 'json'],
		['application/vnd.api+json', 'a.bin', 'json'],
		['text/csv', 'a.bin', 'csv'],
		['text/tab-separated-values', 'a.bin', 'csv'],
		// A `text/*` nothing above names, on a path that says nothing: still text.
		['text/x-unheard-of', 'a.bin', 'text'],
		// The same mime on a path that *does* say something: the extension refines
		// it rather than being ignored, which is what keeps a `text/x-rust` body
		// out of the plain-text branch.
		['text/x-rust', 'main.rs', 'code'],
		// Not text, not an image, and nothing this panel can draw.
		['application/pdf', 'a.md', null],
		['application/zip', 'a.md', null],
		['video/mp4', 'a.md', null],
		['audio/mpeg', 'a.md', null]
	];

	it.each(BY_MIME)('reads %s as %s → %s', (mediaType, path, expected) => {
		expect(previewKindOf(mediaType, path)).toBe(expected);
	});

	it('strips the parameters, so a charset does not hide the type', () => {
		// The one shape every real server sends and no `===` comparison survives.
		expect(previewKindOf('text/csv; charset=utf-8', 'rows.bin')).toBe('csv');
		expect(previewKindOf('TEXT/MARKDOWN', 'notes.bin')).toBe('markdown');
	});

	/**
	 * The rule that inverts `outputKindOf`'s. There, the mime is authoritative
	 * because a wrong guess costs a plural; here it costs the reader the table
	 * they opened the row for, and `text/plain` on a `.csv` is what real
	 * producers send.
	 */
	const UNINFORMATIVE: Array<[string, string, PreviewKind | null]> = [
		['text/plain', 'rows.csv', 'csv'],
		['text/plain', 'report.md', 'markdown'],
		['text/plain', 'data.json', 'json'],
		['text/plain', 'main.py', 'code'],
		// Nothing to refine it with: still text, because the server said text.
		['text/plain', 'notes', 'text'],
		['application/octet-stream', 'rows.csv', 'csv'],
		// A server that said nothing, about a name that says nothing. **Not**
		// text: an empty monospace block under an unknown file is
		// indistinguishable from a file that turned out to be empty.
		['application/octet-stream', 'blob', null]
	];

	it.each(UNINFORMATIVE)('lets the name answer %s on %s → %s', (mediaType, path, expected) => {
		expect(previewKindOf(mediaType, path)).toBe(expected);
	});

	const BY_EXTENSION: Array<[string, PreviewKind | null]> = [
		['q3/report.md', 'markdown'],
		['notes.markdown', 'markdown'],
		['charts/revenue.png', 'image'],
		['diagram.svg', 'image'],
		['run.json', 'json'],
		['rows.csv', 'csv'],
		['rows.tsv', 'csv'],
		['build.log', 'text'],
		['notes.txt', 'text'],
		['main.rs', 'code'],
		['index.ts', 'code'],
		['query.sql', 'code'],
		// Source, not rendered: a task's HTML is untrusted markup and this panel
		// will not run it inside the app's own document.
		['page.html', 'code'],
		// Nothing to offer, and offering nothing is the answer.
		['report.pdf', null],
		['archive.zip', null],
		['LICENSE', null],
		['no-extension', null]
	];

	it.each(BY_EXTENSION)('falls back to the name for %s → %s', (path, expected) => {
		expect(previewKindOf(null, path)).toBe(expected);
		expect(previewKindOf('', path)).toBe(expected);
		expect(previewKindOf(undefined, path)).toBe(expected);
	});

	it('reads the extension off the basename, not off the directory', () => {
		// A directory carrying a dot is ordinary, and a naive `lastIndexOf('.')`
		// over the whole path reads `d/report` as the extension of `q3.old`.
		expect(previewKindOf(null, 'q3.old/report.md')).toBe('markdown');
		expect(previewKindOf(null, 'q3.md/report')).toBeNull();
	});
});

describe('delimiterOf', () => {
	it('splits on tabs only where the file says tabs', () => {
		expect(delimiterOf('text/tab-separated-values', 'rows.bin')).toBe('\t');
		expect(delimiterOf(null, 'rows.tsv')).toBe('\t');
		expect(delimiterOf('text/csv', 'rows.csv')).toBe(',');
		// Never sniffed from the body: a CSV with no commas in it would sniff as
		// tab-separated and come out as one very wide column.
		expect(delimiterOf(null, 'rows.csv')).toBe(',');
	});
});

describe('parseDelimited — a table, not comma soup', () => {
	it('keeps a quoted delimiter inside its own cell', () => {
		// The case naive `split(',')` gets wrong, and gets wrong *silently*: it
		// yields three cells and shifts every column after it.
		const table = parseDelimited('region,revenue\n"Acme, Inc.",120\n', ',');
		expect(table.rows).toEqual([
			['region', 'revenue'],
			['Acme, Inc.', '120']
		]);
	});

	it('reads a doubled quote as one quote, and a quoted newline as text', () => {
		const table = parseDelimited('note\n"she said ""no""\nthen left"\n', ',');
		expect(table.rows).toEqual([['note'], ['she said "no"\nthen left']]);
	});

	it('splits on tabs when told to, so a TSV is not one column', () => {
		expect(parseDelimited('a\tb\n1\t2\n', '\t').rows).toEqual([
			['a', 'b'],
			['1', '2']
		]);
	});

	it('does not invent a row after the trailing newline', () => {
		// A file ending in a newline is the normal case; an extra empty row on
		// every table is a rendering bug that looks like missing data.
		expect(parseDelimited('a,b\n1,2\n', ',').rows).toHaveLength(2);
		expect(parseDelimited('a,b\n1,2', ',').rows).toHaveLength(2);
	});

	it('keeps an empty trailing cell, which is data', () => {
		expect(parseDelimited('a,b,c\n1,,3\n4,5,\n', ',').rows).toEqual([
			['a', 'b', 'c'],
			['1', '', '3'],
			['4', '5', '']
		]);
	});

	it('drops \\r, so a CRLF file does not end every cell in a control character', () => {
		expect(parseDelimited('a,b\r\n1,2\r\n', ',').rows).toEqual([
			['a', 'b'],
			['1', '2']
		]);
	});

	it('counts what it cut rather than cutting in silence', () => {
		const lines = Array.from({ length: PREVIEW_MAX_ROWS + 17 }, (_, i) => `${i},x`).join('\n');
		const table = parseDelimited(lines, ',');
		expect(table.rows).toHaveLength(PREVIEW_MAX_ROWS);
		expect(table.truncatedRows).toBe(17);
		// The rows kept are the first ones, so the header is among them.
		expect(table.rows[0]).toEqual(['0', 'x']);
	});

	it('reports nothing cut when nothing was', () => {
		expect(parseDelimited('a,b\n1,2\n', ',').truncatedRows).toBe(0);
		expect(parseDelimited('', ',').rows).toEqual([]);
	});
});

describe('prettyJson', () => {
	it('indents, so a one-line payload is readable', () => {
		expect(prettyJson('{"a":1,"b":[2,3]}')).toBe('{\n  "a": 1,\n  "b": [\n    2,\n    3\n  ]\n}');
	});

	it('answers null for bytes that are not JSON, rather than claiming to have formatted them', () => {
		// A truncated write and a newline-delimited log are both readable as
		// text; what must not happen is a formatted-looking block that is not.
		expect(prettyJson('{"a":1')).toBeNull();
		expect(prettyJson('{"a":1}\n{"a":2}')).toBeNull();
		expect(prettyJson('')).toBeNull();
	});
});

describe('bytesIfKnown', () => {
	it('scales to the largest unit that leaves a number worth reading', () => {
		expect(bytesIfKnown(512)).toBe('512 B');
		expect(bytesIfKnown(20_480)).toBe('20 KB');
		expect(bytesIfKnown(1_536)).toBe('1.5 KB');
		expect(bytesIfKnown(4 * 1024 * 1024)).toBe('4.0 MB');
	});

	it('answers null for the sizes a row must not print', () => {
		expect(bytesIfKnown(null)).toBeNull();
		expect(bytesIfKnown(0)).toBeNull();
		expect(bytesIfKnown(Number.NaN)).toBeNull();
	});
});

describe('tooLargeLine', () => {
	it('states both numbers, because either one alone says too little', () => {
		// Without the file's own size it reads as an arbitrary refusal; without
		// the ceiling it does not say how far over it was.
		const line = tooLargeLine(4 * 1024 * 1024);
		expect(line).toContain('4.0 MB');
		expect(line).toContain('256 KB');
	});

	it('names the ceiling the loader actually enforces', () => {
		// The sentence and the check must be the same number. A hardcoded `256 KB`
		// here would still read correctly with the constant changed underneath it.
		expect(tooLargeLine(PREVIEW_MAX_BYTES + 1)).toContain(bytesIfKnown(PREVIEW_MAX_BYTES));
	});
});
