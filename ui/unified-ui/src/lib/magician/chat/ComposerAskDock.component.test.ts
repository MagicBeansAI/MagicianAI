import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

const { answerTaskAsk, gotoMock, openMock } = vi.hoisted(() => ({
	answerTaskAsk: vi.fn(async () => ({ ok: true as const })),
	gotoMock: vi.fn(async () => undefined), openMock: vi.fn()
}));
vi.mock('$app/navigation', () => ({ goto: gotoMock }));
vi.mock('$lib/attention/openHitlPrompt', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/attention/openHitlPrompt')>()), openHitlPrompt: openMock
}));
vi.mock('$lib/magician/tasks/taskAsk', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/magician/tasks/taskAsk')>()),
	answerTaskAsk
}));

import type { HitlOpenTarget } from '$lib/hitl/types';
import ComposerAskDock from './ComposerAskDock.svelte';

afterEach(() => { cleanup(); vi.clearAllMocks(); });

function choice(id: string, prompt: string): HitlOpenTarget {
	return {
		id,
		source: 'agentic',
		input_type: 'choice',
		prompt,
		input_schema: {
			options: [
				{ id: 'q1', label: 'Q1' },
				{ id: 'q2', label: 'Q2' }
			]
		},
		identifiers: { pause_state_id: id, correlation_id: id },
		scope: { principal: 'anonymous', workspace: 'default', task_id: 'task-1' }
	} as HitlOpenTarget;
}

