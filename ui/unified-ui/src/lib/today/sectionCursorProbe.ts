import { readerLocalDate } from '$lib/stores/taskStore';
import type { TodaySectionId } from '$lib/today/types';

export interface TodaySectionCursorProbe {
	sectionId: TodaySectionId;
	cursor: string | null;
	sectionPageSize: number;
	digestPageSize: number;
}

/**
 * The URL a page uses to walk a Today section forward one page at a time until
 * it holds the cursor for the page the route asks for.
 *
 * Shared by `/today` and `/square` rather than written out in each: they had
 * byte-identical copies, and a fix that landed in one client's request builder
 * missed both of them.
 *
 * It carries `today=` for the same reason `todayStore.ts::buildQueryString`
 * does, and it must carry the *same* value in the same render. The cursors this
 * mints are consumed by the store's request: a Follow-ups cursor is
 * `{priority}:{updated_at}:{item_id}` and the priority band is derived from the
 * date, so a cursor minted against the server's UTC date and then seeked
 * against the reader's local date lands in a different projection — rows get
 * skipped or repeated, and the totals disagree with what renders. Passed from
 * `readerLocalDate()`, never derived again here.
 */
export function todaySectionCursorProbeUrl(probe: TodaySectionCursorProbe): string {
	const params = new URLSearchParams({
		per_section: String(probe.sectionPageSize),
		section: probe.sectionId,
		limit: String(probe.sectionPageSize),
		digest_limit: String(probe.digestPageSize),
		digest_offset: '0'
	});
	params.set('today', readerLocalDate());
	if (probe.cursor) params.set('cursor', probe.cursor);
	return `/api/magician/v2/today?${params.toString()}`;
}
