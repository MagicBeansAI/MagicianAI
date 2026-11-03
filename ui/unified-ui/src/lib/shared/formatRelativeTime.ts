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

/**
 * The same label read as an age: `now`, `5m ago`, `3h ago`, `2d ago`, then the
 * short date (a date is not an age, so it keeps no suffix).
 */
export function formatTimeAgo(epochMs: number | null | undefined, now = Date.now()): string {
	const label = formatRelativeTime(epochMs, now);
	if (!label || label === 'now' || now - (epochMs as number) >= WEEK) return label;
	return `${label} ago`;
}
