/**
 * Reading an output file **in place**: what a file can be shown as, and how the
 * bytes that came back are shaped for that.
 *
 * Pure, and deliberately the only place either decision is made. The fetch lives
 * in `taskOutputs.ts` because the panel does not fetch, and the render lives in
 * the panel because the module does not draw; what is here is the two answers in
 * between, so the surface deciding whether a file *can* be previewed and the
 * panel deciding *how* to draw it cannot come to different conclusions about the
 * same file.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`, *Reading an output in
 * place*.
 */

/**
 * How a file's bytes are drawn, once they are on screen.
 *
 * **Its own union rather than a widening of `OutputKind`**, which is the same
 * separation `RunStepStatus` keeps from `VerdictState` and for the same reason:
 * the two answer different questions over different state spaces. `OutputKind`
 * exists to choose a plural for a one-line summary — `report.md and 2 images` —
 * and `KIND_PLURAL` is a `Record` over it, so adding `json` and `csv` there
 * would force `2 json files` and `2 csv files` into a line whose own doc
 * comment argues that `2 data files` tells a reader nothing `2 other files`
 * does not. This one exists to choose a renderer, where those distinctions are
 * the whole point.
 *
 * `null` is not a member: **a file with no preview is offered none**, rather
 * than a seventh kind meaning "broken".
 */
export type PreviewKind = 'markdown' | 'image' | 'json' | 'csv' | 'code' | 'text';

/**
 * The ceiling, and the one the retired panel used. Over it the row says so and
 * offers Open — it does not silently truncate, and it does not put four
 * megabytes of text into a drawer.
 */
export const PREVIEW_MAX_BYTES = 256 * 1024;

/**
 * The most rows a table preview draws. A preview is a disclosure inside a row
 * inside an act; a fifty-thousand-line table there is the wall this whole
 * feature is written to avoid. The remainder is **counted and stated**, never
 * dropped in silence.
 */
export const PREVIEW_MAX_ROWS = 200;

/**
 * Media types that carry a real answer. Anything else falls through to the
 * extension, and anything neither answers is offered no preview at all.
 */
const MEDIA_TYPE_KIND: Record<string, PreviewKind> = {
	'text/markdown': 'markdown',
	'text/x-markdown': 'markdown',
	'application/json': 'json',
	'text/json': 'json',
	'application/x-ndjson': 'text',
	'text/csv': 'csv',
	'application/csv': 'csv',
	'text/tab-separated-values': 'csv'
};

/**
 * The media type of a server that did not look. It is not evidence of anything,
 * so it defers to the name entirely — including on whether there is a preview at
 * all, which is why it is a set rather than another `text/`-style fallthrough.
 *
 * **`text/plain` is deliberately not here**, and it was, which mutation caught:
 * it was already handled by the `text/*` branch below — which refines a generic
 * text type by extension and falls back to plain text — so the set entry and the
 * branch were two mechanisms answering one question, and neither was pinned. The
 * branch is the one that does the work, because it also has to serve
 * `text/x-rust` and every other text type nothing enumerates.
 */
const NO_ANSWER_MEDIA_TYPES = new Set(['application/octet-stream']);

const MARKDOWN_EXTENSIONS = new Set(['md', 'markdown', 'mdx']);
const IMAGE_EXTENSIONS = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'avif', 'ico']);
const TABLE_EXTENSIONS = new Set(['csv', 'tsv']);
const TEXT_EXTENSIONS = new Set(['txt', 'log', 'out', 'err', 'text']);

/**
 * Extensions drawn as source. **`html` is here rather than rendered**, which is
 * the one entry worth explaining: a task's HTML output is untrusted content, and
 * a panel that rendered it would be running a task's markup inside the app's own
 * document. Source is the honest view and the safe one, and the row still offers
 * `In tab` for anyone who wants the rendered page in a context of its own.
 */
const CODE_EXTENSIONS = new Set([
	'ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'py', 'rs', 'go', 'rb', 'java', 'kt', 'swift',
	'c', 'h', 'cpp', 'hpp', 'cc', 'cs', 'php', 'sh', 'bash', 'zsh', 'fish', 'sql', 'css',
	'scss', 'less', 'html', 'htm', 'xml', 'yaml', 'yml', 'toml', 'ini', 'cfg', 'conf',
	'svelte', 'vue', 'lua', 'pl', 'r', 'jl', 'ex', 'exs', 'dart', 'scala', 'hs', 'proto',
	'tf', 'nix', 'diff', 'patch', 'jsonl', 'ndjson', 'env', 'gradle', 'mk', 'make'
]);

