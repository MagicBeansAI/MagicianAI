/**
 * Monitor update feedback state (Phase 6, plan §10) — pure, component-free.
 *
 * The detail panel keys one verdict per `update_id`: stored records arrive
 * from `GET /monitors/{task_id}/feedback` (merged newest-wins here), and a
 * click flows optimistic → settled (from the POST response) → or rolled
 * back on error. All transitions are pure map-in/map-out functions so the
 * merge/verdict logic is unit-testable without mounting the component.
 *
 * Contract notes mirrored from the wire (`$lib/types/monitor.ts`):
 * `recorded:false` on the POST response is the idempotent replay of the
 * SAME verdict — the response still carries the authoritative verdict and
 * `feedback_id`, so settling is identical either way. Posting the OPPOSITE
 * verdict replaces the stored one (`recorded:true`).
 */

import type {
	MonitorFeedbackRecordV1,
	MonitorFeedbackResponseV1,
	MonitorFeedbackVerdict,
	MonitorUpdateDetailV1
} from '$lib/types/monitor';

/** Per-update verdict state (stored or optimistic). */
export interface UpdateFeedbackState {
	verdict: MonitorFeedbackVerdict;
	/** Null while an optimistic submit is in flight (server owns the id). */
	feedbackId: string | null;
	inFlight: boolean;
}

/** Keyed by `update_id`. Treated immutably — every transition copies. */
export type FeedbackStateMap = Record<string, UpdateFeedbackState>;

/**
 * §10: feedback rides MATERIAL updates — a `changed` ledger record.
 * Baselines and quiet `every_run` receipts don't take verdicts.
 */
export function isMaterialUpdate(update: Pick<MonitorUpdateDetailV1, 'status'>): boolean {
	return update.status === 'changed';
}

/**
 * Merge stored records into per-update state: ONE verdict per `update_id`,
 * the newest `recorded_at` wins regardless of item order. On unparseable or
 * tied timestamps the FIRST-SEEN record wins (the server returns newest
 * first, so first-seen is the newest under the server's own ordering).
 */
export function feedbackStateFromRecords(records: MonitorFeedbackRecordV1[]): FeedbackStateMap {
	const newest = new Map<string, { record: MonitorFeedbackRecordV1; at: number | null }>();
	for (const record of records) {
		const parsed = Date.parse(record.recorded_at);
		const at = Number.isFinite(parsed) ? parsed : null;
		const existing = newest.get(record.update_id);
		if (!existing) {
			newest.set(record.update_id, { record, at });
			continue;
		}
		const laterTimestamp = at !== null && existing.at !== null && at > existing.at;
		const onlyThisParses = at !== null && existing.at === null;
		if (laterTimestamp || onlyThisParses) {
			newest.set(record.update_id, { record, at });
		}
	}
	const map: FeedbackStateMap = {};
	for (const [updateId, { record }] of newest) {
		map[updateId] = { verdict: record.verdict, feedbackId: record.feedback_id, inFlight: false };
	}
	return map;
}

/** Optimistic transition: the clicked verdict shows immediately. */
export function beginVerdict(
	map: FeedbackStateMap,
	updateId: string,
	verdict: MonitorFeedbackVerdict
): FeedbackStateMap {
	return { ...map, [updateId]: { verdict, feedbackId: null, inFlight: true } };
}

/**
 * Settle from the POST response — authoritative verdict + feedback id.
 * Identical for `recorded:true` and the idempotent-replay `recorded:false`
 * (both carry the stored verdict and id).
 */
export function settleVerdict(
	map: FeedbackStateMap,
	updateId: string,
	response: MonitorFeedbackResponseV1
): FeedbackStateMap {
	return {
		...map,
		[updateId]: { verdict: response.verdict, feedbackId: response.feedback_id, inFlight: false }
	};
}

/** Roll the optimistic verdict back to what was there before (or nothing). */
export function rollbackVerdict(
	map: FeedbackStateMap,
	updateId: string,
	previous: UpdateFeedbackState | undefined
): FeedbackStateMap {
	const next = { ...map };
	if (previous) {
		next[updateId] = { ...previous, inFlight: false };
	} else {
		delete next[updateId];
	}
	return next;
}

export function verdictOf(map: FeedbackStateMap, updateId: string): MonitorFeedbackVerdict | null {
	return map[updateId]?.verdict ?? null;
}

export function isFeedbackInFlight(map: FeedbackStateMap, updateId: string): boolean {
	return map[updateId]?.inFlight === true;
}

export function feedbackVerdictLabel(verdict: MonitorFeedbackVerdict): string {
	return verdict === 'useful' ? 'Useful' : 'Not relevant';
}
