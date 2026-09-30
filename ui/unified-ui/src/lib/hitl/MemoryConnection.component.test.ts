import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import AttentionPromptModal from '$lib/magician/components/AttentionPromptModal.svelte';
import { resolveAttentionPrompt } from '$lib/stores/attentionPromptStore';
import { respondToHitl } from './respondToHitl';
import type { HitlRequest } from './types';

const timedFetch = vi.hoisted(() => vi.fn());
vi.mock('$lib/shared/fetch', () => ({
	timedFetch,
	DEFAULT_FETCH_TIMEOUT_MS: 30_000,
	LONG_FETCH_TIMEOUT_MS: 600_000
}));

// The memory worker's choice contract, exercised through the real modal and
// response adapter. HTTP is mocked; this is component evidence, not device QA.
const request: HitlRequest = {
	id: 'memory-connection-eval',
	source: 'user_request',
	input_type: 'choice',
	prompt: 'Which address should be used for this parcel?\n\nYour memories give two different addresses.',
	schema: { options: [
		{ id: 'acknowledge', label: 'Got it', requires_input: false },
		{ id: 'remember', label: 'Remember my clarification', requires_input: true },
		{ id: 'dismiss', label: 'Dismiss', requires_input: false }
	] },
	scope: {},
	identifiers: { request_id: 'memory-connection-eval', correlation_id: 'memory-connection-eval' }
};

beforeEach(() => {
	timedFetch.mockReset();
	timedFetch.mockResolvedValue(new Response(JSON.stringify({ status: 'accepted' }), { status: 200 }));
});
afterEach(() => { resolveAttentionPrompt(null); cleanup(); });

describe('memory connection clarification', () => {
	it('shows the evidence, requires owner words for Remember, and posts those exact words', async () => {
		render(AttentionPromptModal);
		const answer = respondToHitl(request, { 'x-principal': 'memory-eval', 'x-workspace': 'fixture' });
		expect(await screen.findByText(/Your memories give two different addresses/)).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('radio', { name: /Remember my clarification/ }));
		expect(screen.getByRole('button', { name: 'Submit' })).toBeDisabled();
		const words = 'Use the Bengaluru address for this parcel only.';
		await fireEvent.input(screen.getByRole('textbox', { name: 'Input for "Remember my clarification"' }), { target: { value: words } });
		await fireEvent.click(screen.getByRole('button', { name: 'Submit' }));
		expect(await answer).toEqual({ ok: true });
		const [url, init] = timedFetch.mock.calls[0];
		expect(url).toContain('/hitl/memory-connection-eval/respond');
		expect(JSON.parse(init.body)).toMatchObject({ source: 'user_request', input_type: 'choice',
			value: { type: 'choice', selected_id: 'remember', other_value: words } });
	});

	it.each([['Got it', 'acknowledge'], ['Dismiss', 'dismiss']])('answers %s without saving a clarification', async (label, id) => {
		render(AttentionPromptModal);
		const answer = respondToHitl(request);
		await fireEvent.click(await screen.findByRole('radio', { name: label }));
		expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Submit' }));
		expect(await answer).toEqual({ ok: true });
		expect(JSON.parse(timedFetch.mock.calls[0][1].body).value).toEqual({ type: 'choice', selected_id: id });
	});

	it('closing the prompt sends no answer', async () => {
		render(AttentionPromptModal);
		const answer = respondToHitl(request);
		await fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
		expect(await answer).toEqual({ ok: false, cancelled: true });
		expect(timedFetch).not.toHaveBeenCalled();
	});
});
