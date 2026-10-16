/**
 * The dots the desktop notch ear paints, derived from the work summary
 * (`workLiveStore`): one per coding run, crew (a crew is one dot, its lanes
 * stay inside it) or Work task.
 *
 * The host gets only what it draws: the item id, a state, a title for the
 * tooltip and the item's web page. Items waiting on the owner come first,
 * then working, then ended, as the server ranks them; the ear shows at most
 * `NOTCH_RUN_DOT_LIMIT` and the host counts the rest. An item the owner
 * stopped is not at work and gets no dot; a done item stays green for
 * `NOTCH_DONE_DOT_MS`, the latest `NOTCH_DONE_DOT_LIMIT` of them; a failed
 * one stays as long as the server lists it. The work list tapping a dot opens
 * still lists them all.
 */
import type { WorkItem, WorkItemState } from '$lib/stores/workLiveStore';
import { stripRunTitlePrefix } from '$lib/shell/vibe/conversation/submit';

export const NOTCH_RUN_DOT_LIMIT = 12;
/** How long a done item's green dot stays on the ear after it ends. */
export const NOTCH_DONE_DOT_MS = 2 * 60 * 1000;
/** Most green dots at once: the latest done items. */
export const NOTCH_DONE_DOT_LIMIT = 4;

export interface NotchRunDot {
	/** The work item id: `run:…`, `crew:…` or `task:…`. */
	id: string;
	state: WorkItemState;
	title: string;
	/** The item's page in the main window (`web_path`). */
	path: string | null;
}

/** When an item last changed; for a done item, about when it ended. */
function changedAtMs(item: WorkItem): number {
	return Date.parse(item.updatedAt);
}

export function notchRunDots(items: readonly WorkItem[], nowMs: number = Date.now()): NotchRunDot[] {
	const recentDone = new Set(
		items
			.filter((item) => item.state === 'done' && nowMs - changedAtMs(item) <= NOTCH_DONE_DOT_MS)
			.sort((a, b) => changedAtMs(b) - changedAtMs(a))
			.slice(0, NOTCH_DONE_DOT_LIMIT)
			.map((item) => item.id)
	);
	return items
		.filter((item) => item.state !== 'stopped')
		.filter((item) => item.state !== 'done' || recentDone.has(item.id))
		.slice(0, NOTCH_RUN_DOT_LIMIT)
		.map((item) => ({
			id: item.id,
			state: item.state,
			// The ear's label pill is narrow: the item's own words first, the
			// project after, without the `VibeDev ·` title prefix.
			title: [stripRunTitlePrefix(item.title), item.context].filter(Boolean).join(' · '),
			path: item.webPath || null
		}));
}
