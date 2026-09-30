
/** One line inside a note that matched, 1-based so it can be quoted as-is. */
export interface NoteSearchMatch {
	line: number;
	text: string;
}

export interface NoteSearchHit {
	provider: string;
	relative_path: string;
	source_ref: string;
	title: string;
	modified_at_ms: number;
	open_url?: string | null;
	/** A title or path hit, which ranks above a body-only hit. */
	matched_in_title: boolean;
	/** Total matching lines, which may exceed the `matches` returned. */
	match_count: number;
	matches: NoteSearchMatch[];
}

export interface NoteSearchResults {
	hits: NoteSearchHit[];
	scanned_notes: number;
	/**
	 * The scan stopped before the space ended. A missing note may still exist,
	 * so a caller must not report "nothing there" on a truncated scan.
	 */
	scan_truncated: boolean;
	query_terms: string[];
	/** More notes matched than `limit` allowed back. */
	more_available: boolean;
}

export class NoteSearchError extends Error {}

/**
 * Search the owner's notes for every term in `query`.
 *
 * Terms are matched fuzzily, so a typo can still find the note. When the
 * embedding model is available, notes that mean the same thing are included
 * even when they do not share those words.
 */
export async function searchNotes(
	query: string,
	options: { limit?: number; provider?: string; signal?: AbortSignal } = {}
): Promise<NoteSearchResults> {
	let response: Response;
	try {
		response = await fetch('/api/magician/v2/notes/search', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				query,
				limit: options.limit,
				provider: options.provider
			}),
			signal: options.signal
		});
	} catch (error) {
		// An aborted search is a newer search superseding this one, not a
		// failure worth showing the owner.
		if (error instanceof DOMException && error.name === 'AbortError') throw error;
		throw new NoteSearchError('Notes search could not reach the server');
	}
	if (!response.ok) {
		const detail = await response.text().catch(() => '');
		throw new NoteSearchError(
			detail.trim() || `Notes search failed with status ${response.status}`
		);
	}
	return (await response.json()) as NoteSearchResults;
}

/**
 * Split a matched line around the query terms so the UI can mark them.
 *
 * Returns alternating plain/marked segments rather than HTML: building a string
 * of markup here would mean escaping note content by hand, and a note is exactly
 * the place where someone's angle brackets are their own text.
 */
export function highlightSegments(
	text: string,
	terms: string[]
): Array<{ text: string; match: boolean }> {
	const usable = terms.filter((term) => term.length > 0);
	if (usable.length === 0) return [{ text, match: false }];

	const lowered = text.toLowerCase();
	// Mark every occurrence of every term, then merge, so overlapping terms
	// ("note" and "notes") produce one span instead of nested ones.
	const covered = new Array<boolean>(text.length).fill(false);
	for (const term of usable) {
		const needle = term.toLowerCase();
		let from = lowered.indexOf(needle);
		while (from !== -1) {
			for (let index = from; index < from + needle.length; index += 1) covered[index] = true;
			from = lowered.indexOf(needle, from + needle.length);
		}
	}

	const segments: Array<{ text: string; match: boolean }> = [];
	let start = 0;
	for (let index = 1; index <= text.length; index += 1) {
		if (index === text.length || covered[index] !== covered[start]) {
			segments.push({ text: text.slice(start, index), match: covered[start] });
			start = index;
		}
	}
	return segments;
}
