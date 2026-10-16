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

	it('prefers the newest non-blank identity of each kind when no event pairs them', () => {
		expect(
			deriveTaskBackedInspectTarget([
				{ task_id: 'task-old' },
				{ execution_id: 'execution-old' },
				{ task_id: '  ', execution_id: '' },
				{ task_id: 'task-new' },
				{ execution_id: ' execution-new ' }
			])
		).toEqual({ taskId: 'task-new', executionId: 'execution-new' });
	});

	it('prefers the newest event that pairs both identities', () => {
		expect(
			deriveTaskBackedInspectTarget([
				{ task_id: 'task-old', execution_id: 'execution-old' },
				{ task_id: ' task-new ', execution_id: ' execution-new ' },
				{ task_id: 'task-other' },
				{ execution_id: 'execution-other' }
			])
		).toEqual({ taskId: 'task-new', executionId: 'execution-new' });
	});

	it('keeps a delegated run paired when the mouth answers inline afterwards', () => {
		expect(
			deriveTaskBackedInspectTarget([
				{ task_id: 'task-42', execution_id: 'execution-7' },
				{ task_id: 'task-42' },
				{ execution_id: 'chat-turn-abc' },
				{ task_id: 'task-42', execution_id: '' }
			])
		).toEqual({ taskId: 'task-42', executionId: 'execution-7' });
	});

	it('does not expose a task without an execution', () => {
		expect(deriveTaskBackedInspectTarget([{ task_id: 'task-42' }])).toBeNull();
	});
});
