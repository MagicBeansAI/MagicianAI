// Monitor update feedback state (Phase 6, plan §10) — pure vitest over the
// merge/verdict transitions in feedbackState.ts. No network, no Svelte.
import { describe, expect, it } from 'vitest';
import type {
	MonitorFeedbackRecordV1,
	MonitorFeedbackResponseV1,
	MonitorFeedbackVerdict
} from '../types/monitor';
import {
	beginVerdict,
	feedbackStateFromRecords,
	feedbackVerdictLabel,
	isFeedbackInFlight,
	isMaterialUpdate,
	rollbackVerdict,
	settleVerdict,
	verdictOf,
	type FeedbackStateMap
} from './feedbackState';

function record(
	feedbackId: string,
	updateId: string,
	verdict: MonitorFeedbackVerdict,
	recordedAt: string
): MonitorFeedbackRecordV1 {
	return { feedback_id: feedbackId, update_id: updateId, verdict, recorded_at: recordedAt };
}

function response(
	updateId: string,
	verdict: MonitorFeedbackVerdict,
	recorded: boolean,
	feedbackId = 'mf_settled'
): MonitorFeedbackResponseV1 {
	return {
		task_id: 'task_1',
		update_id: updateId,
		verdict,
		recorded,
		feedback_id: feedbackId
	};
}

describe('isMaterialUpdate', () => {
	it('accepts only changed records — baselines and quiet receipts take no verdict', () => {
		expect(isMaterialUpdate({ status: 'changed' })).toBe(true);
		expect(isMaterialUpdate({ status: 'baseline' })).toBe(false);
		expect(isMaterialUpdate({ status: 'unchanged' })).toBe(false);
		expect(isMaterialUpdate({ status: 'degraded' })).toBe(false);
		expect(isMaterialUpdate({ status: 'failed' })).toBe(false);
	});
});

describe('feedbackStateFromRecords', () => {
	it('keys one verdict per update_id', () => {
		const map = feedbackStateFromRecords([
			record('mf_1', 'mu_a', 'useful', '2026-07-22T10:00:00Z'),
			record('mf_2', 'mu_b', 'not_relevant', '2026-07-22T09:00:00Z')
		]);
		expect(verdictOf(map, 'mu_a')).toBe('useful');
		expect(verdictOf(map, 'mu_b')).toBe('not_relevant');
		expect(map['mu_a']).toEqual({ verdict: 'useful', feedbackId: 'mf_1', inFlight: false });
	});

	it('newest recorded_at wins when an update carries multiple records (newest first)', () => {
		const map = feedbackStateFromRecords([
			record('mf_new', 'mu_a', 'not_relevant', '2026-07-22T12:00:00Z'),
			record('mf_old', 'mu_a', 'useful', '2026-07-22T08:00:00Z')
		]);
		expect(map['mu_a']).toEqual({ verdict: 'not_relevant', feedbackId: 'mf_new', inFlight: false });
	});

	it('newest recorded_at wins regardless of item order (oldest first)', () => {
		const map = feedbackStateFromRecords([
			record('mf_old', 'mu_a', 'useful', '2026-07-22T08:00:00Z'),
			record('mf_new', 'mu_a', 'not_relevant', '2026-07-22T12:00:00Z')
		]);
		expect(map['mu_a']).toEqual({ verdict: 'not_relevant', feedbackId: 'mf_new', inFlight: false });
	});

	it('keeps the first-seen record on unparseable or tied timestamps (server order = newest first)', () => {
		const garbled = feedbackStateFromRecords([
			record('mf_first', 'mu_a', 'useful', 'not-a-timestamp'),
			record('mf_second', 'mu_a', 'not_relevant', 'also-bad')
		]);
		expect(garbled['mu_a'].feedbackId).toBe('mf_first');

		const tied = feedbackStateFromRecords([
			record('mf_first', 'mu_a', 'useful', '2026-07-22T10:00:00Z'),
			record('mf_second', 'mu_a', 'not_relevant', '2026-07-22T10:00:00Z')
		]);
		expect(tied['mu_a'].feedbackId).toBe('mf_first');
	});

	it('prefers a parseable timestamp over an unparseable first-seen one', () => {
		const map = feedbackStateFromRecords([
			record('mf_bad', 'mu_a', 'useful', 'garbage'),
			record('mf_good', 'mu_a', 'not_relevant', '2026-07-22T10:00:00Z')
		]);
		expect(map['mu_a'].feedbackId).toBe('mf_good');
	});

	it('returns an empty map for no records', () => {
		expect(feedbackStateFromRecords([])).toEqual({});
	});
});

