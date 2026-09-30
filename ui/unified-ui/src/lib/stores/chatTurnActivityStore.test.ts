import { describe, expect, it } from 'vitest';
import {
	captureCompletedTurnActivity,
	claimPendingActivityForMessage,
	getActivityForMessage,
	type ChatTurnActivityRow
} from './chatTurnActivityStore';

function row(key: string, detail: string | null = null): ChatTurnActivityRow {
	return {
		key,
		kind: 'tool',
		label: `Tool ${key}`,
		detail,
		status: 'done',
		tone: 'tool',
		startedAt: 1,
		durationMs: 10
	};
}

describe('chat completed-turn activity ownership', () => {
	it('moves pending session activity onto the arriving assistant message', () => {
		const rows = [row('a'), row('b', 'two')];
		captureCompletedTurnActivity('session-claim', rows);
		claimPendingActivityForMessage('session-claim', 'message-claim');
		expect(getActivityForMessage('message-claim')).toEqual(rows);
	});

	it('does not capture empty activity', () => {
		captureCompletedTurnActivity('session-empty', []);
		claimPendingActivityForMessage('session-empty', 'message-empty');
		expect(getActivityForMessage('message-empty')).toBeNull();
	});

	it('keeps pending activity isolated by session', () => {
		captureCompletedTurnActivity('session-a', [row('a')]);
		captureCompletedTurnActivity('session-b', [row('b')]);
		claimPendingActivityForMessage('session-a', 'message-a');
		expect(getActivityForMessage('message-a')?.map((entry) => entry.key)).toEqual(['a']);
		claimPendingActivityForMessage('session-b', 'message-b');
		expect(getActivityForMessage('message-b')?.map((entry) => entry.key)).toEqual(['b']);
	});

	it('replaces stale pending rows when a newer turn completes in the same session', () => {
		captureCompletedTurnActivity('session-replace', [row('old')]);
		captureCompletedTurnActivity('session-replace', [row('new')]);
		claimPendingActivityForMessage('session-replace', 'message-replace');
		expect(getActivityForMessage('message-replace')?.map((entry) => entry.key)).toEqual(['new']);
	});

	it('consumes pending activity at most once', () => {
		captureCompletedTurnActivity('session-once', [row('once')]);
		claimPendingActivityForMessage('session-once', 'message-first');
		claimPendingActivityForMessage('session-once', 'message-second');
		expect(getActivityForMessage('message-first')?.map((entry) => entry.key)).toEqual(['once']);
		expect(getActivityForMessage('message-second')).toBeNull();
	});
});
