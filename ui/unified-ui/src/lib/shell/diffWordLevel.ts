/**
 * Lazy jsdiff loader for word-level diff highlighting inside del/add
 * modification pairs.
 *
 * `diff` ships ~50KB minified / ~15KB gzipped. Loaded only when a
 * consumer sets `wordLevelDiff={true}` on a DiffStrip. Singleton
 * Promise so the module is fetched exactly once per process.
 */
export interface WordDiffSegment {
	value: string;
	added: boolean;
	removed: boolean;
}

let diffPromise: Promise<typeof import('diff')> | null = null;

export function loadDiff(): Promise<typeof import('diff')> {
	if (diffPromise) return diffPromise;
	diffPromise = import('diff');
	return diffPromise;
}

/**
 * Compute word-level segments between two lines. Uses
 * `diffWordsWithSpace` so the rendering preserves whitespace boundaries
 * (otherwise consecutive multi-word changes would smush together).
 *
 * Returns null when the diff module hasn't loaded yet — caller renders
 * the raw text and re-renders when the module arrives.
 *
 * Both sides are returned in the same call so the caller can correlate
 * removed segments (rendered on the old line) with added segments
 * (rendered on the new line).
 */
export function computeWordDiff(
	mod: typeof import('diff') | null,
	oldLine: string,
	newLine: string
): { old: WordDiffSegment[]; new: WordDiffSegment[] } | null {
	if (!mod) return null;
	const segments = mod.diffWordsWithSpace(oldLine, newLine);
	const oldSegs: WordDiffSegment[] = [];
	const newSegs: WordDiffSegment[] = [];
	for (const seg of segments) {
		if (seg.added) {
			newSegs.push({ value: seg.value, added: true, removed: false });
		} else if (seg.removed) {
			oldSegs.push({ value: seg.value, added: false, removed: true });
		} else {
			// Unchanged segment — appears on both sides.
			oldSegs.push({ value: seg.value, added: false, removed: false });
			newSegs.push({ value: seg.value, added: false, removed: false });
		}
	}
	return { old: oldSegs, new: newSegs };
}
