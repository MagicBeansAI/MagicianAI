/**
 * Flatten a markdown snippet into one line of plain preview text.
 *
 * Images drop entirely, links keep their label (the URL is stripped —
 * naive marker-stripping leaves `](http://…)` fragments leaking into
 * teasers), residual emphasis/heading/quote markers collapse to spaces,
 * and whitespace normalizes to single spaces.
 *
 * Shared by the Today scroller previews and ChatPanel's collapsed
 * task-run teaser line.
 */
export function stripMarkdownPreview(value: string): string {
	return value
		.replace(/!\[[^\]]*\]\([^)]+\)/g, ' ')
		.replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
		.replace(/[*_`>#~\-]+/g, ' ')
		.replace(/\s+/g, ' ')
		.trim();
}
