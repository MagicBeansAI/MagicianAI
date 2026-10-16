/**
 * Day labels for the chat transcript's date dividers and the floating date
 * pill: "Today", "Yesterday", a weekday within the last week, then a short
 * date (with the year only when it isn't this year). Local time throughout.
 */

const DAY_MS = 24 * 60 * 60 * 1000;

/** Local calendar day of a millisecond timestamp, e.g. "2026-10-01". */
export function dayKey(ms: number): string {
	const d = new Date(ms);
	const month = String(d.getMonth() + 1).padStart(2, '0');
	const day = String(d.getDate()).padStart(2, '0');
	return `${d.getFullYear()}-${month}-${day}`;
}

function startOfDay(ms: number): number {
	const d = new Date(ms);
	return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

export function dayLabel(ms: number, now: number = Date.now()): string {
	const days = Math.round((startOfDay(now) - startOfDay(ms)) / DAY_MS);
	if (days === 0) return 'Today';
	if (days === 1) return 'Yesterday';
	const date = new Date(ms);
	if (days > 1 && days < 7) return date.toLocaleDateString(undefined, { weekday: 'long' });
	const sameYear = date.getFullYear() === new Date(now).getFullYear();
	return date.toLocaleDateString(undefined, {
		weekday: 'short',
		month: 'short',
		day: 'numeric',
		...(sameYear ? {} : { year: 'numeric' })
	});
}

/**
 * For messages in display order, the ids that open a new day, with their
 * label. The first message always opens one.
 */
export function dayStarts<T extends { id: string; createdAt: number }>(
	items: readonly T[],
	now: number = Date.now()
): Map<string, string> {
	const starts = new Map<string, string>();
	let previous: string | null = null;
	for (const item of items) {
		if (!Number.isFinite(item.createdAt) || item.createdAt <= 0) continue;
		const key = dayKey(item.createdAt);
		if (key !== previous) starts.set(item.id, dayLabel(item.createdAt, now));
		previous = key;
	}
	return starts;
}
