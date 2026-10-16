import { get } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/environment', () => ({ browser: false, dev: false, building: false, version: 'test' }));

import {
	ingestPendingHitlLineForTest,
	pendingHitlByChatTurnId,
	pendingHitlCount,
	pendingHitlEntries,
	teardownPendingHitlBridge
} from './pendingHitlStore';
import { hitlOpenTargetFromPendingEntry } from '$lib/attention/openHitlPrompt';

function requested(data: Record<string, unknown>): string {
	return JSON.stringify({ event_type: 'HitlRequested', data });
}

const answerable = {
	correlation_id: 'question-1',
	source: 'clarification',
	input_type: 'text',
	prompt: 'Which account?',
	principal: 'owner',
	workspace: 'default',
	task_id: 'task-1',
	execution_id: 'planexec-1',
	chat_turn_id: 'turn-1'
};

afterEach(() => {
	teardownPendingHitlBridge();
});

describe('pendingHitlStore counts only asks a surface can open', () => {
	it('counts a parseable request and keeps a raw that reopens it', () => {
		ingestPendingHitlLineForTest(requested(answerable));
		expect(get(pendingHitlCount)).toBe(1);
		const [entry] = get(pendingHitlEntries);
		expect(hitlOpenTargetFromPendingEntry(entry)).not.toBeNull();
		expect(get(pendingHitlByChatTurnId).has('turn-1')).toBe(true);
	});

	it('skips a request the canonical parser rejects', () => {
		// Unknown source.
		ingestPendingHitlLineForTest(
			requested({ ...answerable, correlation_id: 'q-unknown', source: 'mystery' })
		);
		// A choice with no options to choose from.
		ingestPendingHitlLineForTest(
			requested({
				...answerable,
				correlation_id: 'q-choice',
				input_type: 'choice',
				input_schema: { options: [] }
			})
		);
		// No scope to answer it in.
		ingestPendingHitlLineForTest(
			requested({ ...answerable, correlation_id: 'q-scope', principal: undefined, workspace: undefined })
		);
		expect(get(pendingHitlCount)).toBe(0);
		expect(get(pendingHitlByChatTurnId).size).toBe(0);
	});

	it('accepts the backfill shape and stores it in the form the parser reads', () => {
		ingestPendingHitlLineForTest(
			JSON.stringify({ event_id: 'evt-1', event_type: 'HitlRequested', payload: answerable })
		);
		expect(get(pendingHitlCount)).toBe(1);
		const [entry] = get(pendingHitlEntries);
		expect(hitlOpenTargetFromPendingEntry(entry)).not.toBeNull();
	});

	it('drops a counted request on its resolution', () => {
		ingestPendingHitlLineForTest(requested(answerable));
		ingestPendingHitlLineForTest(
			JSON.stringify({ event_type: 'HitlResolved', data: { correlation_id: 'question-1' } })
		);
		expect(get(pendingHitlCount)).toBe(0);
	});
});
