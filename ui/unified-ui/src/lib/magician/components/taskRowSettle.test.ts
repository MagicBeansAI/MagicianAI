import { describe, expect, it } from 'vitest';
import { latestTaskStatuses, settleTaskRowFromTranscript } from './taskRowSettle';

describe('settleTaskRowFromTranscript', () => {
	it('settles a running row to failed when the transcript card failed', () => {
		const statuses = new Map([['task_1', 'failed']]);
		expect(settleTaskRowFromTranscript('task_1', 'Researcher running', statuses)).toEqual({
			status: 'failed',
			label: 'Researcher failed'
		});
	});

	it('settles to done for completed / succeeded', () => {
		expect(
			settleTaskRowFromTranscript('t', 'Coder executing', new Map([['t', 'completed']]))
		).toEqual({ status: 'done', label: 'Coder completed' });
		expect(
			settleTaskRowFromTranscript('t', 'Coder', new Map([['t', 'succeeded']]))
		).toEqual({ status: 'done', label: 'Coder — completed' });
	});

	it('treats cancelled as a failed row', () => {
		expect(
			settleTaskRowFromTranscript('t', 'Coder preparing final result', new Map([['t', 'cancelled']]))
		).toEqual({ status: 'failed', label: 'Coder cancelled' });
	});

	it('leaves the row running when the transcript card is in flight or absent', () => {
		expect(settleTaskRowFromTranscript('t', 'Coder running', new Map([['t', 'running']]))).toBeNull();
		expect(settleTaskRowFromTranscript('t', 'Coder running', new Map())).toBeNull();
	});
});

describe('latestTaskStatuses', () => {
	it('keeps the latest status per task', () => {
		const statuses = latestTaskStatuses([
			{ content: { type: 'task_status_update', task_id: 'a', status: 'running' } },
			{ content: { type: 'text' } },
			{ content: { type: 'task_status_update', task_id: 'b', status: 'planning' } },
			{ content: { type: 'task_status_update', task_id: 'a', status: 'failed' } }
		]);
		expect(statuses.get('a')).toBe('failed');
		expect(statuses.get('b')).toBe('planning');
	});
});
