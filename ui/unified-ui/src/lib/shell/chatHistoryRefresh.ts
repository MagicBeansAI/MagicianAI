import { writable } from 'svelte/store';

// History has server-side lanes, search and paging. Invalidate its current
// query rather than inserting rows that may not belong on the visible page.
export const chatHistoryRevision = writable(0);
let refreshTimer: ReturnType<typeof setTimeout> | undefined;

export function refreshChatHistory(): void {
	if (refreshTimer !== undefined) return;
	refreshTimer = setTimeout(() => {
		refreshTimer = undefined;
		chatHistoryRevision.update(value => value + 1);
	}, 250);
}
