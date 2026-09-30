import { describe, expect, it } from 'vitest';
import { deriveTaskBackedInspectTarget } from './requestActivityInspection';

describe('request activity inspection target', () => {
	it.each([
		['ordinary chat', 'chat-turn-chat'],
		['Tutor', 'chat-turn-tutor'],
		['Thinking Map', 'chat-turn-brainstorm']
	])('does not expose an inspector for an inline-only %s turn', (_surface, executionId) => {
		expect(
			deriveTaskBackedInspectTarget([
				{
					execution_id: executionId,
					// chat.inline telemetry has no durable task identity.
				}
			])
		).toBeNull();
	});

	it('combines task and execution identities carried by separate events', () => {
		expect(
			deriveTaskBackedInspectTarget([
				{ task_id: 'task-42' },
				{ execution_id: 'execution-7' }
			])
		).toEqual({ taskId: 'task-42', executionId: 'execution-7' });
	});

	it('prefers the newest non-blank identity of each kind', () => {
		expect(
			deriveTaskBackedInspectTarget([
				{ task_id: 'task-old', execution_id: 'execution-old' },
				{ task_id: '  ', execution_id: '' },
				{ task_id: 'task-new' },
				{ execution_id: ' execution-new ' }
			])
		).toEqual({ taskId: 'task-new', executionId: 'execution-new' });
	});

	it('does not expose a task without an execution', () => {
		expect(deriveTaskBackedInspectTarget([{ task_id: 'task-42' }])).toBeNull();
	});
});
