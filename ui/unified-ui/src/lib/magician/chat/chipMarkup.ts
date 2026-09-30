/**
 * Shared chip rendering primitives.
 *
 * Two surfaces consume this:
 *   - ChipTextarea.svelte (composer, contenteditable editor)
 *   - ChatMarkdown.svelte (transcript bubbles, post-render DOM walker)
 *
 * Single source of truth for the typed-prefix regex, the chip HTML
 * shape, the icon SVGs, and the kind taxonomy — so the chip a user
 * sees while composing is byte-identical to the chip in the sent
 * message bubble.
 */

export type ChipKind = 'agent' | 'skill' | 'personality' | 'task' | 'feature';

export interface ChipToken {
	kind: ChipKind;
	slug: string;
	routeAgent?: string;
}

/**
 * A structured readout of one chip currently sitting in a ChipTextarea field —
 * what `ChipTextarea.chipTokens()` (and the `MentionTextarea` pass-through)
 * returns. Unlike `serializeChipDom`, which flattens a chip to its `kind:slug`
 * text form and DROPS the human label, this preserves the display label — so a
 * host can recover both the precise id (`slug`, e.g. a task id) AND what the user
 * saw (`label`, e.g. the task title).
 */
export interface ChipFieldToken {
	kind: ChipKind;
	slug: string;
	label: string;
}

/**
 * Recognised typed-prefix tokens. The slug grammar (lowercase
 * alphanumeric + `_` / `-`, must start with `[a-z0-9]`) matches the
 * agent-id slug convention used across the runtime, so we don't
 * accidentally chip-render something like `version:1.2.3` or
 * `http://…`. `\b` on both ends prevents prefix-collision against
 * neighbouring words.
 *
 * Re-create the regex per use rather than sharing a singleton — the
 * `g` flag stores `lastIndex` on the regex object, and a shared
 * instance leaks that state across concurrent callers walking
 * different strings on the same tick.
 */
export function chipTokenRegex(): RegExp {
	return /\b(?:(agent|personality|task):([a-z0-9][a-z0-9_-]*)|skill:([a-z0-9][a-z0-9_-]*)(?:\s+via\s+agent:([a-z0-9][a-z0-9_-]*))?)\b/gi;
}

export function normalizeKind(raw: string): ChipKind {
	const lower = raw.toLowerCase();
	if (lower === 'skill' || lower === 'tool') return 'skill';
	if (lower === 'personality') return 'personality';
	if (lower === 'task') return 'task';
	if (lower === 'feature') return 'feature';
	return 'agent';
}

/**
 * The literal command text a `feature` chip serializes to. A feature chip
 * DISPLAYS a single label (e.g. `@tutor_quick`) but SENDS the exact command the
 * backend's inline feature detectors match in the message text (tutor.rs):
 * `#quick` and `#discuss` are separate whitespace-delimited flags, so
 * `tutor_quick` must serialize to `@tutor #quick` and `vibedev_discuss` to
 * `@vibedev #discuss` — the ASCII space is load-bearing. `#` is a token
 * character in the backend tokenizer, so a glued `@tutor#quick` /
 * `@vibedev#discuss` is one token and matches no marker at all. Unknown slugs
 * fall back to `@<slug>`.
 */
const FEATURE_COMMANDS: Record<string, string> = {
	tutor: '@tutor',
	tutor_quick: '@tutor #quick',
	copilot: '@copilot',
	vibedev: '@vibedev',
	vibedev_discuss: '@vibedev #discuss'
};

export function featureCommandForSlug(slug: string): string {
	return FEATURE_COMMANDS[slug] ?? `@${slug}`;
}

export function escapeChipHtml(text: string): string {
	return text
		.replace(/&/g, '&amp;')
		.replace(/</g, '&lt;')
		.replace(/>/g, '&gt;');
}

