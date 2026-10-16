/**
 * Pure helpers for the Today "Operations carousel" (Economics / State of
 * Operations). Kept free of Svelte so the bucketing, ordering, relative-time
 * and auto-advance rules are unit-testable and match the mobile clients.
 */
import type { Task, TaskStatus } from '$lib/stores/taskStore';

/** Auto-advance cadence and the pause after any manual navigation. */
export const OPS_CAROUSEL_INTERVAL_MS = 8_000;
export const OPS_CAROUSEL_MANUAL_PAUSE_MS = 15_000;
/** Most recently updated tasks shown on the State of Operations slide. */
export const OPS_RECENT_TASK_LIMIT = 20;

export interface OpsCarouselSlide {
	id: string;
	title: string;
}

export interface TaskBuckets {
	active: number;
	succeeded: number;
	failed: number;
	total: number;
}

const ACTIVE_STATUSES: ReadonlySet<TaskStatus> = new Set<TaskStatus>(['running', 'paused', 'planning']);

/**
 * Same buckets as the Today pie: Active = running | paused | planning,
 * Succeeded = completed (never below the pulse's completed-today count),
 * Failed = failed. Other statuses (pending, ready, deferred, …) are not counted.
 */
export function bucketTasks(tasks: readonly Pick<Task, 'status'>[], pulseCompletedToday = 0): TaskBuckets {
	let active = 0;
	let succeeded = 0;
	let failed = 0;
	for (const task of tasks) {
		if (ACTIVE_STATUSES.has(task.status)) active += 1;
		else if (task.status === 'completed') succeeded += 1;
		else if (task.status === 'failed') failed += 1;
	}
	succeeded = Math.max(succeeded, pulseCompletedToday);
	return { active, succeeded, failed, total: active + succeeded + failed };
}

function timestampMs(value: string | undefined): number {
	if (!value) return Number.NEGATIVE_INFINITY;
	const ms = Date.parse(value);
	return Number.isFinite(ms) ? ms : Number.NEGATIVE_INFINITY;
}

/** Most recently updated first (falls back to createdAt), capped at `limit`. */
export function recentTasks<T extends Pick<Task, 'id' | 'updatedAt' | 'createdAt'>>(
	tasks: readonly T[],
	limit = OPS_RECENT_TASK_LIMIT
): T[] {
	return tasks
		.map((task, index) => ({
			task,
			index,
			at: Math.max(timestampMs(task.updatedAt), timestampMs(task.createdAt))
		}))
		.sort((a, b) => (b.at === a.at ? a.index - b.index : b.at - a.at))
		.slice(0, Math.max(0, limit))
		.map((entry) => entry.task);
}

/** Short relative age: "now", "4m", "2h", "1d". Empty for an unparseable time. */
export function shortRelativeTime(iso: string | undefined, nowMs: number): string {
	const at = timestampMs(iso);
	if (!Number.isFinite(at)) return '';
	const seconds = Math.max(0, Math.floor((nowMs - at) / 1000));
	if (seconds < 60) return 'now';
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h`;
	return `${Math.floor(hours / 24)}d`;
}

export type TaskDotTone = 'active' | 'success' | 'danger' | 'muted';

export function taskDotTone(status: TaskStatus): TaskDotTone {
	if (ACTIVE_STATUSES.has(status)) return 'active';
	if (status === 'completed') return 'success';
	if (status === 'failed') return 'danger';
	return 'muted';
}

/** Wrapping step through `count` slides (`delta` may be negative). */
export function stepSlideIndex(current: number, count: number, delta: number): number {
	if (count <= 0) return 0;
	return (((current + delta) % count) + count) % count;
}

/**
 * Auto-advance is held while the user interacts (hover, focus inside, drag)
 * and until `pausedUntilMs` after a manual navigation. Returns how long to
 * wait before the next check; 0 means "advance now".
 */
export function autoAdvanceDelay(options: {
	nowMs: number;
	pausedUntilMs: number;
	interacting: boolean;
	intervalMs?: number;
}): number {
	const interval = options.intervalMs ?? OPS_CAROUSEL_INTERVAL_MS;
	if (options.interacting) return interval;
	if (options.nowMs < options.pausedUntilMs) return options.pausedUntilMs - options.nowMs;
	return 0;
}

/** Accessible label for a slide / its dot: "Slide 2 of 2, State of Operations". */
export function slideLabel(index: number, count: number, title: string): string {
	return `Slide ${index + 1} of ${count}, ${title}`;
}

/** Average model cost per call today, or null before the first call. */
export function avgCostPerCall(spendToday: number, callsToday: number): number | null {
	return callsToday > 0 ? spendToday / callsToday : null;
}

/** `$0.0014`-style per-call figure: four decimals under a cent, else two. */
export function formatPerCall(value: number | null): string {
	if (value === null) return '—';
	return value < 0.01 ? `$${value.toFixed(4)}` : `$${value.toFixed(2)}`;
}

/** The hour with the most spend as `3p`, or null when nothing was spent. */
export function peakSpendHour(hourlySpend: readonly number[]): string | null {
	let best = -1;
	let bestValue = 0;
	hourlySpend.forEach((value, hour) => {
		if (value > bestValue) {
			bestValue = value;
			best = hour;
		}
	});
	if (best < 0) return null;
	return best === 0 ? '12a' : best === 12 ? '12p' : best > 12 ? `${best - 12}p` : `${best}a`;
}
