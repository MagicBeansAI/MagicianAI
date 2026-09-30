/**
 * Shared HTML sanitizer for published-surface rendering.
 *
 * Allowlist-based: every tag and attribute must be in the allowlist; URLs
 * go through a scheme check; `style` values go through a property allowlist;
 * `on*` handlers, `<script>`, `<iframe>`, `<object>`, etc. are dropped.
 *
 * Mirrors the inline sanitizer in `Markdown.svelte` so both renderers behave
 * identically. One special addition for `HtmlDashboard`: the
 * `data-magician-source` attribute is allowed on every tag — the post-render
 * scanner uses it to find live-data chart placeholders.
 */

const HTML_BLOCK_TAGS = new Set([
	'div',
	'section',
	'article',
	'header',
	'footer',
	'main',
	'aside',
	'nav',
	'figure',
	'figcaption',
	'p',
	'blockquote',
	'pre',
	'h1',
	'h2',
	'h3',
	'h4',
	'h5',
	'h6',
	'ul',
	'ol',
	'li',
	'table',
	'thead',
	'tbody',
	'tfoot',
	'tr',
	'th',
	'td',
	'colgroup',
	'col',
	'details',
	'summary',
	'hr'
]);

const HTML_INLINE_TAGS = new Set([
	'span',
	'strong',
	'em',
	'code',
	'a',
	'small',
	'mark',
	'del',
	'ins',
	'sub',
	'sup',
	'kbd',
	'samp',
	'var',
	'b',
	'i',
	'u',
	'br',
	'img',
	'abbr',
	'cite',
	'time',
	'q'
]);

const ALLOWED_HTML_TAGS = new Set([...HTML_BLOCK_TAGS, ...HTML_INLINE_TAGS]);

const DROP_HTML_TAGS = new Set([
	'script',
	'style',
	'iframe',
	'object',
	'embed',
	'form',
	'input',
	'button',
	'textarea',
	'select',
	'option',
	'link',
	'meta',
	'base',
	'frame',
	'frameset',
	'applet',
	'audio',
	'video',
	'source',
	'track',
	'canvas',
	'svg',
	'math'
]);

const ALLOWED_HTML_ATTRS_GLOBAL = new Set([
	'class',
	'id',
	'title',
	'style',
	'lang',
	'dir',
	'data-magician-source'
]);

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

const SAFE_STYLE_PROP =
	/^(color|background|background-color|font-(size|weight|style|family|variant)|text-(align|decoration|transform|indent|shadow)|padding|padding-(top|right|bottom|left)|margin|margin-(top|right|bottom|left)|border|border-(top|right|bottom|left|color|width|style|radius|collapse|spacing)|width|max-width|min-width|height|max-height|min-height|display|flex|flex-(direction|wrap|grow|shrink|basis|flow)|justify-content|align-(items|self|content)|gap|row-gap|column-gap|grid-(template|template-columns|template-rows|gap|column|row|column-gap|row-gap)|opacity|line-height|letter-spacing|word-spacing|white-space|overflow|overflow-(x|y)|vertical-align|cursor|list-style|list-style-(type|position|image)|float|clear|box-shadow|box-sizing|object-fit|object-position|filter|backdrop-filter|transform|transition|position|top|right|bottom|left|z-index)$/i;

const DANGEROUS_STYLE_PATTERN = /url\s*\(|e[x]pression\s*\(|javascript:|behavior:|<|@import/i;

function sanitizeStyle(value: string): string {
	if (DANGEROUS_STYLE_PATTERN.test(value)) return '';
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
	if (t.startsWith('http://') || t.startsWith('https://') || t.startsWith('mailto:') || t.startsWith('#'))
		return true;
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
	)
		return true;
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

export function sanitizeHtml(html: string): string {
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