export function escapeChipAttr(text: string): string {
	return escapeChipHtml(text).replace(/"/g, '&quot;');
}

export function parseChipMatch(match: RegExpExecArray): ChipToken | null {
	const input = match.input ?? '';
	const start = match.index;
	const end = start + match[0].length;
	const preceding = input.slice(Math.max(0, start - 3), start);
	const trailing = input.slice(end);
	// Do not chipify route-like URL segments or a typed prefix that is only
	// the first segment of a dotted identifier. A normal sentence-ending dot
	// remains valid because it is not followed by another word character.
	if (preceding.endsWith('://') || input[start - 1] === '/' || /^\.[a-z0-9_]/i.test(trailing)) {
		return null;
	}
	if (match[1] && match[2]) {
		return {
			kind: normalizeKind(match[1]),
			slug: match[2]
		};
	}
	if (match[3]) {
		return {
			kind: 'skill',
			slug: match[3],
			routeAgent: match[4] || undefined
		};
	}
	return null;
}

const ROUTED_SKILL_SLUG_RE = /^([a-z0-9][a-z0-9_-]*)\s+via\s+agent:([a-z0-9][a-z0-9_-]*)$/i;

export function normalizeChipToken(kind: ChipKind, slug: string, routeAgent?: string): ChipToken {
	const normalizedKind = normalizeKind(kind);
	const trimmedSlug = slug.trim();
	const trimmedRouteAgent = routeAgent?.trim() || undefined;
	if (normalizedKind === 'skill' && !trimmedRouteAgent) {
		const routeMatch = trimmedSlug.match(ROUTED_SKILL_SLUG_RE);
		if (routeMatch) {
			return {
				kind: 'skill',
				slug: routeMatch[1],
				routeAgent: routeMatch[2]
			};
		}
	}
	return {
		kind: normalizedKind,
		slug: trimmedSlug,
		routeAgent: normalizedKind === 'skill' ? trimmedRouteAgent : undefined
	};
}

export function serializeChipToken(kind: ChipKind, slug: string, routeAgent?: string): string {
	const token = normalizeChipToken(kind, slug, routeAgent);
	// `feature` chips serialize to a free-form command (e.g. `@tutor #quick`) — NOT
	// the `kind:slug` grammar — because that is the literal text the backend's
	// inline feature detectors look for in the message.
	if (token.kind === 'feature') return featureCommandForSlug(token.slug);
	if (token.kind === 'skill' && token.routeAgent) {
		return `skill:${token.slug} via agent:${token.routeAgent}`;
	}
	return `${token.kind}:${token.slug}`;
}

export function chipIconSvg(kind: ChipKind): string {
	// 14px monoline icons. `currentColor` lets the chip palette drive
	// the stroke colour, so light/dark themes share the same SVG.
	if (kind === 'agent') {
		return '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="4" y="7" width="16" height="12" rx="2.5"/><circle cx="9" cy="13" r="1.2" fill="currentColor" stroke="none"/><circle cx="15" cy="13" r="1.2" fill="currentColor" stroke="none"/><path d="M12 4v3"/><circle cx="12" cy="3" r="1" fill="currentColor" stroke="none"/></svg>';
	}
	if (kind === 'skill') {
		return '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14.7 6.3a4 4 0 0 1 5 5l-1.6.4-3.8-3.8z"/><path d="M13 8l-7 7a2 2 0 1 0 2.8 2.8l7-7"/><path d="M5 17l2 2"/></svg>';
	}
	if (kind === 'task') {
		return '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 11l2 2 4-4"/><rect x="4" y="4" width="16" height="16" rx="2.5"/></svg>';
	}
	if (kind === 'feature') {
		// Mortarboard / graduation cap — the tutor & copilot assist commands.
		return '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 9l9-4 9 4-9 4-9-4z"/><path d="M7 11.2v3.3c0 1.1 2.2 2 5 2s5-0.9 5-2v-3.3"/><path d="M21 9v4.5"/></svg>';
	}
	return '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l1.7 4.5L18 9l-4.3 1.5L12 15l-1.7-4.5L6 9l4.3-1.5z"/><path d="M19 16l.6 1.6L21 18l-1.4.4L19 20l-.6-1.6L17 18l1.4-.4z"/></svg>';
}

/**
 * Build the chip HTML for a given kind + slug. Used by both the
 * composer (contenteditable insertion) and the transcript chipifier
 * (post-render text-node replacement). `contenteditable="false"`
 * makes the chip an atom inside an editable surface; outside an
 * editable host the attribute is a harmless no-op.
 */
export function buildChipHtml(
	kind: ChipKind,
	slug: string,
	routeAgent?: string,
	/** Human label shown INSIDE the chip when the slug isn't human-readable —
	 *  e.g. a task chip whose `slug` is the task id (`task:<id>`, serialized for
	 *  precision) but which should DISPLAY the task title. Stored as `data-label`
	 *  so it survives DOM moves; serialization still emits `kind:slug`. */
	displayLabel?: string
): string {
	const token = normalizeChipToken(kind, slug, routeAgent);
	const safeSlugText = escapeChipHtml(token.slug);
	const safeSlugAttr = escapeChipAttr(token.slug);
	const safeRouteText = token.routeAgent ? escapeChipHtml(token.routeAgent) : '';
	const safeRouteAttr = token.routeAgent ? escapeChipAttr(token.routeAgent) : '';
	const icon = chipIconSvg(token.kind);
	const routeAttr = safeRouteAttr ? ` data-route-agent="${safeRouteAttr}"` : '';
	const title = escapeChipAttr(serializeChipToken(token.kind, token.slug, token.routeAgent));
	const trimmedLabel = displayLabel?.trim();
	const labelText = trimmedLabel ? escapeChipHtml(trimmedLabel) : safeSlugText;
	const labelAttr = trimmedLabel ? ` data-label="${escapeChipAttr(trimmedLabel)}"` : '';
	const label = safeRouteText
		? `<span class="chip__label-main">${safeSlugText}</span><span class="chip__route">via ${safeRouteText}</span>`
		: labelText;
	return `<span class="chip chip--${token.kind}" contenteditable="false" data-chip="1" data-kind="${token.kind}" data-slug="${safeSlugAttr}"${routeAttr}${labelAttr} title="${title}">${icon}<span class="chip__label">${label}</span></span>`;
}

/**
 * Build the chip as an actual DOM Element (rather than an HTML
 * string). Used by the transcript chipifier where we surgically
 * replace a span inside a text node — `innerHTML += …` would clobber
 * sibling text nodes.
 */
export function buildChipElement(kind: ChipKind, slug: string, routeAgent?: string): HTMLSpanElement {
	const wrapper = document.createElement('span');
	wrapper.innerHTML = buildChipHtml(kind, slug, routeAgent);
	return wrapper.firstElementChild as HTMLSpanElement;
}

/**
 * Render a plain string into HTML, converting any `<kind>:<slug>`
 * prefix tokens into chip spans. Preserves newlines as `<br>`. Used
 * by the composer to hydrate the editor from a value prop.
 */
export function renderValueToHtml(value: string): string {
	if (!value) return '';
	let html = '';
	let last = 0;
	const re = chipTokenRegex();
	let match: RegExpExecArray | null;
	while ((match = re.exec(value)) !== null) {
		const token = parseChipMatch(match);
		const start = match.index;
		const end = start + match[0].length;
		if (start > last) {
			html += escapeChipHtml(value.slice(last, start)).replace(/\n/g, '<br>');
		}
		html += token
			? buildChipHtml(token.kind, token.slug, token.routeAgent)
			: escapeChipHtml(value.slice(start, end));
		last = end;
	}
	if (last < value.length) {
		html += escapeChipHtml(value.slice(last)).replace(/\n/g, '<br>');
	}
	return html;
}

/**
 * Serialize plain text + chip tokens from a contenteditable
 * subtree. Chips emit their typed-prefix form, `<br>` and
 * block-level direct children become newlines.
 *
 * Plain recursion rather than a TreeWalker: the walker variant has
 * to bookkeep its own subtree-skip state when it hits a chip, and
 * gets the boundary wrong when the chip is the last child of a
 * block. Recursive form is half the code and provably correct.
 */
/**
 * True when the editor's last node in document order is a `<br>`.
 *
 * A contenteditable always keeps a trailing "bogus" `<br>` so the last (or
 * only) line stays focusable — it is a rendering artifact the browser manages,
 * not something the user typed. Serializing it made a fully-cleared composer
 * emit `"\n"` instead of `""`, so `showPlaceholder` (`!value.length`) stayed
 * false and the placeholder never came back after typing and deleting.
 *
 * Rendering a genuine trailing newline needs TWO breaks (the real one plus the
 * bogus one), so dropping exactly one is lossless.
 */
function endsWithBogusLineBreak(root: Element): boolean {
	let node: Node | null = root.lastChild;
	while (node) {
		if (node.nodeType === Node.TEXT_NODE) return false;
		if (!(node instanceof HTMLElement)) return false;
		if (node.tagName === 'BR') return true;
		if (node.dataset.chip === '1') return false;
		node = node.lastChild;
	}
	return false;
}

export function serializeChipDom(root: Element): string {
	let out = '';
	const walk = (node: Node, isDirectChild: boolean): void => {
		if (node.nodeType === Node.TEXT_NODE) {
			out += node.textContent ?? '';
			return;
		}
		if (!(node instanceof HTMLElement)) return;
		if (node.dataset.chip === '1') {
			const kind = node.dataset.kind ?? 'agent';
			const slug = node.dataset.slug ?? '';
			const routeAgent = node.dataset.routeAgent || undefined;
			out += serializeChipToken(normalizeKind(kind), slug, routeAgent);
			return;
		}
		if (node.tagName === 'BR') {
			out += '\n';
			return;
		}
		// Block-level direct children act as line separators (Chrome
		// wraps Enter-newlines in <div>). The first one doesn't get a
		// leading newline because it's just the implicit wrapper.
		const isBlock = node.tagName === 'DIV' || node.tagName === 'P';
		if (isBlock && isDirectChild && out.length > 0 && !out.endsWith('\n')) {
			out += '\n';
		}
		for (const child of Array.from(node.childNodes)) walk(child, false);
	};
	for (const child of Array.from(root.childNodes)) walk(child, true);
	if (out.endsWith('\n') && endsWithBogusLineBreak(root)) out = out.slice(0, -1);
	return out;
}
