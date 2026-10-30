/**
 * Whether an ask's hint is the material being asked about rather than a one-line
 * aside. A hint on several lines, or longer than one short sentence, is shown as
 * Markdown under "Details" (see `HitlDetails.svelte`); anything else stays the
 * plain line it always was.
 */
export const DETAILED_HINT_MIN_CHARS = 200;

export function isDetailedHint(hint: string | null | undefined): boolean {
	const value = typeof hint === 'string' ? hint.trim() : '';
	return value.includes('\n') || value.length > DETAILED_HINT_MIN_CHARS;
}
