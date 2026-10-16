import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ChatMessage, ChatRenderTaskExecutionGroup } from '$lib/stores/chatStore';
import {
	ingestPendingHitlLineForTest,
	teardownPendingHitlBridge
} from '$lib/stores/pendingHitlStore';
import LiveTaskCard from './LiveTaskCard.svelte';
import LiveTaskCardSlotHarness from '../../../../test/fixtures/LiveTaskCardSlotHarness.svelte';

afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

const T0 = 1_760_000_000_000;

function taskMessage(
	id: string,
	status: string,
	executionId?: string,
	createdAt = T0,
	extra: Record<string, unknown> = {}
): ChatMessage {
	return {
		id,
		session_id: 'session-1',
		direction: 'system',
		created_at: createdAt,
		content: {
			type: 'task_status_update',
			task_id: 'task-1',
			display_label: 'Review release',
			status,
			execution_id: executionId,
			summary: 'Checking the release gates.',
			...extra
		}
	} as unknown as ChatMessage;
}

function group(executionId: string, message: ChatMessage): ChatRenderTaskExecutionGroup {
	return { id: executionId, message, messageIds: [message.id], updates: [message] };
}

function props(message: ChatMessage, taskExecutionGroups: ChatRenderTaskExecutionGroup[] = [], extra = {}) {
	return {
		message,
		taskExecutionGroups,
		onToggleRun: vi.fn(),
		onOpenTask: vi.fn(),
		onInspectFromIds: vi.fn(),
		onWatchLive: vi.fn(),
		onStopWatching: vi.fn(),
		...extra
	};
}

