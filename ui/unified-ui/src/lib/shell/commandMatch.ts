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
export function commandFilter(value: string, search: string): number {
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