/** The extension, lowercased, or `''`. A dotfile has none — `.env` is a name. */
function extensionOf(path: string): string {
	const base = path.split('/').pop() ?? path;
	const dot = base.lastIndexOf('.');
	return dot <= 0 ? '' : base.slice(dot + 1).toLowerCase();
}

function kindByExtension(path: string): PreviewKind | null {
	const extension = extensionOf(path);
	if (!extension) return null;
	if (MARKDOWN_EXTENSIONS.has(extension)) return 'markdown';
	if (IMAGE_EXTENSIONS.has(extension)) return 'image';
	if (extension === 'json') return 'json';
	if (TABLE_EXTENSIONS.has(extension)) return 'csv';
	if (TEXT_EXTENSIONS.has(extension)) return 'text';
	if (CODE_EXTENSIONS.has(extension)) return 'code';
	return null;
}

/**
 * What this file can be shown as, or `null` when the answer is **nothing** — a
 * PDF, an archive, a video, a name with no extension and a server that sent no
 * mime. `null` is a refusal rather than a fallback: an empty monospace block
 * under a `.zip` looks exactly like a file that turned out to be empty, and this
 * panel's whole thesis is that nothing on it may assert more than it knows.
 *
 * The mime is read first and the parameters are stripped, so
 * `text/csv; charset=utf-8` is a table. A `text/*` the list above does not name
 * defers to the extension — a `text/x-rust` body on a `.rs` path is source, and
 * on a path that says nothing it is still text a reader can read.
 *
 * **The `text/*` fallthrough is where `text/plain` is answered**, and that
 * inverts `outputKindOf`'s rule that the mime is authoritative, on purpose: that
 * function is choosing a noun for a summary, where a wrong guess costs a plural.
 * This one is choosing a renderer, where `text/plain` on a `.csv` — which is
 * what real producers send — costs the reader the table they opened the row for.
 */
export function previewKindOf(mediaType: string | null | undefined, path: string): PreviewKind | null {
	const mime = (mediaType ?? '').split(';')[0].trim().toLowerCase();

	if (mime && !NO_ANSWER_MEDIA_TYPES.has(mime)) {
		if (mime.startsWith('image/')) return 'image';
		const known = MEDIA_TYPE_KIND[mime];
		if (known) return known;
		if (mime.endsWith('+json')) return 'json';
		// Any text type, named or not: the extension refines it, and plain text
		// is the floor rather than a guess — the server said it is text.
		if (mime.startsWith('text/')) return kindByExtension(path) ?? 'text';
		// application/pdf, application/zip, audio/*, video/*, everything else.
		return null;
	}

	// No mime, or one that told us nothing: the name is all there is, and when
	// the name says nothing either the answer is no preview rather than an empty
	// monospace block indistinguishable from a file that really is empty.
	return kindByExtension(path);
}

/**
 * The separator a table preview splits on. TSV is the only thing here that is
 * not a comma, and it is identified by name or by mime rather than by sniffing
 * the body — a comma-free CSV would sniff as tab-separated and become one wide
 * column.
 */
export function delimiterOf(mediaType: string | null | undefined, path: string): string {
	const mime = (mediaType ?? '').split(';')[0].trim().toLowerCase();
	if (mime === 'text/tab-separated-values') return '\t';
	return extensionOf(path) === 'tsv' ? '\t' : ',';
}

/**
 * A size, scaled to the largest unit that leaves a number worth reading.
 * `null` for a size the record did not carry, and for zero — a zero-byte
 * output is a real thing, but `0 B` beside a filename reads as a rendering bug
 * rather than as a fact, and the row already names the file.
 *
 * **One formatter, two readers.** The output row's meta line and the
 * over-the-ceiling sentence both state a size, and two functions rounding
 * bytes slightly differently would put `20 KB` on the row and `19.9 KB` in the
 * sentence beneath it.
 */