describe('optimistic begin → settle → rollback', () => {
	it('beginVerdict shows the clicked verdict immediately and flags in-flight', () => {
		const map = beginVerdict({}, 'mu_a', 'useful');
		expect(verdictOf(map, 'mu_a')).toBe('useful');
		expect(isFeedbackInFlight(map, 'mu_a')).toBe(true);
		expect(map['mu_a'].feedbackId).toBeNull();
	});

	it('settleVerdict stores the authoritative verdict + feedback id (recorded:true)', () => {
		const begun = beginVerdict({}, 'mu_a', 'useful');
		const settled = settleVerdict(begun, 'mu_a', response('mu_a', 'useful', true, 'mf_9'));
		expect(settled['mu_a']).toEqual({ verdict: 'useful', feedbackId: 'mf_9', inFlight: false });
	});

	it('settles identically on the idempotent replay (recorded:false, same verdict)', () => {
		const stored = feedbackStateFromRecords([
			record('mf_9', 'mu_a', 'useful', '2026-07-22T10:00:00Z')
		]);
		const begun = beginVerdict(stored, 'mu_a', 'useful');
		const settled = settleVerdict(begun, 'mu_a', response('mu_a', 'useful', false, 'mf_9'));
		expect(settled['mu_a']).toEqual({ verdict: 'useful', feedbackId: 'mf_9', inFlight: false });
	});

	it('an opposite verdict replaces the stored one on settle', () => {
		const stored = feedbackStateFromRecords([
			record('mf_9', 'mu_a', 'useful', '2026-07-22T10:00:00Z')
		]);
		const begun = beginVerdict(stored, 'mu_a', 'not_relevant');
		expect(verdictOf(begun, 'mu_a')).toBe('not_relevant');
		const settled = settleVerdict(
			begun,
			'mu_a',
			response('mu_a', 'not_relevant', true, 'mf_10')
		);
		expect(settled['mu_a']).toEqual({
			verdict: 'not_relevant',
			feedbackId: 'mf_10',
			inFlight: false
		});
	});

	it('rollbackVerdict restores the previous stored verdict on error', () => {
		const stored = feedbackStateFromRecords([
			record('mf_9', 'mu_a', 'useful', '2026-07-22T10:00:00Z')
		]);
		const previous = stored['mu_a'];
		const begun = beginVerdict(stored, 'mu_a', 'not_relevant');
		const rolledBack = rollbackVerdict(begun, 'mu_a', previous);
		expect(rolledBack['mu_a']).toEqual({ verdict: 'useful', feedbackId: 'mf_9', inFlight: false });
	});

	it('rollbackVerdict removes the entry when there was no prior verdict', () => {
		const begun = beginVerdict({}, 'mu_a', 'useful');
		const rolledBack = rollbackVerdict(begun, 'mu_a', undefined);
		expect(verdictOf(rolledBack, 'mu_a')).toBeNull();
		expect('mu_a' in rolledBack).toBe(false);
	});

	it('transitions never mutate the input map', () => {
		const original: FeedbackStateMap = feedbackStateFromRecords([
			record('mf_9', 'mu_a', 'useful', '2026-07-22T10:00:00Z')
		]);
		const snapshot = structuredClone(original);
		beginVerdict(original, 'mu_a', 'not_relevant');
		settleVerdict(original, 'mu_a', response('mu_a', 'not_relevant', true));
		rollbackVerdict(original, 'mu_a', undefined);
		expect(original).toEqual(snapshot);
	});
});

describe('helpers', () => {
	it('verdictOf and isFeedbackInFlight default for unknown updates', () => {
		expect(verdictOf({}, 'mu_missing')).toBeNull();
		expect(isFeedbackInFlight({}, 'mu_missing')).toBe(false);
	});

	it('labels both verdicts', () => {
		expect(feedbackVerdictLabel('useful')).toBe('Useful');
		expect(feedbackVerdictLabel('not_relevant')).toBe('Not relevant');
	});
});
