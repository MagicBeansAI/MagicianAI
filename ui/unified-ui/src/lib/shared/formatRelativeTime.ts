const SEC = 1_000;
const MIN = 60 * SEC;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
const WEEK = 7 * DAY;

/** Compact relative timestamp with a short absolute fallback after a week. */
export function formatRelativeTime(epochMs: number | null | undefined, now = Date.now()): string {
	if (epochMs == null || !Number.isFinite(epochMs)) return '';
	const delta = Math.max(0, now - epochMs);

	if (delta < MIN) return 'now';
	if (delta < HOUR) return `${Math.floor(delta / MIN)}m`;
	if (delta < DAY) return `${Math.floor(delta / HOUR)}h`;
	if (delta < WEEK) return `${Math.floor(delta / DAY)}d`;

	const date = new Date(epochMs);
	return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}