export function bytesIfKnown(size: number | null): string | null {
	if (size === null || !Number.isFinite(size) || size <= 0) return null;
	const units = ['B', 'KB', 'MB', 'GB'];
	let value = size;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value >= 10 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

/**
 * What the row says instead of a preview, when the file is over the ceiling.
 * It states **both** numbers: what this file is, and what the limit is. A
 * sentence carrying only the first reads as an arbitrary refusal, and one
 * carrying only the second does not say how far over it was.
 */
export function tooLargeLine(sizeBytes: number): string {
	return `${bytesIfKnown(sizeBytes) ?? 'This file'} is over the ${bytesIfKnown(PREVIEW_MAX_BYTES)} preview limit`;
}

/**
 * JSON, indented, or `null` when the bytes are not JSON.
 *
 * `null` rather than an error: a file whose name says JSON and whose bytes are
 * a truncated write, or newline-delimited records, is still text a reader can
 * read, and the caller renders it as-is. What it must not do is claim to have
 * formatted something it could not parse.
 */
export function prettyJson(text: string): string | null {
	try {
		return JSON.stringify(JSON.parse(text), null, 2);
	} catch {
		return null;
	}
}

export interface PreviewTable {
	/** Every row, first one first. The first is drawn as the header. */
	rows: string[][];
	/** Rows past `PREVIEW_MAX_ROWS`, counted so the panel can say how many. */
	truncatedRows: number;
}

/**
 * Delimited text as rows, per RFC 4180: a quoted field may contain the
 * delimiter, a newline, and `""` for a literal quote.
 *
 * Hand-rolled rather than pulled in, because the whole rule is twenty lines and
 * the alternative is a dependency for one preview. Naive `split(',')` is what
 * this exists instead of — it turns `"Acme, Inc.",120` into three cells and
 * silently shifts every column after it, which is worse than showing the raw
 * text because it looks right.
 *
 * A trailing newline does not make an empty last row; a genuinely blank line in
 * the middle does, because that is a row of one empty field and dropping it
 * would misalign nothing but would hide a malformed file.
 */
export function parseDelimited(text: string, delimiter: string): PreviewTable {
	const rows: string[][] = [];
	let row: string[] = [];
	let field = '';
	let quoted = false;
	let started = false;

	const endField = (): void => {
		row.push(field);
		field = '';
		started = false;
	};
	const endRow = (): void => {
		endField();
		rows.push(row);
		row = [];
	};

	for (let index = 0; index < text.length; index += 1) {
		const char = text[index];
		if (quoted) {
			if (char !== '"') {
				field += char;
			} else if (text[index + 1] === '"') {
				field += '"';
				index += 1;
			} else {
				quoted = false;
			}
			continue;
		}
		if (char === '"' && !started) {
			quoted = true;
			started = true;
			continue;
		}
		if (char === delimiter) {
			endField();
			continue;
		}
		if (char === '\r') continue;
		if (char === '\n') {
			endRow();
			continue;
		}
		field += char;
		started = true;
	}
	// The last row only exists if something followed the last newline.
	if (field.length > 0 || row.length > 0) endRow();

	const truncatedRows = Math.max(0, rows.length - PREVIEW_MAX_ROWS);
	return { rows: truncatedRows > 0 ? rows.slice(0, PREVIEW_MAX_ROWS) : rows, truncatedRows };
}

/**
 * Where a preview got to. `loading` is the only one that renders no content and
 * is not a claim about the file — the other three each say something the reader
 * can act on.
 */
export type PreviewStatus = 'loading' | 'ready' | 'too-large' | 'failed';

/**
 * One file's contents, on their way to the panel.
 *
 * **It carries no kind**, and that absence is load-bearing. How to draw a file
 * is `previewKindOf` over the row the panel is already holding, and a second
 * copy travelling with the bytes would be a second answer to one question — the
 * shape of defect this feature's tests exist to catch. The loader still asks
 * `previewKindOf` itself, to know whether there is anything to fetch at all; it
 * just does not publish the answer.
 *
 * `index` is into `output.files`, and it is what keeps a slow reply for one row
 * from rendering under another. The panel checks it before drawing anything.
 */
export interface TaskFilePreview {
	index: number;
	status: PreviewStatus;
	/** The bytes, as text. `null` unless `status` is `ready`. */
	text: string | null;
	/**
	 * The sentence that explains a status that is not `ready` — why it failed,
	 * or how far over the ceiling it was. `null` when the status speaks for
	 * itself.
	 */
	detail: string | null;
}
