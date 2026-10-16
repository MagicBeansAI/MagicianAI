/** Case-fold a palette query and a command's searchable text the same way. */
export function normalizeCommandText(value: string): string {
	return value.normalize('NFKC').trim().toLocaleLowerCase('en-US').replace(/\s+/g, ' ');
}

/**
 * Prefer literal words over subsequence matches.
 *
 * bits-ui's default fuzzy scorer considers "settings" close to phrases such
 * as "recent sessions". A command matches when the full query is present, or
 * every query word is an exact or prefix word. Matching ignores capitalization.
 */
export function commandFilter(value: string, search: string, keywords: string[] = []): number {
	const score = matchScore(value, search);
	return score > 0 ? score + sectionBoost(keywords) : 0;
}

/**
 * Sections that lead search results. bits-ui orders groups by their best
 * item's score, so a match in "Jump to" outranks every other section, then
 * "Manage"; the rest keep match-quality order. A palette item opts in with
 * the keyword `section:<name>` (see `sectionKeyword`).
 */
const SECTION_BOOST: Record<string, number> = { 'Jump to': 2, Manage: 1 };

export function sectionKeyword(section: string): string {
	return `section:${section}`;
}

function sectionBoost(keywords: string[]): number {
	let boost = 0;
	for (const keyword of keywords) {
		if (keyword.startsWith('section:')) {
			boost = Math.max(boost, SECTION_BOOST[keyword.slice('section:'.length)] ?? 0);
		}
	}
	return boost;
}

function matchScore(value: string, search: string): number {
	const haystack = normalizeCommandText(value);
	const needle = normalizeCommandText(search);
	if (!needle) return 1;
	if (haystack.startsWith(needle)) return 1;
	if (haystack.includes(needle)) return 0.9;

	const words = haystack.split(' ');
	const terms = needle.split(' ');
	let score = 0;
	for (const term of terms) {
		if (words.includes(term)) {
			score += 1;
		} else if (words.some((word) => word.startsWith(term))) {
			score += 0.8;
		} else {
			return 0;
		}
	}
	return 0.6 + 0.3 * (score / terms.length);
}