describe('ComposerAskDock', () => {
	it('keeps a second answer busy when an earlier question finishes late', async () => {
		let finishFirst!: (value: { ok: true }) => void;
		let finishSecond!: (value: { ok: true }) => void;
		answerTaskAsk.mockReturnValueOnce(new Promise((resolve) => { finishFirst = resolve; }))
			.mockReturnValueOnce(new Promise((resolve) => { finishSecond = resolve; }));
		const view = render(ComposerAskDock, { targets: [choice('first', 'First question')] });
		await fireEvent.click(screen.getByLabelText('Q1'));
		await fireEvent.click(screen.getByRole('button', { name: 'Answer' }));
		await view.rerender({ targets: [choice('second', 'Second question')] });
		await fireEvent.click(screen.getByLabelText('Q1'));
		await fireEvent.click(screen.getByRole('button', { name: 'Answer' }));
		finishFirst({ ok: true });
		await waitFor(() => expect(screen.getByRole('button', { name: 'Sending…' })).toBeDisabled());
		expect(answerTaskAsk).toHaveBeenCalledTimes(2);
		finishSecond({ ok: true });
		await waitFor(() => expect(screen.getByRole('button', { name: 'Answer' })).toBeEnabled());
	});

	it('declines an MCP sign-in with "Not now" from the composer', async () => {
		const target = {
			id: 'mcp_oauth:binding:flow:attempt',
			source: 'mcp_oauth',
			input_type: 'external_action',
			prompt: 'Finish connecting Swiggy (food) in your browser.',
			input_schema: {
				instructions: 'Complete sign-in here, or open it on the computer.',
				done_label: 'Open on computer',
				dismiss_label: 'Not now',
				service_label: 'Swiggy (food)'
			},
			identifiers: { correlation_id: 'mcp_oauth:binding:flow:attempt' },
			scope: { principal: 'anonymous', workspace: 'default' }
		} as HitlOpenTarget;
		render(ComposerAskDock, { targets: [target] });
		await fireEvent.click(screen.getByRole('button', { name: 'Not now' }));
		expect(answerTaskAsk).toHaveBeenCalledWith(
			target,
			{ kind: 'choice', choiceId: 'dismiss' },
			expect.anything()
		);
	});

	it('opens a plan from a composer without a host-specific review callback', async () => {
		const target = choice('plan', 'Review this plan'); target.source = 'plan_approval';
		target.input_type = 'confirmation';
		render(ComposerAskDock, { targets: [target] });
		await fireEvent.click(screen.getByRole('button', { name: 'Review plan' }));
		expect(gotoMock).toHaveBeenCalledWith('/work?selected=task-1&plan=open');
	});

	it('does not queue a second focused form for the same request', async () => {
		let finish!: (value: { status: 'cancelled' }) => void;
		openMock.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
		const target = choice('secret', 'Enter your password'); target.input_type = 'password';
		render(ComposerAskDock, { targets: [target] });
		const button = screen.getByRole('button', { name: 'Enter it securely →' });
		await fireEvent.click(button); await fireEvent.click(button);
		expect(openMock).toHaveBeenCalledTimes(1);
		finish({ status: 'cancelled' });
		await waitFor(() => expect(button).toBeEnabled());
	});
	it.each([['allow_once', 'Allow once'], ['session:file', 'This session'], ['always:file', 'Always'], ['deny', 'Deny']])('submits the God-mode %s decision from the composer', async (decision, label) => {
		const target = choice('god-file', 'God mode: write this protected file?');
		target.source = 'user_request';
		target.identifiers = { request_id: target.id, correlation_id: target.id };
		target.input_schema = {
			request_type: 'sandbox_override',
			context: { god_mode: true, chat_session_id: 'chat-1', session: 'chat:chat-1', agent_id: 'personal-assistant', command: '/notes/programs/plan.md', files: [{ path: '/notes/programs/plan.md', status: 'M', additions: 1, deletions: 1, unified_diff: '@@ -1 +1 @@\n-old plan\n+new plan\n' }] },
			options: [
				{ id: 'allow_once', label: 'Allow once' },
				{ id: 'session:file', label: 'This session: this file' },
				{ id: 'always:file', label: 'Always: this file' },
				{ id: 'deny', label: 'Deny' }
			]
		};
		render(ComposerAskDock, { targets: [target], originChatTitles: { 'chat-1': 'Operating updates' } });
        expect(screen.getByText(/Requested in Operating updates · personal-assistant/)).toBeTruthy();
        expect(screen.getByText(/This session.*refers to that originating chat or run/)).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Allow once' })).toBeTruthy();
		expect(screen.getByRole('button', { name: 'This session' })).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Always' })).toBeTruthy();
		expect(screen.queryByRole('button', { name: 'Answer' })).toBeNull();
		expect(screen.getByText('Staged changes')).toBeTruthy();
		await fireEvent.keyDown(screen.getByTestId('composer-ask-dock'), { key: 'Enter' });
		expect(answerTaskAsk).not.toHaveBeenCalled();
		await fireEvent.click(screen.getByRole('button', { name: label }));
		await waitFor(() => expect(answerTaskAsk).toHaveBeenLastCalledWith(
			target, expect.objectContaining({ kind: 'choice', choiceId: decision }), expect.anything()
		));
	});

	it('shows long scopes separately and submits the selected scope without changing its id', async () => {
		const path = `/notes/${'long-folder/'.repeat(25)}plan.md`;
		const target = choice('god-scopes', 'Apply these edits?');
		target.source = 'user_request';
		target.identifiers = { request_id: target.id, correlation_id: target.id };
		target.input_schema = {
			request_type: 'sandbox_override',
			context: { god_mode: true, files: [{ path, unified_diff: '+new line\n'.repeat(1000) }] },
			options: [
				{ id: 'allow_once', label: 'Allow once' },
				{ id: 'session:file', label: `This session: ${path}` },
				{ id: 'always:file', label: `Always: ${path}` },
				{ id: 'session:folder', label: 'This session: anything under /notes' },
				{ id: 'always:folder', label: 'Always: anything under /notes' },
				{ id: 'deny', label: 'Deny' }
			]
		};
		render(ComposerAskDock, { targets: [target] });
		expect(screen.getByText(path, { selector: '.grant-scope-detail' })).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Always' })).toBeEnabled();
		await fireEvent.change(screen.getByRole('combobox', { name: 'Grant scope' }), { target: { value: 'folder' } });
		expect(screen.getByText('anything under /notes')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Always' }));
		await waitFor(() => expect(answerTaskAsk).toHaveBeenLastCalledWith(
			target, expect.objectContaining({ kind: 'choice', choiceId: 'always:folder' }), expect.anything()
		));
	});

	it('shows the question with its answer options above the composer', () => {
		render(ComposerAskDock, { targets: [choice('p1', 'Which quarter?')] });
		expect(screen.getByTestId('composer-ask-dock')).toHaveTextContent('Waiting on you');
		expect(screen.getByText('Which quarter?')).toBeTruthy();
		expect(screen.getByText('Q1')).toBeTruthy();
		expect(screen.queryByText(/of 2/)).toBeNull();
	});

	it('switches between several questions without losing its place', async () => {
		render(ComposerAskDock, {
			targets: [choice('p1', 'Which quarter?'), choice('p2', 'Which region?')]
		});
		expect(screen.getByText('1 of 2')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Next question' }));
		expect(screen.getByText('Which region?')).toBeTruthy();
		expect(screen.getByText('2 of 2')).toBeTruthy();
	});

	it('offers approve/reject and the review link for a Relentless review, answered in place', async () => {
		const review: HitlOpenTarget = {
			id: 'req-review-1',
			source: 'user_request',
			input_type: 'choice',
			prompt: 'Relentless accepted a draft. Approve it?',
			input_schema: {
				request_type: 'relentless.accepted_review',
				options: [
					{ id: 'approve', label: 'Approve draft' },
					{ id: 'reject', label: 'Reject draft' }
				],
				review_href: '/work?view=quality&run=run-1&task=task-r',
				review_label: 'Review draft'
			},
			identifiers: { request_id: 'req-review-1', correlation_id: 'req-review-1' },
			scope: { principal: 'anonymous', workspace: 'default', task_id: 'task-r' }
		};
		render(ComposerAskDock, { targets: [review] });
		expect(screen.getByRole('link', { name: 'Review draft →' })).toHaveAttribute(
			'href',
			'/work?view=quality&run=run-1&task=task-r'
		);
		expect(screen.getByText('Reject draft')).toBeTruthy();
		const submit = screen.getByRole('button', { name: 'Answer' });
		expect(submit).toBeDisabled();
		await fireEvent.click(screen.getByRole('radio', { name: /Approve draft/ }));
		await fireEvent.click(submit);
		await waitFor(() => expect(answerTaskAsk).toHaveBeenCalledTimes(1));
		expect(answerTaskAsk).toHaveBeenCalledWith(
			review,
			expect.objectContaining({ kind: 'choice', choiceId: 'approve' }),
			expect.anything()
		);
	});

	it('renders nothing when nothing is waiting', () => {
		render(ComposerAskDock, { targets: [] });
		expect(screen.queryByTestId('composer-ask-dock')).toBeNull();
	});
});
