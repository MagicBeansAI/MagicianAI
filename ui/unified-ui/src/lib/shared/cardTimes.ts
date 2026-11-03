// When a chat card's work began and last moved: "Created 14:28 · Updated 2m ago".
//
// The chat task cards (live and classic) and the turn's Steps card show it. The
// start is a clock time (with the date when it is not today, the format chat
// messages use), the update an age that the card's clock keeps current; the
// exact date-times sit in the label's `title`. A time nothing gives is left
// out, never shown as "Invalid Date".

import { formatTimeAgo } from './formatRelativeTime';

/** Epoch ms from epoch ms, epoch seconds (below 1e11) or an ISO string;
 *  undefined for anything that is not a usable time. */
export function toEpochMs(value: number | string | null | undefined): number | undefined {
	const ms = typeof value === 'string' ? (value.trim() ? Date.parse(value) : Number.NaN) : value;
	if (typeof ms !== 'number' || !Number.isFinite(ms) || ms <= 0) return undefined;
	return ms < 1e11 ? ms * 1000 : ms;
}

function sameLocalDay(a: Date, b: Date): boolean {
	return (
		a.getDate() === b.getDate() &&
		a.getMonth() === b.getMonth() &&
		a.getFullYear() === b.getFullYear()
	);
}

/** `14:28` today, else `Oct 31, 14:28` (the viewer's locale); '' when unusable. */
export function formatClockTime(epochMs: number | null | undefined, now = Date.now()): string {
	const ms = toEpochMs(epochMs);
	if (ms === undefined) return '';
	const date = new Date(ms);
	const time = date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	if (sameLocalDay(date, new Date(now))) return time;
	return `${date.toLocaleDateString([], { month: 'short', day: 'numeric' })}, ${time}`;
}

/** The full date and time to the second, for a hover title; '' when unusable. */
export function formatExactTime(epochMs: number | null | undefined): string {
	const ms = toEpochMs(epochMs);
	if (ms === undefined) return '';
	return new Date(ms).toLocaleString([], {
		year: 'numeric',
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});
}

/** Epoch ms; either may be unknown. */
export interface CardTimes {
	createdAt?: number;
	updatedAt?: number;
}

/** The earliest and latest usable time among `values` (as created / updated). */
export function timeSpan(values: Iterable<number | string | null | undefined>): CardTimes {
	let first: number | undefined;
	let last: number | undefined;
	for (const value of values) {
		const ms = toEpochMs(value);
		if (ms === undefined) continue;
		if (first === undefined || ms < first) first = ms;
		if (last === undefined || ms > last) last = ms;
	}
	return { createdAt: first, updatedAt: last };
}

export interface CardTimesLabel {
	/** `Created Oct 31, 14:28 · Updated 2m ago` */
	text: string;
	/** The exact date-time of each shown part, one per line. */
	title: string;
}

/**
 * The card's times line, or null when neither time is known. `createdWord`
 * names the start (`Created` for a task, `Started` for a turn's steps).
 */
export function cardTimesLabel(
	times: CardTimes,
	now = Date.now(),
	createdWord = 'Created'
): CardTimesLabel | null {
	const parts: string[] = [];
	const exact: string[] = [];
	const created = toEpochMs(times.createdAt);
	if (created !== undefined) {
		parts.push(`${createdWord} ${formatClockTime(created, now)}`);
		exact.push(`${createdWord} ${formatExactTime(created)}`);
	}
	const updated = toEpochMs(times.updatedAt);
	if (updated !== undefined) {
		parts.push(`Updated ${formatTimeAgo(updated, now)}`);
		exact.push(`Updated ${formatExactTime(updated)}`);
	}
	return parts.length ? { text: parts.join(' · '), title: exact.join('\n') } : null;
}
