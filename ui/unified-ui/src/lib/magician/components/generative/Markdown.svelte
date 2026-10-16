<script lang="ts">
	/**
	 * Markdown Component — GD-F02-F
	 *
	 * Minimal markdown parser without external dependencies.
	 * Handles: headers, bold, italic, code, links, line breaks, GFM tables,
	 * and embedded HTML blocks via an allowlist sanitizer (so reports can
	 * mix MD prose with HTML for richer layout — multi-row tables, badges,
	 * <details>, etc.).
	 * XSS-safe: HTML escapes inline text; embedded HTML blocks pass
	 * through a tag/attribute allowlist with safe-URL and safe-style checks.
	 */

	export let content: unknown = '';
	/**
	 * Deprecated — retained for prop-stability while callers migrate.
	 *
	 * Was introduced in v0.6.601 as a streaming fast-path: when `true`,
	 * the component bypassed the markdown parse + `{@html}` and rendered
	 * plain text instead, on the theory that the per-token DOM
	 * replacement was driving the visible bubble re-render during
	 * streaming. v0.6.604 traced the actual cause to `animate:flip` on
	 * the parent `.chat-message-row` (Svelte's FLIP fires on every
	 * bounding-rect change, not just reorder — so token-driven height
	 * growth queued 25+ overlapping 260ms transform animations and
	 * visually read as "the bubble keeps re-rendering"). With that
	 * fixed at the row level, the markdown parse + `{@html}` per-token
	 * is fast enough on modern engines (sub-millisecond for short
	 * chat content) that bypassing it produces no measurable benefit
	 * and costs the user formatted markdown during the stream.
	 *
	 * The prop is now a no-op — markdown renders the same regardless.
	 * Kept around so chat-page call sites can drop their
	 * `streaming={...}` argument lazily; once they have, this can be
	 * removed.
	 */
	export let streaming = false;
	// Reference the prop so the unused-warning doesn't fire while
	// callers migrate. Has no runtime effect.
	void streaming;

	function toString(value: unknown): string {
		return typeof value === 'string' ? value : '';
	}

	function escapeHtml(text: string): string {
		return text
			.replace(/&/g, '&amp;')
			.replace(/</g, '&lt;')
			.replace(/>/g, '&gt;')
			.replace(/"/g, '&quot;')
			.replace(/'/g, '&#x27;');
	}

	function isSafeUrl(url: string): boolean {
		const trimmed = url.trim().toLowerCase();
		if (trimmed.startsWith('http://') || trimmed.startsWith('https://') || trimmed.startsWith('mailto:')) {
			return true;
		}
		if (trimmed.startsWith('/') && !trimmed.startsWith('//')) {
			return true;
		}
		if (/^[a-z][a-z0-9+.-]*:/i.test(trimmed)) {
			return false;
		}
		if (trimmed.startsWith('javascript:') || trimmed.startsWith('data:') || trimmed.startsWith('vbscript:')) {
			return false;
		}
		return true;
	}

	const HTML_BLOCK_TAGS = new Set([
		'table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td', 'caption', 'colgroup', 'col',
		'details', 'summary',
		'section', 'article', 'aside', 'header', 'footer', 'nav', 'main',
		'div', 'figure', 'figcaption', 'blockquote',
		'dl', 'dt', 'dd',
		'ol', 'ul', 'li',
		'pre', 'hr',
		'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'p'
	]);

	const HTML_INLINE_TAGS = new Set([
		'span', 'a', 'strong', 'em', 'b', 'i', 'u', 's', 'strike', 'del', 'ins',
		'code', 'kbd', 'mark', 'sub', 'sup', 'small', 'samp', 'var', 'q',
		'br', 'img', 'abbr', 'cite', 'time'
	]);

	const ALLOWED_HTML_TAGS = new Set([...HTML_BLOCK_TAGS, ...HTML_INLINE_TAGS]);

	const DROP_HTML_TAGS = new Set([
		'script', 'style', 'iframe', 'object', 'embed', 'form',
		'input', 'button', 'textarea', 'select', 'option',
		'link', 'meta', 'base', 'frame', 'frameset', 'applet',
		'audio', 'video', 'source', 'track', 'canvas', 'svg', 'math'
	]);

	const ALLOWED_HTML_ATTRS_GLOBAL = new Set(['class', 'id', 'title', 'style', 'lang', 'dir']);
	const ALLOWED_HTML_ATTRS_PER_TAG: Record<string, Set<string>> = {
		a: new Set(['href', 'target', 'rel']),
		img: new Set(['src', 'alt', 'width', 'height']),
		td: new Set(['colspan', 'rowspan', 'align', 'valign', 'headers']),
		th: new Set(['colspan', 'rowspan', 'align', 'valign', 'scope']),
		table: new Set(['align', 'border', 'cellpadding', 'cellspacing']),
		col: new Set(['span', 'align']),
		colgroup: new Set(['span', 'align']),
		details: new Set(['open']),
		ol: new Set(['start', 'reversed', 'type']),
		blockquote: new Set(['cite']),
		q: new Set(['cite']),
		time: new Set(['datetime'])
	};

	const SAFE_STYLE_PROP = /^(color|background|background-color|font-(size|weight|style|family|variant)|text-(align|decoration|transform|indent|shadow)|padding|padding-(top|right|bottom|left)|margin|margin-(top|right|bottom|left)|border|border-(top|right|bottom|left|color|width|style|radius|collapse|spacing)|width|max-width|min-width|height|max-height|min-height|display|flex|flex-(direction|wrap|grow|shrink|basis|flow)|justify-content|align-(items|self|content)|gap|row-gap|column-gap|grid-(template|template-columns|template-rows|gap|column|row|column-gap|row-gap)|opacity|line-height|letter-spacing|word-spacing|white-space|overflow|overflow-(x|y)|vertical-align|cursor|list-style|list-style-(type|position|image)|float|clear|box-shadow|box-sizing|object-fit|object-position|filter|backdrop-filter|transform|transition|position|top|right|bottom|left|z-index)$/i;

	function sanitizeStyle(value: string): string {
		if (/url\s*\(|expression\s*\(|javascript:|behavior:|<|@import/i.test(value)) {
			return '';
		}
		return value
			.split(';')
			.map((d) => d.trim())
			.filter((d) => {
				const idx = d.indexOf(':');
				if (idx < 0) return false;
				const prop = d.slice(0, idx).trim();
				return SAFE_STYLE_PROP.test(prop);
			})
			.join('; ');
	}

	function isSafeHref(url: string): boolean {
		const t = url.trim().toLowerCase();
		if (t.startsWith('http://') || t.startsWith('https://') || t.startsWith('mailto:') || t.startsWith('#')) return true;
		if (t.startsWith('/') && !t.startsWith('//')) return true;
		return false;
	}

	function isSafeImgSrc(url: string): boolean {
		const t = url.trim().toLowerCase();
		if (t.startsWith('http://') || t.startsWith('https://')) return true;
		if (
			t.startsWith('data:image/png') ||
			t.startsWith('data:image/jpeg') ||
			t.startsWith('data:image/jpg') ||
			t.startsWith('data:image/gif') ||
			t.startsWith('data:image/webp') ||
			t.startsWith('data:image/svg+xml')
		) {
			return true;
		}
		if (t.startsWith('/') && !t.startsWith('//')) return true;
		return false;
	}

	function escapeAttr(value: string): string {
		return value.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
	}

	function sanitizeAttrs(tag: string, attrs: string): string {
		if (!attrs.trim()) return '';
		const allowedForTag = ALLOWED_HTML_ATTRS_PER_TAG[tag] ?? new Set<string>();
		const out: string[] = [];
		const seen = new Set<string>();
		const re = /\s*([a-zA-Z_][a-zA-Z0-9_:.-]*)\s*(?:=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>`]+)))?/g;
		let m: RegExpExecArray | null;
		let hasTargetBlank = false;
		let hasRel = false;
		while ((m = re.exec(attrs)) !== null) {
			if (!m[1]) continue;
			const name = m[1].toLowerCase();
			if (seen.has(name)) continue;
			seen.add(name);
			const value = m[2] ?? m[3] ?? m[4] ?? '';
			if (name.startsWith('on')) continue;
			if (!ALLOWED_HTML_ATTRS_GLOBAL.has(name) && !allowedForTag.has(name)) continue;
			if (name === 'href') {
				if (!isSafeHref(value)) continue;
			} else if (name === 'src') {
				if (!isSafeImgSrc(value)) continue;
			} else if (name === 'style') {
				const cleaned = sanitizeStyle(value);
				if (!cleaned) continue;
				out.push(` style="${cleaned.replace(/"/g, '&quot;')}"`);
				continue;
			} else if (name === 'target') {
				if (value === '_blank') hasTargetBlank = true;
			} else if (name === 'rel') {
				hasRel = true;
			}
			out.push(` ${name}="${escapeAttr(value)}"`);
		}
		if (hasTargetBlank && !hasRel) {
			out.push(` rel="noopener noreferrer"`);
		}
		return out.join('');
	}

	function sanitizeHtmlBlock(html: string): string {
		let s = html
			.replace(/<!--[\s\S]*?-->/g, '')
			.replace(/<\?[\s\S]*?\?>/g, '')
			.replace(/<!\[CDATA\[[\s\S]*?\]\]>/g, '')
			.replace(/<!doctype[\s\S]*?>/gi, '');

		for (const dropTag of DROP_HTML_TAGS) {
			const reBlock = new RegExp(`<${dropTag}\\b[^>]*>[\\s\\S]*?</${dropTag}\\s*>`, 'gi');
			s = s.replace(reBlock, '');
			const reSelf = new RegExp(`<${dropTag}\\b[^>]*/?>`, 'gi');
			s = s.replace(reSelf, '');
		}

		s = s.replace(/<\/?([a-zA-Z][a-zA-Z0-9]*)\b([^>]*)>/g, (full, rawTag, attrs) => {
			const tag = rawTag.toLowerCase();
			if (!ALLOWED_HTML_TAGS.has(tag)) return '';
			const isClosing = full.startsWith('</');
			if (isClosing) return `</${tag}>`;
			const isSelfClosing = /\/\s*$/.test(attrs);
			const cleanedAttrs = sanitizeAttrs(tag, attrs.replace(/\/\s*$/, ''));
			return `<${tag}${cleanedAttrs}${isSelfClosing ? ' />' : '>'}`;
		});

		return s;
	}

	function detectHtmlBlockTag(line: string): string | null {
		const m = line.trim().match(/^<([a-zA-Z][a-zA-Z0-9]*)\b/);
		if (!m) return null;
		const tag = m[1].toLowerCase();
		if (!HTML_BLOCK_TAGS.has(tag)) return null;
		return tag;
	}

	function findHtmlBlockEnd(lines: string[], startIdx: number, tag: string): number {
		const openRe = new RegExp(`<${tag}\\b`, 'gi');
		const closeRe = new RegExp(`</${tag}\\s*>`, 'gi');
		let depth = 0;
		for (let i = startIdx; i < lines.length; i++) {
			const line = lines[i];
			depth += (line.match(openRe) ?? []).length;
			depth -= (line.match(closeRe) ?? []).length;
			if (depth <= 0 && i >= startIdx) return i + 1;
		}
		return lines.length;
	}

	function inlineTransforms(text: string): string {
		return text
			.replace(/\[([^\]]+?)\]\(([^)]+?)\)/g, (_, label, url) => {
				const safeUrl = isSafeUrl(url) ? url : '#';
				return `<a class="muij-md-link" href="${safeUrl}" target="_blank" rel="noopener noreferrer">${label}</a>`;
			})
			.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
			.replace(/\*(.+?)\*/g, '<em>$1</em>')
			.replace(/`(.+?)`/g, '<code class="muij-md-code">$1</code>');
	}

	function isTableSeparator(line: string): boolean {
		const trimmed = line.trim();
		if (!trimmed.startsWith('|') || !trimmed.endsWith('|')) return false;
		const inner = trimmed.slice(1, -1);
		const cells = inner.split('|');
		if (cells.length === 0) return false;
		return cells.every((cell) => /^\s*:?-{3,}:?\s*$/.test(cell));
	}

	function parseTableAlignment(separator: string): Array<'left' | 'center' | 'right' | null> {
		const inner = separator.trim().slice(1, -1);
		return inner.split('|').map((cell) => {
			const t = cell.trim();
			const left = t.startsWith(':');
			const right = t.endsWith(':');
			if (left && right) return 'center';
			if (right) return 'right';
			if (left) return 'left';
			return null;
		});
	}

	function splitTableRow(line: string): string[] {
		const trimmed = line.trim();
		if (trimmed.startsWith('|') && trimmed.endsWith('|')) {
			return trimmed.slice(1, -1).split('|').map((c) => c.trim());
		}
		return trimmed.split('|').map((c) => c.trim());
	}

	function renderTable(headerLine: string, separatorLine: string, bodyLines: string[]): string {
		const alignments = parseTableAlignment(separatorLine);
		const headers = splitTableRow(headerLine);
		const renderCell = (text: string, align: 'left' | 'center' | 'right' | null, tag: 'th' | 'td'): string => {
			const escaped = escapeHtml(text);
			const styled = inlineTransforms(escaped);
			const style = align ? ` style="text-align: ${align};"` : '';
			const cls = tag === 'th' ? 'muij-md-th' : 'muij-md-td';
			return `<${tag} class="${cls}"${style}>${styled}</${tag}>`;
		};
		const headRow = headers.map((h, i) => renderCell(h, alignments[i] ?? null, 'th')).join('');
		const bodyRows = bodyLines
			.map((line) => splitTableRow(line))
			.map((cells) => `<tr class="muij-md-tr">${cells.map((c, i) => renderCell(c, alignments[i] ?? null, 'td')).join('')}</tr>`)
			.join('');
		return `<table class="muij-md-table"><thead class="muij-md-thead"><tr class="muij-md-tr">${headRow}</tr></thead><tbody class="muij-md-tbody">${bodyRows}</tbody></table>`;
	}

	function parseMarkdown(text: string): string {
		// Strip trailing whitespace before splitting so a source string
		// with a trailing `\n` (common when chat content round-trips
		// through textarea → backend → display) doesn't produce a
		// trailing `<br>` after the `.replace(/\n/g, '<br>')` below.
		// That trailing `<br>` rendered as a full extra line-height of
		// empty space at the bottom of every chat bubble whose source
		// ended with a newline — operator-visible as "every bubble is
		// taller than it needs to be."
		//
		// `trimEnd` only strips trailing whitespace; leading whitespace
		// stays (preserves indented blocks at the top of a message).
		const lines = text.replace(/\s+$/u, '').split('\n');
		const blocks: string[] = [];
		let buffer: string[] = [];

		const flushBuffer = () => {
			if (buffer.length === 0) return;
			let chunk = escapeHtml(buffer.join('\n'));
			chunk = chunk
				.replace(/^(?:---|___|\*\*\*)$/gm, '<hr class="muij-md-hr">')
				.replace(/(?:^&gt; ?.*(?:\n|$))+/gm, (block) => {
					const inner = block
						.split('\n')
						.filter((line) => line.length > 0)
						.map((line) => line.replace(/^&gt; ?/, ''))
						.join('<br>');
					return `<blockquote class="muij-md-quote">${inner}</blockquote>\n`;
				})
				.replace(/^### (.+)$/gm, '<h3 class="muij-md-h3">$1</h3>')
				.replace(/^## (.+)$/gm, '<h2 class="muij-md-h2">$1</h2>')
				.replace(/^# (.+)$/gm, '<h1 class="muij-md-h1">$1</h1>')
				.replace(/^\d+\. (.+)$/gm, '<li class="muij-md-oli">$1</li>')
				.replace(/(<li class="muij-md-oli">.*<\/li>\n?)+/g, '<ol class="muij-md-ol">$&</ol>')
				.replace(/^[*-] (.+)$/gm, '<li class="muij-md-li">$1</li>')
				.replace(/(<li class="muij-md-li">.*<\/li>\n?)+/g, '<ul class="muij-md-ul">$&</ul>');

			// Split on blank lines so every paragraph gets a real
			// `<p>...</p>` wrapper. The previous implementation replaced
			// `\n\n` with `</p><p>`, which leaves the FIRST paragraph as
			// bare text (no opening `<p>`) and the LAST paragraph
			// unclosed. The browser then renders the first paragraph as
			// inline text with no margin while wrapping every subsequent
			// paragraph in its own `<p>` with a top/bottom margin —
			// visible during streaming as a layout "jump" whenever a new
			// paragraph appeared and again whenever the streaming
			// re-render shifted where the implicit boundaries land.
			// Wrapping every paragraph uniformly makes adjacent `<p>`
			// margins collapse predictably, so the chat bubble grows
			// linearly as text streams in instead of swelling at each
			// new paragraph break.
			// Stripping newlines adjacent to block-level HTML tags BEFORE
			// the `\n → <br>` step below. Without this, the parser emits
			// `<ul>\n<li>one</li>\n<li>two</li>\n</ul>` (because the
			// heading / list transforms above re-emit block tags onto
			// their own source lines), and `.replace(/\n/g, '<br>')`
			// then injects `<br>` tags between every list item AND
			// before/after the `<ul>`. The browser renders each `<br>`
			// as a full line-height of empty space — operator-visible
			// as a wide gap between list items, after headings, etc.
			//
			// Done inside the per-paragraph map (rather than chunk-level
			// before the split) so a real blank-line boundary that
			// happened to be adjacent to a block tag (e.g.
			// `Intro.\n\n<ul>...`) is still respected as a paragraph
			// split — only newlines INTERNAL to a paragraph chunk are
			// touched here.
			const BLOCK_TAGS_RE_SRC =
				'ul|ol|li|h[1-6]|p|table|thead|tbody|tr|td|th|caption|colgroup|col|' +
				'blockquote|details|summary|pre|hr|div|section|article|aside|' +
				'header|footer|nav|main|figure|figcaption|dl|dt|dd';
			const stripBlockAdjacentNewlines = (s: string): string =>
				s
					.replace(new RegExp(`\\n+(?=<(?:${BLOCK_TAGS_RE_SRC})\\b)`, 'gi'), '')
					.replace(new RegExp(`(<\\/(?:${BLOCK_TAGS_RE_SRC})>)\\n+`, 'gi'), '$1')
					.replace(new RegExp(`(<(?:${BLOCK_TAGS_RE_SRC})\\b[^>]*>)\\n+`, 'gi'), '$1')
					.replace(new RegExp(`\\n+(?=<\\/(?:${BLOCK_TAGS_RE_SRC})>)`, 'gi'), '');

			const paragraphs = chunk.split(/\n{2,}/);
			chunk = paragraphs
				.map((paragraph) => {
					const cleaned = stripBlockAdjacentNewlines(paragraph);
					const withBreaks = cleaned.replace(/\n/g, '<br>');
					// Skip wrapping when the paragraph already starts
					// with a block-level tag we produced above
					// (heading / list). Wrapping a list inside a `<p>`
					// is invalid HTML and the browser would unwrap it
					// anyway — causing the same layout shift this fix
					// is preventing.
					if (/^\s*<(h[1-3]|ul|ol|li|table|blockquote|details|pre|hr)\b/i.test(withBreaks)) {
						return withBreaks;
					}
					const trimmed = withBreaks.replace(/^\s+|\s+$/g, '');
					if (!trimmed) return '';
					return `<p class="muij-md-p">${withBreaks}</p>`;
				})
				.filter((piece) => piece.length > 0)
				.join('');
			chunk = inlineTransforms(chunk);
			blocks.push(chunk);
			buffer = [];
		};

		let i = 0;
		while (i < lines.length) {
			const line = lines[i];
			const fence = line.match(/^ {0,3}```([^`]*)$/);
			if (fence) {
				flushBuffer();
				const language = fence[1].trim();
				const codeLines: string[] = [];
				i += 1;
				while (i < lines.length && !/^ {0,3}```\s*$/.test(lines[i])) {
					codeLines.push(lines[i]);
					i += 1;
				}
				if (i < lines.length) i += 1;
				const langClass = /^[A-Za-z0-9_+-]+$/.test(language)
					? ` class="language-${language}"`
					: '';
				blocks.push(
					`<pre class="muij-md-pre"><code${langClass}>${escapeHtml(codeLines.join('\n'))}</code></pre>`
				);
				continue;
			}

			const blockTag = detectHtmlBlockTag(line);
			if (blockTag) {
				flushBuffer();
				const endIdx = findHtmlBlockEnd(lines, i, blockTag);
				const htmlChunk = lines.slice(i, endIdx).join('\n');
				blocks.push(sanitizeHtmlBlock(htmlChunk));
				i = endIdx;
				continue;
			}

			const isTableHeader =
				line.trim().startsWith('|') &&
				line.trim().endsWith('|') &&
				i + 1 < lines.length &&
				isTableSeparator(lines[i + 1]);
			if (isTableHeader) {
				flushBuffer();
				const headerLine = line;
				const separatorLine = lines[i + 1];
				const body: string[] = [];
				let j = i + 2;
				while (j < lines.length) {
					const next = lines[j];
					if (next.trim().startsWith('|') && next.trim().endsWith('|')) {
						body.push(next);
						j += 1;
					} else {
						break;
					}
				}
				blocks.push(renderTable(headerLine, separatorLine, body));
				i = j;
				continue;
			}

			buffer.push(line);
			i += 1;
		}
		flushBuffer();
		// Belt-and-braces: drop any trailing `<br>` (or chain of them)
		// from the assembled output. The `trimEnd` in `lines` above
		// catches the common case (source string with trailing `\n`),
		// but the `replace(/\n/g, '<br>')` inside `flushBuffer` could
		// still leave a trailing `<br>` if a structured block
		// (heading, list, paragraph break) closes on a newline that
		// the trimming didn't reach. Strip a trailing run of
		// `<br>` / `<br/>` / whitespace at the very end so chat
		// bubbles never carry phantom blank lines.
		return blocks.join('').replace(/(?:<br\s*\/?>|\s)+$/u, '');
	}

	$: safeContent = toString(content);
	$: parsedHtml = parseMarkdown(safeContent);
</script>

{#if safeContent}
	<div class="muij-markdown">
		{@html parsedHtml}
	</div>
{:else}
	<div class="muij-markdown muij-markdown-empty">
		<span class="muij-markdown-empty-text">No content</span>
	</div>
{/if}

<style>
	.muij-markdown {
		font-family: var(--font-primary);
		font-size: 0.875rem;
		line-height: 1.6;
		color: var(--text-primary);
		/* Prevent long unbreakable tokens (file paths, IDs, hashes,
		 * URLs) from pushing the markdown surface wider than its
		 * container. Applied at the root so it inherits across every
		 * generated block, including bare text nodes and `<p>`-wrapped
		 * paragraphs. `<pre>` / `<code>` overrides below opt back out
		 * so source code keeps its own scoped scrollbar instead of
		 * mid-token wraps. */
		overflow-wrap: anywhere;
		word-break: break-word;
		min-width: 0;
	}

	:global(.muij-markdown pre),
	:global(.muij-markdown pre code) {
		max-width: 100%;
		overflow-x: auto;
		overflow-wrap: normal;
		word-break: normal;
	}

	.muij-markdown-empty {
		padding: var(--space-md);
		text-align: center;
		color: var(--text-muted);
	}

	:global(.muij-md-h1) {
		font-size: 1.5rem;
		font-weight: 700;
		margin: var(--space-md) 0 var(--space-sm);
		color: var(--text-primary);
	}

	:global(.muij-md-h2) {
		font-size: 1.25rem;
		font-weight: 600;
		margin: var(--space-sm) 0 var(--space-xs);
		color: var(--text-primary);
	}

	:global(.muij-md-h3) {
		font-size: 1rem;
		font-weight: 600;
		margin: var(--space-sm) 0 var(--space-xs);
		color: var(--text-primary);
	}

	:global(.muij-md-p) {
		margin: var(--space-sm) 0;
	}

	:global(.muij-md-code) {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		background: var(--bg-soft);
		padding: 0.125rem 0.375rem;
		border-radius: var(--radius-sm);
		color: var(--text-primary);
	}

	:global(.muij-md-link) {
		color: var(--accent-primary);
		text-decoration: none;
	}

	:global(.muij-md-link:hover) {
		text-decoration: underline;
	}

	:global(.muij-md-ul),
	:global(.muij-md-ol) {
		margin: var(--space-sm) 0;
		padding-left: var(--space-lg);
	}

	:global(.muij-md-li),
	:global(.muij-md-oli) {
		margin: var(--space-xs) 0;
	}

	:global(.muij-md-pre) {
		margin: var(--space-sm) 0;
		padding: 0.75rem 0.9rem;
		border-radius: var(--radius-sm, 4px);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		overflow-x: auto;
	}

	:global(.muij-md-pre code) {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		white-space: pre;
	}

	:global(.muij-md-hr) {
		margin: var(--space-md) 0;
		border: 0;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
	}

	:global(.muij-md-table) {
		width: 100%;
		border-collapse: collapse;
		margin: var(--space-md) 0;
		font-size: 0.825rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: var(--radius-sm, 4px);
		overflow: hidden;
	}

	:global(.muij-md-thead) {
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
	}

	:global(.muij-md-th) {
		text-align: left;
		font-weight: 600;
		padding: 0.4rem 0.6rem;
		color: var(--text-primary);
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		white-space: nowrap;
	}

	:global(.muij-md-td) {
		padding: 0.35rem 0.6rem;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		vertical-align: top;
	}

	:global(.muij-md-tbody .muij-md-tr:last-child .muij-md-td) {
		border-bottom: none;
	}

	.muij-markdown :global(table) {
		width: 100%;
		border-collapse: collapse;
		margin: var(--space-md) 0;
		font-size: 0.825rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.muij-markdown :global(th),
	.muij-markdown :global(td) {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		vertical-align: top;
		text-align: left;
	}

	.muij-markdown :global(thead) {
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
	}

	.muij-markdown :global(th) {
		font-weight: 600;
		color: var(--text-primary);
	}

	.muij-markdown :global(details) {
		margin: var(--space-sm) 0;
		padding: var(--space-sm) var(--space-md);
		background: var(--bg-soft, rgba(0, 0, 0, 0.03));
		border-radius: var(--radius-sm, 4px);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.muij-markdown :global(details summary) {
		cursor: pointer;
		font-weight: 600;
		color: var(--text-primary);
	}

	.muij-markdown :global(blockquote) {
		margin: var(--space-sm) 0;
		padding: 0.4rem var(--space-md);
		border-left: 3px solid var(--accent-primary, rgba(0, 0, 0, 0.18));
		background: var(--bg-soft, rgba(0, 0, 0, 0.03));
		color: var(--text-secondary, var(--text-body));
	}

	.muij-markdown :global(kbd) {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		padding: 0.1rem 0.35rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.18));
		border-bottom-width: 2px;
		border-radius: 3px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		color: var(--text-primary);
	}

	.muij-markdown :global(mark) {
		background: var(--accent-soft, rgba(255, 230, 0, 0.45));
		color: inherit;
		padding: 0 0.2rem;
		border-radius: 2px;
	}

	.muij-markdown :global(.badge),
	.muij-markdown :global(.pill) {
		display: inline-block;
		padding: 0.1rem 0.55rem;
		border-radius: 999px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.08));
		color: var(--text-primary);
		font-size: 0.75rem;
		font-weight: 600;
		line-height: 1.4;
	}

	.muij-markdown :global(.callout) {
		margin: var(--space-md) 0;
		padding: var(--space-sm) var(--space-md);
		border-left: 3px solid var(--accent-primary, rgba(0, 0, 0, 0.2));
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border-radius: var(--radius-sm, 4px);
	}
</style>
