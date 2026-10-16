import { describe, it, expect } from 'vitest';
import { infoEventToCard } from './infoModel';

describe('infoEventToCard — errors (auto-dismiss 15s)', () => {
	it('maps ExecutionFailed to a transient error card (15s)', () => {
		const card = infoEventToCard({
			event_type: 'ExecutionFailed',
			data: { execution_id: 'e1', error: 'boom', step_id: 's2' }
		});
		expect(card).toEqual({
			id: 'exec-fail-e1',
			kind: 'error',
			title: 'Execution failed',
			message: 'boom',
			deepLink: 'e1',
			dismissAfterMs: 15000
		});
	});

	it('maps ProcessingError using error_message', () => {
		const card = infoEventToCard({
			event_type: 'ProcessingError',
			data: { execution_id: 'e9', error_message: 'parse failure', error_type: 'Parse' }
		});
		expect(card).toEqual({
			id: 'exec-fail-e9',
			kind: 'error',
			title: 'Processing error',
			message: 'parse failure',
			deepLink: 'e9',
			dismissAfterMs: 15000
		});
	});

	it('maps AgenticStepFailed to an error card with no message', () => {
		const card = infoEventToCard({
			event_type: 'AgenticStepFailed',
			data: { execution_id: 'e3', plan_id: 'p', step_id: 's', iteration: 4 }
		});
		expect(card).toEqual({
			id: 'exec-fail-e3',
			kind: 'error',
			title: 'Step failed',
			deepLink: 'e3',
			dismissAfterMs: 15000
		});
	});

	it('maps AgenticMaxIterationsReached to an error card', () => {
		const card = infoEventToCard({
			event_type: 'AgenticMaxIterationsReached',
			data: { execution_id: 'e4', plan_id: 'p', step_id: 's', iterations_used: 25 }
		});
		expect(card).toEqual({
			id: 'exec-fail-e4',
			kind: 'error',
			title: 'Max iterations reached',
			deepLink: 'e4',
			dismissAfterMs: 15000
		});
	});

	it('returns null when an error event has no execution_id', () => {
		expect(infoEventToCard({ event_type: 'ExecutionFailed', data: { error: 'no id' } })).toBeNull();
		expect(infoEventToCard({ event_type: 'AgenticStepFailed', data: {} })).toBeNull();
	});
});

describe('infoEventToCard — completions (auto-dismiss 5s)', () => {
	it('maps a successful ExecutionCompleted to a success card', () => {
		const card = infoEventToCard({
			event_type: 'ExecutionCompleted',
			data: { execution_id: 'e5', success: true, steps_total: 3 }
		});
		expect(card).toEqual({
			id: 'exec-done-e5',
			kind: 'success',
			title: 'Completed',
			deepLink: 'e5',
			dismissAfterMs: 5000
		});
	});

	it('returns null for a failed ExecutionCompleted (ExecutionFailed covers it)', () => {
		expect(
			infoEventToCard({
				event_type: 'ExecutionCompleted',
				data: { execution_id: 'e6', success: false, steps_total: 3 }
			})
		).toBeNull();
	});

	it('returns null when success is missing (not strictly true)', () => {
		expect(
			infoEventToCard({ event_type: 'ExecutionCompleted', data: { execution_id: 'e7' } })
		).toBeNull();
	});
});

describe('infoEventToCard — non-informational / unknown', () => {
	it('returns null for HitlRequested / HitlResolved (handled by hitlEventToCard)', () => {
		expect(
			infoEventToCard({ event_type: 'HitlRequested', data: { correlation_id: 'c1' } })
		).toBeNull();
		expect(
			infoEventToCard({ event_type: 'HitlResolved', data: { correlation_id: 'c1' } })
		).toBeNull();
	});

	it('returns null for a ChatMessageReceived (no longer surfaced as a card)', () => {
		// ChatMessageReceived fires for ALL messages (the user's own UI messages,
		// assistant replies, system/status updates) — pure notification noise — so
		// the mapping was cut. The actionable guest-request card covers the
		// valuable "someone is reaching you" case instead.
		expect(
			infoEventToCard({
				event_type: 'ChatMessageReceived',
				data: { session_id: 's', message: { id: 'm1', direction: 'user', content: 'hello' } }
			})
		).toBeNull();
	});

	it('returns null for an unknown event_type', () => {
		expect(infoEventToCard({ event_type: 'SomethingElse', data: {} })).toBeNull();
	});
});
