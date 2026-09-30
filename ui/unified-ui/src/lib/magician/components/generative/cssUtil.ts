/**
 * Sanitize a CSS value string to prevent CSS injection (R58).
 *
 * Strips semicolons (prevents multi-property injection), curly braces
 * (prevents rule injection), `expression()` (legacy IE), and `url()`
 * (prevents external resource loading / data exfiltration).
 *
 * Used by layout components that interpolate user-provided strings
 * into `style` attributes (Container, Stack, Grid, SplitPanel, ScrollArea, Divider).
 */
export function sanitizeCssValue(value: unknown): string {
	if (value == null) return '';
	const raw =
		typeof value === 'string'
			? value
			: (typeof value === 'number' || typeof value === 'bigint')
				? String(value)
				: '';
	return raw
		// R301: Strip CSS comments to prevent obfuscation (e.g., url/**/())
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/[;{}]/g, '')
		// R217: Strip CSS unicode escape sequences (e.g., \75rl( for url()
		.replace(/\\[0-9a-fA-F]{1,6}\s?/g, '')
		.replace(/\\./g, '')
		.replace(/expression\s*\(/gi, '')
		.replace(/url\s*\(/gi, '');
}

const CSS_LENGTH_WITH_UNIT = /^-?(?:\d+|\d*\.\d+)(?:px|rem|em|%|vh|vw|vmin|vmax|ch|ex|cm|mm|in|pt|pc)$/i;
const CSS_NUMBER = /^-?(?:\d+|\d*\.\d+)$/;
const CSS_FUNCTIONAL_LENGTH = /^(?:var|calc|min|max|clamp)\(.+\)$/i;

interface CssLengthOptions {
	allowNegative?: boolean;
}

function hasDisallowedFunctionalNegativeLiteral(token: string): boolean {
	const openIdx = token.indexOf('(');
	const closeIdx = token.lastIndexOf(')');
	if (openIdx < 0 || closeIdx <= openIdx) return false;
	const body = token.slice(openIdx + 1, closeIdx);
	// Reject unary negative numeric literals in functional length expressions
	// for non-negative property contexts (for example `calc(-10px)`).
	const negativeLiteral = /-\s*(?:\d+|\d*\.\d+)/g;
	let match: RegExpExecArray | null;
	while ((match = negativeLiteral.exec(body)) !== null) {
		const minusIdx = match.index;
		let prevIdx = minusIdx - 1;
		while (prevIdx >= 0 && /\s/.test(body[prevIdx])) prevIdx -= 1;
		const prev = prevIdx >= 0 ? body[prevIdx] : '';
		// Unary when at start, or immediately after an operator/list delimiter.
		if (!prev || prev === '(' || prev === ',' || prev === '+' || prev === '-' || prev === '*' || prev === '/') {
			return true;
		}
	}
	return false;
}

function normalizeLengthToken(token: string, options: CssLengthOptions = {}): string | undefined {
	const trimmed = token.trim();
	const allowNegative = options.allowNegative ?? true;
	if (!trimmed) return undefined;
	if (trimmed === '0') return '0';
	if (CSS_NUMBER.test(trimmed)) {
		const numeric = Number(trimmed);
		if (!allowNegative && numeric < 0) return undefined;
		return `${trimmed}px`;
	}
	if (CSS_LENGTH_WITH_UNIT.test(trimmed)) {
		const numeric = Number(trimmed.replace(/[a-z%]+$/i, ''));
		if (!allowNegative && Number.isFinite(numeric) && numeric < 0) return undefined;
		return trimmed;
	}
	if (CSS_FUNCTIONAL_LENGTH.test(trimmed)) {
		if (!allowNegative && hasDisallowedFunctionalNegativeLiteral(trimmed)) return undefined;
		return trimmed;
	}
	return undefined;
}

function tokenizeCssLengthList(value: string): string[] {
	const tokens: string[] = [];
	let current = '';
	let depth = 0;
	for (const ch of value) {
		if (ch === '(') {
			depth += 1;
			current += ch;
			continue;
		}
		if (ch === ')') {
			depth = Math.max(0, depth - 1);
			current += ch;
			continue;
		}
		if (/\s/.test(ch) && depth === 0) {
			const trimmed = current.trim();
			if (trimmed) tokens.push(trimmed);
			current = '';
			continue;
		}
		current += ch;
	}
	const trailing = current.trim();
	if (trailing) tokens.push(trailing);
	return tokens;
}

/**
 * Normalize a CSS length-like value with semantic validation.
 *
 * - numeric values become `px` lengths (`12` -> `12px`)
 * - unitful lengths and var/calc/min/max/clamp functions are accepted
 * - anything else falls back to the caller-provided default
 */
export function sanitizeCssLength(value: unknown, fallback: string): string {
	const cleaned = sanitizeCssValue(value).trim();
	if (!cleaned) return fallback;
	return normalizeLengthToken(cleaned) ?? fallback;
}

/**
 * Normalize a CSS list of 1..N length-like values (for padding/gap shorthands).
 *
 * Accepts the same token grammar as `sanitizeCssLength` per list item.
 * Falls back when token count is outside bounds or any token is invalid.
 */
export function sanitizeCssLengthList(
	value: unknown,
	fallback: string,
	maxParts: number = 4,
	options: CssLengthOptions = {}
): string {
	const cleaned = sanitizeCssValue(value).trim();
	if (!cleaned) return fallback;
	const tokens = tokenizeCssLengthList(cleaned);
	if (tokens.length === 0 || tokens.length > maxParts) return fallback;
	const normalized = tokens.map((token) => normalizeLengthToken(token, options));
	if (normalized.some((t) => t == null)) return fallback;
	return normalized.join(' ');
}

/**
 * Normalize a CSS length-like value for non-negative properties
 * (for example: gap, padding, min-size, max-height, spacing).
 */
export function sanitizeCssNonNegativeLength(value: unknown, fallback: string): string {
	const cleaned = sanitizeCssValue(value).trim();
	if (!cleaned) return fallback;
	return normalizeLengthToken(cleaned, { allowNegative: false }) ?? fallback;
}

/**
 * Normalize a constrained CSS keyword token.
 */
export function sanitizeCssKeyword(
	value: unknown,
	allowed: readonly string[],
	fallback: string
): string {
	const cleaned = sanitizeCssValue(value).trim().toLowerCase();
	if (!cleaned) return fallback;
	return allowed.includes(cleaned) ? cleaned : fallback;
}
