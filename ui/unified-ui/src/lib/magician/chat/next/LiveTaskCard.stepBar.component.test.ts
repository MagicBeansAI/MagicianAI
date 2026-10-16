import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ChatMessage } from '$lib/stores/chatStore';

const readTaskRunState = vi.hoisted(() => vi.fn());
vi.mock('$lib/magician/tasks/taskAttention', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/magician/tasks/taskAttention')>()),
	readTaskRunState
}));

import LiveTaskCard from './LiveTaskCard.svelte';
import { resetPlanStepPolls } from './planStepPoll';
import { resetTaskUsage } from './taskUsage';

function message(status: string): ChatMessage {
	return {
		id: `m-${status}`,
		session_id: 'session-1',
		direction: 'system',
		created_at: 1_760_000_000_000,
		content: {
			type: 'task_status_update',
			task_id: 'task-1',
			display_label: 'Review release',
			status,
			execution_id: 'exec-1'
		}
	} as unknown as ChatMessage;
}

function panel(statuses: Array<{ name: string; status: string }>) {
	return {
		ok: true,
		state: {
			overview: { status: 'running' },
			debug: {
				selected_execution: {
					step_statuses: statuses.map((step, index) => ({
						number: index + 1,
						step_id: `s${index + 1}`,
						...step
					}))
				}
			}
		}
	};
}

const PLAN = { status: 'approved', steps: ['Gather', 'Draft', 'Review'] };

function props(status: string, extra: Record<string, unknown> = {}) {
	return {
		message: message(status),
		plan: PLAN,
		onToggleRun: vi.fn(),
		onOpenTask: vi.fn(),
		onInspectFromIds: vi.fn(),
		onWatchLive: vi.fn(),
		onStopWatching: vi.fn(),
		...extra
	};
}

beforeEach(() => {
	resetPlanStepPolls();
	resetTaskUsage();
	readTaskRunState.mockReset();
});

afterEach(() => {
	cleanup();
});

describe('LiveTaskCard plan-step line', () => {
	it('shows the segmented bar and "Step N of M · name" while a planned task runs', async () => {
		readTaskRunState.mockResolvedValue(
			panel([
				{ name: 'Gather', status: 'completed' },
				{ name: 'Draft', status: 'in_progress' },
				{ name: 'Review', status: 'pending' }
			])
		);
		const { container } = render(LiveTaskCard, props('running'));

		const line = await screen.findByTestId('live-task-step-line');
		expect(line).toHaveTextContent('Step 2 of 3 · Draft');
		const bar = screen.getByTestId('live-task-step-bar');
		expect(bar).toHaveAttribute('role', 'img');
		expect(bar).toHaveAttribute('aria-label', 'Step 2 of 3: Draft');
		const marks = [...container.querySelectorAll('.cn-live-task__seg')].map((seg) =>
			seg.getAttribute('data-mark')
		);
		expect(marks).toEqual(['done', 'inProgress', 'pending']);
		expect(readTaskRunState).toHaveBeenCalledWith('task-1', expect.any(String), expect.any(String));
	});

	it('uses one continuous fill past 12 steps', async () => {
		readTaskRunState.mockResolvedValue(
			panel(
				Array.from({ length: 13 }, (_, index) => ({
					name: `Step ${index + 1}`,
					status: index < 4 ? 'completed' : index === 4 ? 'in_progress' : 'pending'
				}))
			)
		);
		const { container } = render(LiveTaskCard, props('running'));

		expect(await screen.findByTestId('live-task-step-line')).toHaveTextContent('Step 5 of 13 · Step 5');
		expect(container.querySelectorAll('.cn-live-task__seg')).toHaveLength(0);
		expect(container.querySelector('.cn-live-task__fill')).not.toBeNull();
	});

	it('keeps the generic line when the run reports no plan steps', async () => {
		readTaskRunState.mockResolvedValue(panel([]));
		render(LiveTaskCard, props('running'));

		await waitFor(() => expect(readTaskRunState).toHaveBeenCalled());
		expect(screen.queryByTestId('live-task-step-bar')).toBeNull();
		expect(screen.queryByTestId('live-task-step-line')).toBeNull();
	});

	it('does not poll a task without a plan', async () => {
		readTaskRunState.mockResolvedValue(panel([{ name: 'Gather', status: 'in_progress' }]));
		render(LiveTaskCard, props('running', { plan: null }));

		await new Promise((resolve) => setTimeout(resolve, 20));
		// The usage snapshot may read once; the step line never appears.
		expect(screen.queryByTestId('live-task-step-bar')).toBeNull();
	});

	it('shows no step bar once the task has finished', async () => {
		readTaskRunState.mockResolvedValue(panel([{ name: 'Gather', status: 'completed' }]));
		render(LiveTaskCard, props('completed'));

		await new Promise((resolve) => setTimeout(resolve, 20));
		expect(screen.queryByTestId('live-task-step-bar')).toBeNull();
	});
});
