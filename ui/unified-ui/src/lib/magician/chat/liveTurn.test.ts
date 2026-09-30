import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '$lib/stores/chatStore';
import { latestActiveTaskTurnId } from './liveTurn';

function taskStatus(
	id: string,
	taskId: string,
	status: string,
	createdAt: number,
	chatTurnId?: string
): ChatMessage {
	return {
		id,
		session_id: 'session-1',
		direction: 'system',
		content: {
			type: 'task_status_update',
			task_id: taskId,
			status
		},
		created_at: createdAt,
		chat_turn_id: chatTurnId
	};
}

describe('latestActiveTaskTurnId', () => {
	it('ignores uncorrelated historical tasks instead of pinning the newest chat turn', () => {
		const messages = [
			taskStatus('old-planning', 'task-old', 'planning', 10),
			taskStatus('old-paused', 'task-paused', 'paused', 20)
		];

		expect(latestActiveTaskTurnId(messages)).toBeNull();
	});

	it('uses the latest status per task so an old running row cannot survive completion', () => {
		const messages = [
			taskStatus('running', 'task-1', 'running', 10, 'turn-1'),
			taskStatus('completed', 'task-1', 'completed', 20, 'turn-1')
		];

		expect(latestActiveTaskTurnId(messages)).toBeNull();
	});

	it('keeps the actual spawning turn live while its correlated task is active', () => {
		const messages = [
			taskStatus('old', 'task-old', 'planning', 10),
			taskStatus('active-1', 'task-1', 'running', 30, 'turn-1'),
			taskStatus('active-2', 'task-2', 'paused', 40, 'turn-2')
		];

		expect(latestActiveTaskTurnId(messages)).toBe('turn-2');
	});

	it('retains task-local turn provenance when a later running update omits correlation', () => {
		const messages = [
			taskStatus('correlated-start', 'task-1', 'running', 10, 'turn-1'),
			taskStatus('uncorrelated-progress', 'task-1', 'running', 20)
		];

		expect(latestActiveTaskTurnId(messages)).toBe('turn-1');
	});

	it('lets an uncorrelated terminal update settle a previously correlated task', () => {
		const messages = [
			taskStatus('correlated-start', 'task-1', 'running', 10, 'turn-1'),
			taskStatus('uncorrelated-terminal', 'task-1', 'completed', 20)
		];

		expect(latestActiveTaskTurnId(messages)).toBeNull();
	});

	it('accepts the chat turn side-map resolver used for optimistic messages', () => {
		const message = taskStatus('active', 'task-1', 'running', 10);

		expect(
			latestActiveTaskTurnId([message], (candidate) =>
				candidate.id === 'active' ? 'turn-from-side-map' : null
			)
		).toBe('turn-from-side-map');
	});
});
