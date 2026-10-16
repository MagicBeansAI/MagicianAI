import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { HitlOpenTarget } from '$lib/hitl/types';
import type { PendingAsk } from '$lib/attention/pendingAsks';
import TaskPanelDrawer from './TaskPanelDrawer.svelte';
import type { TaskPanelModel } from './UnifiedTaskPanel.svelte';

/**
 * The deep panel lists an ask that only the pending store / Attention feed
 * carries — the run payload (`needs_attention`) has none — and answers it in
 * place through `answerTaskAsk`, on a surface that wires no answer handler
 * (the internal and Crew drawers).
 */
const { answerTaskAsk, pendingAsks } = vi.hoisted(() => {
	let value: unknown[] = [];
	const subscribers = new Set<(next: unknown[]) => void>();
	return {
		answerTaskAsk: vi.fn(async () => ({ ok: true as const })),
		pendingAsks: {
			subscribe(run: (next: unknown[]) => void) {
				subscribers.add(run);
				run(value);
				return () => subscribers.delete(run);
			},
			set(next: unknown[]) {
				value = next;
				for (const run of subscribers) run(value);
			}
		}
	};
});

vi.mock('$lib/attention/pendingAsks', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/attention/pendingAsks')>()),
	pendingAsks
}));
vi.mock('./taskAsk', async (importOriginal) => ({
	...(await importOriginal<typeof import('./taskAsk')>()),
	answerTaskAsk
}));

const NOW = 1_000_000;

const TASK: TaskPanelModel = {
	id: 'task-relentless',
	status: 'running',
	attention: null,
	ask: null,
	error: null,
	queuedFor: null,
	currentStep: null,
	totalSteps: null,
	currentStepLabel: null,
	elapsedMs: null,
	lastProgressAt: null,
	plan: null,
	run: { steps: [], timeline: null, responsibility: null, provenance: [] },
	output: null,
	runs: null
};

function reviewAsk(id = 'req-review-1', prompt = 'Relentless accepted a draft. Approve it?'): HitlOpenTarget {
	return {
		id,
		source: 'user_request',
		input_type: 'choice',
		prompt,
		input_schema: {
			request_type: 'relentless.accepted_review',
			options: [
				{ id: 'approve', label: 'Approve draft' },
				{ id: 'reject', label: 'Reject draft' }
			],
			review_href: '/work?view=quality&run=run-1&task=task-relentless',
			review_label: 'Review draft'
		},
		identifiers: { request_id: id, correlation_id: id },
		scope: { principal: 'anonymous', workspace: 'default', task_id: 'task-relentless' },
		at: NOW - 60_000
	};
}

function pending(target: HitlOpenTarget): PendingAsk {
	return { target, taskId: 'task-relentless', threadId: null, at: target.at ?? 0 };
}

afterEach(() => {
	cleanup();
	pendingAsks.set([]);
	answerTaskAsk.mockClear();
});

describe('TaskPanelDrawer folds in pending asks for its task', () => {
	it('lists and answers an ask present only in the pending store / feed', async () => {
		pendingAsks.set([pending(reviewAsk())]);
		render(TaskPanelDrawer, { props: { task: TASK, title: 'Draft the launch note', now: NOW } });

		expect(screen.getByText('Relentless accepted a draft. Approve it?', { selector: '.ask__prompt' })).toBeTruthy();
		expect(screen.getByRole('link', { name: 'Review draft →' })).toHaveAttribute(
			'href',
			'/work?view=quality&run=run-1&task=task-relentless'
		);
		expect(screen.getByText('Reject draft')).toBeTruthy();

		await fireEvent.click(screen.getByRole('radio', { name: /Approve draft/ }));
		await fireEvent.click(screen.getByRole('button', { name: 'Answer' }));
		await waitFor(() => expect(answerTaskAsk).toHaveBeenCalledTimes(1));
		expect(answerTaskAsk).toHaveBeenCalledWith(
			expect.objectContaining({ id: 'req-review-1' }),
			expect.objectContaining({ kind: 'choice', choiceId: 'approve' }),
			expect.anything(),
			{},
			{ cancel: false }
		);
	});

	it('lists every pending ask for the task, the rest under "Also waiting on you"', () => {
		pendingAsks.set([pending(reviewAsk()), pending(reviewAsk('req-budget', 'Extend the budget?'))]);
		render(TaskPanelDrawer, { props: { task: TASK, title: 'Draft the launch note', now: NOW } });
		const more = screen.getByRole('list', { name: 'Also waiting on you' });
		expect(more).toHaveTextContent('Extend the budget?');
	});

	it('ignores asks for other tasks', () => {
		pendingAsks.set([pending(reviewAsk())]);
		render(TaskPanelDrawer, {
			props: { task: { ...TASK, id: 'task-other' }, title: 'Other', now: NOW }
		});
		expect(screen.queryByText('Relentless accepted a draft. Approve it?')).toBeNull();
	});

	it('passes the answer to a surface that answers asks itself', async () => {
		pendingAsks.set([pending(reviewAsk())]);
		const onAnswer = vi.fn();
		render(TaskPanelDrawer, {
			props: { task: TASK, title: 'Draft the launch note', now: NOW, answerAsk: true },
			events: { answer: (event: CustomEvent) => onAnswer(event.detail) }
		});
		await fireEvent.click(screen.getByRole('radio', { name: /Reject draft/ }));
		await fireEvent.click(screen.getByRole('button', { name: 'Answer' }));
		await waitFor(() => expect(onAnswer).toHaveBeenCalledTimes(1));
		expect(answerTaskAsk).not.toHaveBeenCalled();
		expect(onAnswer.mock.calls[0][0]).toEqual(
			expect.objectContaining({ result: expect.objectContaining({ choiceId: 'reject' }) })
		);
	});
});