describe('LiveTaskCard', () => {
	function dot(container: HTMLElement): HTMLElement {
		return container.querySelector('[data-testid="live-task-dot"]') as HTMLElement;
	}

	it('shows a status dot, not a progress ring or percentage', () => {
		const done = taskMessage('r1', 'completed', 'e1');
		const running = taskMessage('r2', 'running', 'e2');
		const { container } = render(LiveTaskCard, props(running, [group('e1', done), group('e2', running)]));

		expect(screen.queryByRole('progressbar')).toBeNull();
		expect(container.querySelector('.cn-live-task__ring')).toBeNull();
		expect(container.textContent).not.toMatch(/\d+%/);
		expect(dot(container)).toHaveAttribute('data-status', 'running');
		expect(dot(container)).toHaveAttribute('aria-hidden', 'true');
		expect(screen.getByText('Review release')).toBeInTheDocument();
	});

	it.each([
		['running', 'running'],
		['completed', 'completed'],
		['failed', 'failed'],
		['paused', 'waiting'],
		['cancelled', 'cancelled']
	])('gives a %s task a %s dot', (status, expected) => {
		const { container } = render(LiveTaskCard, props(taskMessage('m', status)));
		expect(dot(container)).toHaveAttribute('data-status', expected);
	});

	it('gives a partial completion an amber partial dot', () => {
		const { container } = render(
			LiveTaskCard,
			props(taskMessage('m', 'completed', undefined, T0, { completion_kind: 'partial' }))
		);
		expect(dot(container)).toHaveAttribute('data-status', 'partial');
		expect(container.querySelector('.cn-live-task')).toHaveAttribute('data-status', 'partial');
		expect(screen.getByText('Partially completed')).toBeInTheDocument();
	});

	it('shows the reason in the collapsed card for failed and completed tasks', () => {
		const failed = taskMessage('m', 'failed', undefined, T0, {
			summary: 'GitHub token expired - re-login needed.'
		});
		const { unmount } = render(LiveTaskCard, props(failed));
		// The reason fits and nothing else would show: no expand toggle.
		expect(queryToggle()).toBeNull();
		// Rendered verbatim as markdown: the hyphen survives.
		expect(screen.getByText('GitHub token expired - re-login needed.')).toBeVisible();
		unmount();

		render(LiveTaskCard, props(taskMessage('m', 'completed', undefined, T0, { summary: 'Release **v1.2** tagged.' })));
		expect(queryToggle()).toBeNull();
		const reason = screen.getByTestId('live-task-reason');
		expect(reason).toHaveTextContent('Release v1.2 tagged.');
		expect(reason.querySelector('strong')).toHaveTextContent('v1.2');
	});

	it('does not repeat the reason as the current step', () => {
		const run = taskMessage('r1', 'running', 'e1');
		render(LiveTaskCard, props(run, [group('e1', run)]));
		expect(screen.getAllByText('Checking the release gates.')).toHaveLength(1);
	});

	it('offers "more" when the reason overflows its clamp, which expands the card', async () => {
		const scroll = vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockReturnValue(200);
		const client = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(60);
		try {
			render(LiveTaskCard, props(taskMessage('m', 'failed')));
			const more = await screen.findByRole('button', { name: 'Show more' });
			await fireEvent.click(more);
			expect(toggle()).toHaveAttribute('aria-expanded', 'true');
			expect(screen.queryByRole('button', { name: 'Show more' })).toBeNull();
		} finally {
			scroll.mockRestore();
			client.mockRestore();
		}
	});

	it('offers no "more" when the reason fits', () => {
		render(LiveTaskCard, props(taskMessage('m', 'failed')));
		expect(screen.queryByRole('button', { name: 'Show more' })).toBeNull();
	});

	it('ticks the elapsed time and clears the interval on unmount', async () => {
		vi.useFakeTimers();
		vi.setSystemTime(T0 + 5_000);
		const clearSpy = vi.spyOn(globalThis, 'clearInterval');
		const { unmount } = render(LiveTaskCard, props(taskMessage('m', 'running')));

		expect(screen.getByTestId('live-task-elapsed')).toHaveTextContent('0:05');
		await vi.advanceTimersByTimeAsync(2_000);
		expect(screen.getByTestId('live-task-elapsed')).toHaveTextContent('0:07');

		unmount();
		expect(clearSpy).toHaveBeenCalled();
		expect(vi.getTimerCount()).toBe(0);
	});

	it('does not tick once the task is terminal', () => {
		vi.useFakeTimers();
		render(LiveTaskCard, props(taskMessage('m', 'completed')));
		expect(vi.getTimerCount()).toBe(0);
	});

	function toggle(): HTMLElement {
		return screen.getByRole('button', { name: /Review release/ });
	}

	function queryToggle(): HTMLElement | null {
		return screen.queryByRole('button', { name: /Review release/ });
	}

	it('expands in place: the toggle flips aria-expanded and reveals the details region', async () => {
		const run = taskMessage('r1', 'running', 'e1');
		render(LiveTaskCard, props(run, [group('e1', run)]));
		const button = toggle();
		expect(button).toHaveAttribute('aria-expanded', 'false');
		expect(screen.queryByRole('region', { name: /Review release details/ })).toBeNull();

		await fireEvent.click(button);
		expect(button).toHaveAttribute('aria-expanded', 'true');
		const region = screen.getByRole('region', { name: /Review release details/ });
		expect(button.getAttribute('aria-controls')).toBe(region.id);
		expect(screen.queryByRole('dialog')).toBeNull();

		await fireEvent.click(button);
		expect(button).toHaveAttribute('aria-expanded', 'false');
	});

	function withOutputs(message: ChatMessage, names: string[]): ChatMessage {
		return {
			...message,
			content: {
				...message.content,
				output_files: names.map((name) => ({
					type: 'file',
					source: { type: 'session_output' },
					relative_path: name,
					display_name: name,
					mime_type: 'application/octet-stream'
				}))
			}
		} as unknown as ChatMessage;
	}

	it('shows a finished task\'s outputs in the card body while collapsed, with no Outputs heading', () => {
		const done = withOutputs(taskMessage('r1', 'completed', 'e1'), ['report.bin']);
		const { container } = render(LiveTaskCard, props(done, [group('e1', done)]));
		// One output, all shown: nothing left to expand.
		expect(queryToggle()).toBeNull();
		const outputs = screen.getByRole('region', { name: 'Outputs' });
		expect(outputs).toHaveTextContent('report.bin');
		expect(container.querySelector('.cn-live-task__details')).not.toContainElement(outputs);
		expect(screen.queryByRole('heading', { name: /outputs/i })).toBeNull();
		expect(container.textContent).not.toMatch(/\bOutputs\b/);
	});

	it('shows the first two outputs of a finished task and "+N more" for the rest', async () => {
		const done = withOutputs(taskMessage('r1', 'completed', 'e1'), ['a.bin', 'b.bin', 'c.bin', 'd.bin']);
		render(LiveTaskCard, props(done, [group('e1', done)]));
		const outputs = screen.getByRole('region', { name: 'Outputs' });
		expect(outputs).toHaveTextContent('a.bin');
		expect(outputs).toHaveTextContent('b.bin');
		expect(outputs).not.toHaveTextContent('c.bin');
		await fireEvent.click(screen.getByRole('button', { name: '+2 more' }));
		expect(toggle()).toHaveAttribute('aria-expanded', 'true');
		expect(screen.getByRole('region', { name: 'Outputs' })).toHaveTextContent('d.bin');
		expect(screen.queryByRole('button', { name: /more$/ })).toBeNull();
	});

	it('keeps a running task\'s outputs in the expanded details', async () => {
		const run = withOutputs(taskMessage('r1', 'running', 'e1'), ['partial.bin']);
		render(LiveTaskCard, props(run, [group('e1', run)]));
		expect(screen.queryByRole('region', { name: 'Outputs' })).toBeNull();
		await fireEvent.click(toggle());
		const region = screen.getByRole('region', { name: /Review release details/ });
		expect(region).toContainElement(screen.getByRole('region', { name: 'Outputs' }));
	});

	it('shows no runs list or step line for a finished single-run task', () => {
		const run = taskMessage('r1', 'completed', 'exec-1');
		const { container } = render(LiveTaskCard, props(run, [group('exec-1', run)]));
		expect(queryToggle()).toBeNull();
		expect(screen.queryByRole('list', { name: 'Runs' })).toBeNull();
		expect(container.querySelector('.cn-live-task__step')).toBeNull();
		expect(container.querySelector('.cn-live-task__details-inner')).toBeNull();
		expect(container.querySelector('.cn-live-task__chevron')).toBeNull();
	});

	it('shows the runs list for a finished task whose runs ended differently', async () => {
		const failed = taskMessage('r1', 'failed', 'e1');
		const done = taskMessage('r2', 'completed', 'e2');
		render(LiveTaskCard, props(done, [group('e1', failed), group('e2', done)]));
		await fireEvent.click(toggle());
		const runs = screen.getByRole('list', { name: 'Runs' });
		expect(runs).toHaveTextContent('failed');
		expect(runs).toHaveTextContent('done');
	});

	it('shows no runs list for a finished task whose runs all ended the same way', () => {
		const a = taskMessage('r1', 'completed', 'e1');
		const b = taskMessage('r2', 'completed', 'e2');
		render(LiveTaskCard, props(b, [group('e1', a), group('e2', b)]));
		expect(queryToggle()).toBeNull();
		expect(screen.queryByRole('list', { name: 'Runs' })).toBeNull();
	});

	it('shows the runs list while running when there is no activity slot', async () => {
		const run = taskMessage('r1', 'running', 'e1');
		render(LiveTaskCard, props(run, [group('e1', run)]));
		await fireEvent.click(toggle());
		expect(screen.getByRole('list', { name: 'Runs' })).toHaveTextContent('running');
	});

	it('shows one Inspect chip beside the title while collapsed, outside the toggle', () => {
		const run = taskMessage('r1', 'running', 'exec-2');
		const { container } = render(LiveTaskCard, props(run, [group('exec-2', run)]));
		expect(toggle()).toHaveAttribute('aria-expanded', 'false');

		const chip = screen.getByRole('button', { name: 'Inspect →' });
		const top = container.querySelector('.cn-live-task__top') as HTMLElement;
		const meta = container.querySelector('.cn-live-task__meta') as HTMLElement;
		expect(top).toContainElement(chip);
		expect(meta).not.toContainElement(chip);
		expect(chip).toHaveClass('cn-live-task__chip');
		expect(chip).toHaveClass('mg-chip-action');
		expect(toggle()).not.toContainElement(chip);
		expect(screen.queryByRole('button', { name: 'Open task →' })).toBeNull();
	});

	it('does not toggle the expansion when a chip is clicked', async () => {
		const run = taskMessage('r1', 'running', 'exec-2');
		const p = props(run, [group('exec-2', run)]);
		render(LiveTaskCard, p);
		await fireEvent.click(screen.getByRole('button', { name: 'Inspect →' }));
		expect(toggle()).toHaveAttribute('aria-expanded', 'false');
		expect(p.onInspectFromIds).toHaveBeenCalled();
	});

	it('lets clicks on the chevron reach the header toggle (expanded, it is rotated)', async () => {
		// jsdom does no hit-testing, so the rule itself is the contract: a rotated
		// chevron paints above the absolutely positioned toggle and would swallow
		// the click that should collapse the card.
		const { readFileSync } = await import('node:fs');
		const { join } = await import('node:path');
		const source = readFileSync(
			join(process.cwd(), 'src/lib/magician/chat/next/LiveTaskCard.svelte'),
			'utf8'
		);
		const rule = source.match(/(?:^|\n)\s*\.cn-live-task__chevron \{[^}]*\}/)?.[0] ?? '';
		expect(rule).toContain('pointer-events: none');
	});

	it('puts the chevron in the top (title) row', () => {
		const run = taskMessage('r1', 'running', 'e1');
		const { container } = render(LiveTaskCard, props(run, [group('e1', run)]));
		const top = container.querySelector('.cn-live-task__top') as HTMLElement;
		expect(top).toHaveTextContent('Review release');
		expect(top.querySelector('.cn-live-task__chevron')).not.toBeNull();
	});

	it('offers Run and previews the approved plan with a plan graph chip', async () => {
		const onRun = vi.fn();
		const onOpenPlan = vi.fn();
		const ready = taskMessage('r1', 'ready');
		const { container } = render(
			LiveTaskCard,
			props(ready, [], {
				onRun,
				onOpenPlan,
				plan: { status: 'approved', steps: ['Look up the weather', 'Write the brief'] }
			})
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Run' }));
		expect(onRun).toHaveBeenCalledWith('task-1');

		// Expanding reveals the plan's steps and the plan graph chip.
		const toggle = container.querySelector('.cn-live-task__toggle') as HTMLElement;
		await fireEvent.click(toggle);
		expect(container.querySelector('.cn-live-task__plan-steps')).toHaveTextContent('Look up the weather');
		await fireEvent.click(screen.getByRole('button', { name: 'Open plan graph' }));
		expect(onOpenPlan).toHaveBeenCalledWith('task-1');
	});

	it('asks for the plan once when an unstarted card has no steps yet', () => {
		const onNeedPlan = vi.fn();
		render(LiveTaskCard, props(taskMessage('r1', 'ready'), [], { onNeedPlan, plan: null }));
		expect(onNeedPlan).toHaveBeenCalledTimes(1);
		expect(onNeedPlan).toHaveBeenCalledWith('task-1');
	});

	it('shows an approved, unstarted task as not started: no timer, no Cancel', () => {
		const ready = taskMessage('r1', 'ready', 'e1');
		const { container } = render(LiveTaskCard, props(ready, [], { onCancel: vi.fn() }));
		expect(container).toHaveTextContent('Plan approved, not started');
		const meta = container.querySelector('.cn-live-task__meta') as HTMLElement;
		expect(meta.firstElementChild).toHaveTextContent('Not started');
		expect(screen.queryByTestId('live-task-elapsed')).toBeNull();
		expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
	});

	it('puts Cancel in the title row, just before the chevron, outside the toggle', () => {
		const run = taskMessage('r1', 'running', 'e1');
		const { container } = render(LiveTaskCard, props(run, [group('e1', run)], { onCancel: vi.fn() }));
		const cancel = screen.getByRole('button', { name: 'Cancel' });
		const top = container.querySelector('.cn-live-task__top') as HTMLElement;
		expect(top).toContainElement(cancel);
		expect(cancel).toHaveClass('mg-chip-action');
		expect(cancel).toHaveAttribute('aria-haspopup', 'dialog');
		const chevron = top.querySelector('.cn-live-task__chevron');
		expect(chevron).not.toBeNull();
		expect(cancel.closest('.cn-live-task__cancel-wrap')?.nextElementSibling).toBe(chevron);
		expect(container.querySelector('.cn-live-task__details')).not.toContainElement(cancel);
	});

	it('is not expandable when the only extra would be Cancel', () => {
		// A planning task: no run id yet, an empty live activity slot.
		const { container } = render(LiveTaskCardSlotHarness, {
			message: taskMessage('m', 'planning'),
			slotText: '',
			onCancel: vi.fn()
		});
		expect(queryToggle()).toBeNull();
		expect(container.querySelector('[aria-expanded]:not(.cn-live-task__cancel)')).toBeNull();
		expect(container.querySelector('.cn-live-task__chevron')).toBeNull();
		expect(screen.getByRole('button', { name: 'Cancel' })).toBeInTheDocument();
		expect(screen.getByRole('group', { name: /^Review release, / })).toBeInTheDocument();
	});

	it('becomes expandable once the live activity slot renders something', async () => {
		const message = taskMessage('m', 'running', 'exec-2');
		const groups = [group('exec-2', message)];
		const { rerender } = render(LiveTaskCardSlotHarness, {
			message,
			taskExecutionGroups: groups,
			slotText: ''
		});
		expect(queryToggle()).toBeNull();
		await rerender({ message, taskExecutionGroups: groups, slotText: 'Reading the inbox' });
		await vi.waitFor(() => expect(queryToggle()).not.toBeNull());
		await fireEvent.click(toggle());
		expect(screen.getByRole('region', { name: /Review release details/ })).toHaveTextContent(
			'Reading the inbox'
		);
	});

	it('shows no Outputs section when no run has blocks', async () => {
		const run = taskMessage('r1', 'running', 'e1');
		render(LiveTaskCard, props(run, [group('e1', run)]));
		await fireEvent.click(toggle());
		expect(screen.queryByRole('region', { name: 'Outputs' })).toBeNull();
	});

	it('Inspect falls back to the task view without a run id', async () => {
		const p = props(taskMessage('m', 'running'));
		const { container } = render(LiveTaskCard, p);
		await fireEvent.click(screen.getByRole('button', { name: 'Inspect →' }));
		expect(p.onOpenTask).toHaveBeenCalledWith(
			expect.objectContaining({ task_id: 'task-1' }),
			container.querySelector('.cn-live-task')
		);
	});

	it('Inspect opens the latest run when both ids and a handler exist', async () => {
		const run = taskMessage('r1', 'running', 'exec-2');
		const p = props(run, [group('exec-2', run)]);
		render(LiveTaskCard, p);
		await fireEvent.click(screen.getByRole('button', { name: 'Inspect →' }));
		expect(p.onInspectFromIds).toHaveBeenCalledWith({ taskId: 'task-1', executionId: 'exec-2' });
		expect(p.onOpenTask).not.toHaveBeenCalled();
	});

	it('confirms in a popover anchored to Cancel, focus in and back', async () => {
		const onCancel = vi.fn();
		const run = taskMessage('r1', 'running', 'e1');
		render(LiveTaskCard, props(run, [group('e1', run)], { onCancel }));
		const cancel = screen.getByRole('button', { name: 'Cancel' });

		await fireEvent.click(cancel);
		expect(onCancel).not.toHaveBeenCalled();
		const dialog = screen.getByRole('dialog', { name: 'Cancel this task?' });
		expect(dialog).toHaveAttribute('aria-modal', 'false');
		expect(cancel).toHaveAttribute('aria-expanded', 'true');
		expect(cancel.closest('.cn-live-task__cancel-wrap')).toContainElement(dialog);
		// The popover never toggles the card.
		expect(toggle()).toHaveAttribute('aria-expanded', 'false');
		await vi.waitFor(() =>
			expect(screen.getByRole('button', { name: 'Keep running' })).toHaveFocus()
		);

		await fireEvent.click(screen.getByRole('button', { name: 'Keep running' }));
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(cancel).toHaveFocus();
		expect(onCancel).not.toHaveBeenCalled();

		await fireEvent.click(cancel);
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel task' }));
		expect(onCancel).toHaveBeenCalledWith('task-1');
		await vi.waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(toggle()).toHaveAttribute('aria-expanded', 'false');
	});

	it('closes the cancel popover on Escape (focus back) and on a click outside', async () => {
		const onCancel = vi.fn();
		render(LiveTaskCard, props(taskMessage('m', 'running'), [], { onCancel }));
		const cancel = screen.getByRole('button', { name: 'Cancel' });

		await fireEvent.click(cancel);
		expect(screen.getByRole('dialog')).toBeInTheDocument();
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(cancel).toHaveFocus();

		await fireEvent.click(cancel);
		await fireEvent.pointerDown(screen.getByRole('dialog'));
		expect(screen.getByRole('dialog')).toBeInTheDocument();
		await fireEvent.pointerDown(document.body);
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(onCancel).not.toHaveBeenCalled();
	});

	it('offers no Cancel once terminal or without a handler', () => {
		const { unmount } = render(
			LiveTaskCard,
			props(taskMessage('m', 'completed'), [], { onCancel: vi.fn() })
		);
		expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
		unmount();

		render(LiveTaskCard, props(taskMessage('m', 'running')));
		expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
	});

	it('renders the activity slot while running, with the task and run ids', async () => {
		const run = taskMessage('r1', 'running', 'exec-2');
		render(LiveTaskCardSlotHarness, { message: run, taskExecutionGroups: [group('exec-2', run)] });
		await fireEvent.click(toggle());
		expect(screen.getByTestId('activity-slot')).toHaveTextContent('task-1:exec-2');
		expect(screen.queryByRole('list', { name: 'Runs' })).toBeNull();
	});

	it('drops the activity slot once terminal and falls back to the run list', async () => {
		const failed = taskMessage('r1', 'failed', 'exec-1');
		const run = taskMessage('r2', 'completed', 'exec-2');
		render(LiveTaskCardSlotHarness, {
			message: run,
			taskExecutionGroups: [group('exec-1', failed), group('exec-2', run)]
		});
		await fireEvent.click(toggle());
		expect(screen.queryByTestId('activity-slot')).toBeNull();
		expect(screen.getByRole('list', { name: 'Runs' })).toHaveTextContent('done');
	});

	it('pops the dot once, on the transition to completed', async () => {
		const running = taskMessage('m', 'running');
		const { container, rerender } = render(LiveTaskCard, props(running));
		const statusDot = dot(container);
		expect(statusDot).not.toHaveClass('cn-live-task__dot--pop');

		await rerender(props(taskMessage('m', 'completed')));
		expect(statusDot).toHaveClass('cn-live-task__dot--pop');

		await fireEvent.animationEnd(statusDot);
		expect(statusDot).not.toHaveClass('cn-live-task__dot--pop');

		await rerender(props(taskMessage('m2', 'completed')));
		expect(statusDot).not.toHaveClass('cn-live-task__dot--pop');
	});

	it('does not pop a card that mounts already completed, nor a failed or partial finish', async () => {
		const { container, unmount } = render(LiveTaskCard, props(taskMessage('m', 'completed')));
		expect(dot(container)).not.toHaveClass('cn-live-task__dot--pop');
		unmount();

		for (const [status, extra] of [
			['failed', {}],
			['completed', { completion_kind: 'partial' }]
		] as const) {
			const r = render(LiveTaskCard, props(taskMessage('m', 'running')));
			await r.rerender(props(taskMessage('m', status, undefined, T0, extra)));
			expect(dot(r.container)).not.toHaveClass('cn-live-task__dot--pop');
			r.unmount();
		}
	});

	describe('Needs you', () => {
		function pendingAskFor(taskId: string): void {
			ingestPendingHitlLineForTest(
				JSON.stringify({
					event_type: 'HitlRequested',
					data: {
						correlation_id: `ask-${taskId}`,
						source: 'clarification',
						input_type: 'text',
						prompt: 'Which account?',
						principal: 'owner',
						workspace: 'default',
						task_id: taskId,
						execution_id: 'planexec-1'
					}
				})
			);
		}

		afterEach(() => teardownPendingHitlBridge());

		it('marks a paused task with a pending ask as needing the user, with its reason', () => {
			pendingAskFor('task-1');
			const { container } = render(
				LiveTaskCard,
				props(taskMessage('m', 'paused', undefined, T0, { summary: 'Approve the e-mail send.' }))
			);
			expect(screen.getByText('Needs you')).toBeInTheDocument();
			expect(screen.getByText('Approve the e-mail send.')).toBeInTheDocument();
			expect(container.querySelector('.cn-live-task')).toHaveAttribute('data-status', 'waiting');
		});

		it('does not claim a paused task needs the user when nothing is asked', () => {
			render(LiveTaskCard, props(taskMessage('m', 'paused')));
			expect(screen.queryByText('Needs you')).toBeNull();
		});

		it('marks a running task blocked on a mid-run ask', () => {
			pendingAskFor('task-1');
			render(LiveTaskCard, props(taskMessage('m', 'running', 'e1')));
			expect(screen.getByText('Needs you')).toBeInTheDocument();
		});

		it('ignores an ask that blocks another task', () => {
			pendingAskFor('task-2');
			render(LiveTaskCard, props(taskMessage('m', 'running', 'e1')));
			expect(screen.queryByText('Needs you')).toBeNull();
		});

		it('keeps a drafted plan awaiting approval as needing the user', () => {
			render(LiveTaskCard, props(taskMessage('m', 'draft')));
			expect(screen.getByText('Needs you')).toBeInTheDocument();
		});
	});

	it('names the button with title and status, never a percentage or the ticking time', () => {
		const done = taskMessage('r1', 'completed', 'e1');
		const running = taskMessage('r2', 'running', 'e2', Date.now() - 5_000);
		render(LiveTaskCard, props(running, [group('e1', done), group('e2', running)]));
		const card = screen.getByRole('button', { name: /Review release/ });
		const name = card.getAttribute('aria-label') ?? '';
		expect(name).toBe('Review release, Running — show details');
		expect(name).not.toMatch(/%/);
		expect(name).not.toMatch(/\d+:\d{2}/);
	});

	it('copies the full task id from the short id in an id-only title', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		const message = taskMessage('m', 'completed');
		(message.content as { display_label?: string | null }).display_label = null;
		(message.content as { task_id?: string }).task_id = 'task_2679cefd6f664bf7bd4a3b57fae4ce75';
		render(LiveTaskCard, props(message));

		const idButton = screen.getByRole('button', {
			name: 'Copy task id task_2679cefd6f664bf7bd4a3b57fae4ce75'
		});
		expect(idButton).toHaveTextContent('2679ce');
		await fireEvent.click(idButton);
		await vi.waitFor(() =>
			expect(writeText).toHaveBeenCalledWith('task_2679cefd6f664bf7bd4a3b57fae4ce75')
		);
		// Copying never expands the card.
		expect(document.querySelector('.cn-live-task--expanded')).toBeNull();
	});
});
