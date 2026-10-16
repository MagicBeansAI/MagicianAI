/**
 * A preference alignment hold, rendered through the modal every surface uses.
 *
 * Pinned: the private reason appears only from the owner-private read; the
 * act's choices, the lasting change and the reply are separate controls; a
 * lasting change is a readback first and a token-bearing confirmation
 * second; nothing is preselected.
 */
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/scopeIdentityStore', async (importOriginal) => {
	const actual = await importOriginal<Record<string, unknown>>();
	return {
		...actual,
		scopedRequestHeaders: (headers: Record<string, string>) => headers
	};
});

import AttentionPromptModal from '$lib/magician/components/AttentionPromptModal.svelte';
import {
	requestAttentionInput,
	resolveAttentionPrompt,
	type AttentionPromptResult
} from '$lib/stores/attentionPromptStore';

import type { AlignmentProjection } from './alignmentAsk';

const projection: AlignmentProjection = {
	askId: 'ask-1',
	purpose: 'allow_once',
	choices: [
		{ id: 'allow_once', label: 'Allow this once' },
		{ id: 'change_preference', label: 'Change my preference' },
		{ id: 'cancel', label: 'Cancel' }
	],
	freeText: true,
	deadlineAtMs: null,
	stage: 'decide',
	alsoCovers: []
};

function detail(stage: 'decide' | 'confirm_wording') {
	return {
		private: true,
		alignment: {
			kind: 'preference_alignment',
			ask_id: 'ask-1',
			stage,
			free_text: true,
			explanation: 'You asked to keep dinners vegetarian.',
			act_summary: 'swiggy order',
			intentions: [
				{
					id: 'pref-1',
					statement: 'diet: vegetarian dinners',
					assertion_status: 'direct_owner_statement',
					editable: true
				}
			],
			choices:
				stage === 'decide'
					? [
							{ id: 'allow_once', label: 'Allow this once', lasting: false, consent: true },
							{
								id: 'change_preference',
								label: 'Change my preference',
								lasting: true,
								needs_wording: true
							},
							{ id: 'cancel', label: 'Cancel', lasting: false }
						]
					: [
							{ id: 'confirm_wording', label: 'Save this wording', lasting: true },
							{ id: 'keep_as_is', label: "Don't change it", lasting: false },
							{ id: 'cancel', label: 'Cancel', lasting: false }
						],
			readback:
				stage === 'confirm_wording'
					? {
							kind: 'replace',
							wording: 'vegetarian on weekdays',
							superseded: 'diet: vegetarian dinners',
							token: 'tok-123'
						}
					: undefined
		}
	};
}

function open(stage: 'decide' | 'confirm_wording'): Promise<AttentionPromptResult | null> {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => new Response(JSON.stringify(detail(stage)), { status: 200 }))
	);
	render(AttentionPromptModal);
	return requestAttentionInput({
		title: 'Review before it runs',
		body: 'An action needs your review before it runs.',
		kind: 'confirmation',
		confirmation: { confirmLabel: 'Allow this once', denyLabel: 'Cancel', destructive: false },
		alignment: {
			correlationId: 'exec-1:plan:step~ask-1',
			projection: { ...projection, stage }
		}
	});
}

beforeEach(() => vi.unstubAllGlobals());
afterEach(() => {
	resolveAttentionPrompt(null);
	cleanup();
	vi.unstubAllGlobals();
});

describe('a preference alignment hold', () => {
	it('shows the private reason and answers the act with its typed choice', async () => {
		const answered = open('decide');
		expect(await screen.findByTestId('alignment-explanation')).toHaveTextContent(
			'You asked to keep dinners vegetarian.'
		);
		expect(screen.getByText('diet: vegetarian dinners', { exact: false })).toBeInTheDocument();
		// The lasting change is its own section, not one of the act's buttons.
		expect(screen.getByText('Change what Magician remembers')).toBeInTheDocument();
		expect(screen.queryAllByRole('radio')).toHaveLength(0);

		await fireEvent.click(screen.getByRole('button', { name: 'Allow this once' }));
		expect(await answered).toEqual({ kind: 'choice', choiceId: 'allow_once', input: undefined });
	});

	it('asks for the new wording before anything is read back', async () => {
		const answered = open('decide');
		await screen.findByTestId('alignment-explanation');
		await fireEvent.click(screen.getByRole('button', { name: 'Change my preference' }));
		const readBack = screen.getByRole('button', { name: 'Read it back' });
		expect(readBack).toBeDisabled();
		await fireEvent.input(screen.getByLabelText('New wording for the preference'), {
			target: { value: 'vegetarian on weekdays' }
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Read it back' }));
		expect(await answered).toEqual({
			kind: 'choice',
			choiceId: 'change_preference',
			input: 'vegetarian on weekdays'
		});
	});

	it('confirms the exact readback with its token', async () => {
		const answered = open('confirm_wording');
		expect(await screen.findByTestId('alignment-readback')).toHaveTextContent(
			'vegetarian on weekdays'
		);
		expect(screen.getByText('Replaces: diet: vegetarian dinners')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Save this wording' }));
		expect(await answered).toEqual({
			kind: 'choice',
			choiceId: 'confirm_wording',
			input: 'tok-123'
		});
	});

	it('sends a typed reply as the owner’s words', async () => {
		const answered = open('decide');
		await screen.findByTestId('alignment-explanation');
		await fireEvent.input(screen.getByLabelText('Reply with what to do instead'), {
			target: { value: 'order the paneer one' }
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Send reply' }));
		expect(await answered).toEqual({
			kind: 'choice',
			choiceId: 'respond',
			input: 'order the paneer one'
		});
	});

	it('still answers when the reason cannot be loaded, without lasting choices', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { status: 503 })));
		render(AttentionPromptModal);
		const answered = requestAttentionInput({
			title: 'Review before it runs',
			kind: 'confirmation',
			confirmation: { confirmLabel: 'Allow this once', denyLabel: 'Cancel', destructive: false },
			alignment: { correlationId: 'c', projection }
		});
		expect(
			await screen.findByText('The reason could not be loaded here. Your answer still works.')
		).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Change my preference' })).not.toBeInTheDocument();
		// The hold's cancel is a choice, named apart from the dialog's Cancel.
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel the action' }));
		expect(await answered).toEqual({ kind: 'choice', choiceId: 'cancel', input: undefined });
	});
});
