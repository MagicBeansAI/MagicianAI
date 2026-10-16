import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import TaskStatusCard from './TaskStatusCard.svelte';
import type { ChatMessage, ChatRenderTaskExecutionGroup } from '$lib/stores/chatStore';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

function taskMessage(executionId?: string, status = 'running'): ChatMessage {
	return {
		id: executionId ? 'run-update' : 'task-card',
		session_id: 'session-1',
		direction: 'assistant',
		created_at: Date.now(),
		content: {
			type: 'task_status_update',
			task_id: 'task-1',
			display_label: 'Review release',
			status,
			execution_id: executionId,
			summary: 'Reviewing the release checks.'
		}
	} as unknown as ChatMessage;
}

describe('TaskStatusCard execution controls', () => {
	it('routes the task-card run affordance to durable task details', async () => {
		const message = taskMessage(undefined, 'completed');
		const runMessage = taskMessage('exec-complete', 'completed');
		const onOpenTask = vi.fn();

		render(TaskStatusCard, {
			message,
			taskExecutionGroups: [
				{
					id: 'exec-complete',
					message: runMessage,
					messageIds: [runMessage.id],
					updates: [runMessage]
				}
			],
			onToggleRun: vi.fn(),
			onOpenTask,
			onInspectFromIds: vi.fn(),
			onWatchLive: vi.fn(),
			onStopWatching: vi.fn()
		});

		await fireEvent.click(screen.getByRole('button', { name: 'Inspect run →' }));
		expect(onOpenTask).toHaveBeenCalledOnce();
		expect(onOpenTask.mock.calls[0][0]).toMatchObject({
			task_id: 'task-1',
			execution_id: 'exec-complete'
		});
	});

	it('renders every authoritative live control for the explicit run', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockImplementation(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.endsWith('/control-state')) {
					return new Response(
						JSON.stringify({
							execution_id: 'exec-chat',
							waiting_state: 'executing',
							active: true,
							can_pause: true,
							can_resume: false,
							can_steer: true,
							can_cancel: true
						}),
						{ status: 200, headers: { 'Content-Type': 'application/json' } }
					);
				}
				return new Response(JSON.stringify({ events: [], total: 0, has_more: false }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			})
		);
		const message = taskMessage();
		const runMessage = taskMessage('exec-chat');
		const groups: ChatRenderTaskExecutionGroup[] = [
			{ id: 'exec-chat', message: runMessage, messageIds: [runMessage.id], updates: [runMessage] }
		];

		render(TaskStatusCard, {
			message,
			taskExecutionGroups: groups,
			onToggleRun: vi.fn(),
			onOpenTask: vi.fn(),
			onInspectFromIds: vi.fn(),
			onWatchLive: vi.fn(),
			onStopWatching: vi.fn()
		});

		expect(await screen.findByRole('button', { name: 'Steer run' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Pause run' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Stop run' })).toBeEnabled();
	});

	it('keeps authoritative Stop available for a manually paused chat run', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockImplementation(async (input: RequestInfo | URL) => {
				if (String(input).endsWith('/control-state')) {
					return new Response(
						JSON.stringify({
							execution_id: 'exec-paused',
							waiting_state: 'paused',
							active: false,
							can_pause: false,
							can_resume: true,
							can_steer: false,
							can_cancel: true
						}),
						{ status: 200, headers: { 'Content-Type': 'application/json' } }
					);
				}
				return new Response(JSON.stringify({ events: [], total: 0, has_more: false }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			})
		);
		const message = taskMessage(undefined, 'paused');
		const runMessage = taskMessage('exec-paused', 'paused');

		render(TaskStatusCard, {
			message,
			taskExecutionGroups: [
				{ id: 'exec-paused', message: runMessage, messageIds: [runMessage.id], updates: [runMessage] }
			],
			onToggleRun: vi.fn(),
			onOpenTask: vi.fn(),
			onInspectFromIds: vi.fn(),
			onWatchLive: vi.fn(),
			onStopWatching: vi.fn()
		});

		expect(await screen.findByRole('button', { name: 'Resume run' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Stop run' })).toBeEnabled();
	});
});
