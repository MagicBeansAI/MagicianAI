/** Turn `[[Page]]` and `[[Page|label]]` into links the reading pane can open. */
export function rewriteWikiLinks(markdown: string): string {
	const parts = markdown.split(/(```[\s\S]*?```)/g);
	return parts
		.map((part, index) => (index % 2 === 1 ? part : part.replace(/\[\[([^\]\n]+)\]\]/g, wikiLink)))
		.join('');
}

function wikiLink(_full: string, inner: string): string {
	const pipe = inner.indexOf('|');
	const head = (pipe >= 0 ? inner.slice(0, pipe) : inner).trim();
	const label = (pipe >= 0 ? inner.slice(pipe + 1) : head).trim();
	const target = head.split('#')[0]?.trim() ?? '';
	if (!target || !label) return _full;
	return `[${label}](#note/${encodeURIComponent(target)})`;
}

/** Page name carried by a reading-pane link, or a relative Markdown href. */
export function targetFromHref(href: string): string | null {
	if (!href || href.startsWith('http:') || href.startsWith('https:') || href.toLowerCase().startsWith('mailto:')) {
		return null;
	}
	if (href.startsWith('#note/')) {
		try {
			return decodeURIComponent(href.slice('#note/'.length)).trim() || null;
		} catch {
			return null;
		}
	}
	if (href.startsWith('#')) return null;
	const path = href.split('#')[0]?.trim() ?? '';
	if (!path || path.includes('://')) return null;
	const lower = path.toLowerCase();
	if (!lower.endsWith('.md') && !lower.endsWith('.markdown')) return null;
	try {
		return decodeURIComponent(path);
	} catch {
		return path;
	}
}

/** Paths to try, from the notes root and from the note that contains the link. */
export function linkCandidates(fromPath: string, target: string): string[] {
	const normalized = target.replaceAll('\\', '/').trim().replace(/^\/+/, '');
	if (!normalized || normalized.includes('://')) return [];
	const fromDir = fromPath.includes('/') ? fromPath.slice(0, fromPath.lastIndexOf('/') + 1) : '';
	const stems = /\.(md|markdown)$/i.test(normalized)
		? [normalized]
		: [`${normalized}.md`, `${normalized}/index.md`];
	const out: string[] = [];
	for (const stem of stems) {
		for (const base of [stem, `${fromDir}${stem}`]) {
			const safe = normalizeNotePath(base);
			if (safe && !out.includes(safe)) out.push(safe);
		}
	}
	return out;
}

function normalizeNotePath(path: string): string | null {
	const parts: string[] = [];
	for (const part of path.replaceAll('\\', '/').split('/')) {
		if (!part || part === '.') continue;
		if (part === '..') {
			if (parts.length === 0) return null;
			parts.pop();
			continue;
		}
		parts.push(part);
	}
	return parts.length > 0 ? parts.join('/') : null;
}

/**
 * The canonical link to one note: `/notes?path=<path>&workspace=<workspace>`.
 *
 * Relative on purpose: the same link works on every host the owner reaches
 * Magician through, and opening it goes through the normal sign-in and
 * workspace check. The server builds note links with the same encoding
 * (`encodeURIComponent` for both values, `workspace` omitted when empty), so a
 * link a chat reply carries and one this app builds are the same string.
 */
export function noteHref(path: string, workspace?: string | null): string {
	const scope = workspace?.trim() ?? '';
	const query = `path=${encodeURIComponent(path)}`;
	return scope ? `/notes?${query}&workspace=${encodeURIComponent(scope)}` : `/notes?${query}`;
}

/** The absolute form of {@link noteHref}, for copying a note's link to share. */
export function absoluteNoteLink(
	path: string,
	workspace?: string | null,
	origin: string = typeof window === 'undefined' ? '' : window.location.origin
): string {
	return `${origin.replace(/\/+$/, '')}${noteHref(path, workspace)}`;
}
