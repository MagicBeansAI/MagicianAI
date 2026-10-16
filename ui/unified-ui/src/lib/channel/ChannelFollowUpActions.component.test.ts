import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

import ChannelFollowUpActions from './ChannelFollowUpActions.svelte';
import {
	followUpAttentionMutationKey,
	optimisticAttentionMutationQueue
} from '$lib/attention/optimisticAttentionMutationQueue';
import { notifications } from '$lib/shared/stores/notifications';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';
import OptimisticChannelFollowUpHarness from '../../test/fixtures/OptimisticChannelFollowUpHarness.svelte';

const DEFAULT_SCOPE = { principal: 'anonymous', workspace: 'default' };

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((next) => (resolve = next));
	return { promise, resolve };
}

function followUp(overrides: Partial<ChannelFollowUp> = {}): ChannelFollowUp {
	return {
		annotation_id: 'ann-1',
		provider: 'gmail',
		account_alias: 'personal',
		account_email: 'me@example.test',
		thread_id: 'thread-1',
		lane: 'user_assist',
		label: 'needs_reply',
		confidence: 0.9,
		reason: null,
		proposed_action: null,
		subject: 'Test subject',
		sender: 'Someone',
		summary: 'A short summary',
		evidence_message_id: null,
		evidence_message_at: null,
		created_at: 0,
		received_at: null,
		open_url: null,
		...overrides
	};
}

afterEach(() => {
	cleanup();
	optimisticAttentionMutationQueue.reset();
	scopeIdentityStore.reset();
	notifications.clear();
	vi.unstubAllGlobals();
});

describe('ChannelFollowUpActions dropdown dismissal', () => {
	it('closes the Dismiss-reason menu when clicking elsewhere on the page', async () => {
		render(ChannelFollowUpActions, { followUp: followUp() });

		await fireEvent.click(screen.getByRole('button', { name: 'Dismiss with a reason' }));
		expect(await screen.findByRole('menuitem', { name: 'Spam / junk' })).toBeInTheDocument();

		await fireEvent.click(document.body);
		await waitFor(() =>
			expect(screen.queryByRole('menuitem', { name: 'Spam / junk' })).not.toBeInTheDocument()
		);
	});

	it('closes the Message-options menu when clicking elsewhere on the page', async () => {
		render(ChannelFollowUpActions, { followUp: followUp() });

		await fireEvent.click(screen.getByRole('button', { name: 'Message options' }));
		expect(await screen.findByRole('menuitem', { name: 'Writing style' })).toBeInTheDocument();

		await fireEvent.click(document.body);
		await waitFor(() =>
			expect(screen.queryByRole('menuitem', { name: 'Writing style' })).not.toBeInTheDocument()
		);
	});

	it('posts a typed dismissal reason and exposes learned-versus-baseline rank evidence', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(
				JSON.stringify({
					feedback_receipt: {
						outcome_id: 'outcome-1',
						outcome: 'irrelevant',
						surface: 'follow_up',
						feedback_recorded: true,
						affected_candidates: 12,
						rescore_status: 'completed'
					}
				}),
				{ status: 200, headers: { 'Content-Type': 'application/json' } }
			)
		);
		vi.stubGlobal('fetch', fetchMock);
		render(ChannelFollowUpActions, {
			followUp: followUp({
				baseline_rank: 3,
				learned_rank: 15,
				rank_delta: -12,
				learning_score: 0.18,
				semantic_ranking_enabled: true,
				actionability: {
					probability: 0.84,
					explanation: { code: 'direct_owner_request', label: 'Direct request to you' },
					model_version: 'actionability-gbt-v1',
					snapshot_id: 'snapshot-42',
					semantic_feature_status: 'succeeded',
					score_status: 'scored',
					mode: 'shadow'
				}
			})
		});

		expect(screen.getByTestId('follow-up-rank-delta')).toHaveTextContent(
			'Learned rank 15 · down 12 from baseline'
		);
		expect(screen.getByTestId('attention-actionability')).toHaveTextContent(
			'Actionability preview 84% · Direct request to you'
		);
		expect(screen.getByTestId('attention-actionability')).not.toHaveClass(
			'attention-actionability--active'
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Dismiss with a reason' }));
		await fireEvent.click(screen.getByRole('menuitem', { name: 'Spam / junk' }));
		await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
		expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({ reason: 'spam' });
	});

	it('suppresses the owning fallback row before the API settles and restores it on failure', async () => {
		const response = deferred<Response>();
		vi.stubGlobal('fetch', vi.fn(() => response.promise));
		render(OptimisticChannelFollowUpHarness, { followUps: [followUp()] });

		await fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
		expect(screen.queryByRole('article', { name: 'Test subject' })).not.toBeInTheDocument();
		expect(
			optimisticAttentionMutationQueue.status(followUpAttentionMutationKey('ann-1', DEFAULT_SCOPE))
		).toBe('pending');

		response.resolve(new Response(JSON.stringify({ error: 'write failed' }), { status: 503 }));

		await waitFor(() => expect(screen.getByRole('article', { name: 'Test subject' })).toBeInTheDocument());
		expect(
			optimisticAttentionMutationQueue.status(followUpAttentionMutationKey('ann-1', DEFAULT_SCOPE))
		).toBeNull();
		expect(get(notifications).notifications).toMatchObject([
			{ type: 'error', title: 'Message action failed', message: 'write failed' }
		]);
	});
});
