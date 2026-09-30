import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import ChannelFollowUpGroupPanel from './ChannelFollowUpGroupPanel.svelte';
import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';

const groupingPage = {
	mode: 'enforced' as const,
	snapshot_id: 'group-snapshot-1',
	generation: 7,
	scope: 'eligible_universe' as const,
	health: {
		candidate_total: 2,
		member_total: 2,
		cluster_total: 1,
		representative_total: 1,
		collapsed_member_total: 1,
		scored_pair_total: 1,
		cannot_link_total: 0,
		fallback_ungrouped_total: 0,
		totals_reconcile: true
	}
};

function followUp(
	id: string,
	subject: string,
	isRepresentative: boolean,
	revision: string
): ChannelFollowUp {
	return {
		annotation_id: id,
		candidate_id: id,
		source_revision: revision,
		provider: 'gmail',
		account_alias: 'personal',
		account_email: 'owner@example.test',
		thread_id: `thread-${id}`,
		lane: 'user_assist',
		label: 'needs_reply',
		confidence: 0.9,
		reason: null,
		proposed_action: null,
		subject,
		sender: `${subject} sender`,
		summary: `${subject} summary`,
		evidence_message_id: `message-${id}`,
		evidence_message_at: 1,
		created_at: 1,
		received_at: isRepresentative ? 2 : 1,
		open_url: `https://mail.example/${id}`,
		baseline_rank: isRepresentative ? 1 : 2,
		learned_rank: isRepresentative ? 1 : 2,
		rank_delta: 0,
		learning_score: 0.8,
		grouping: {
			cluster_id: 'cluster-1',
			representative_id: 'ann-1',
			is_representative: isRepresentative,
			member_count: 2,
			related_count: 1,
			model_version: 'pair-gbt-v1',
			snapshot_id: 'group-snapshot-1',
			merge_probability: isRepresentative ? null : 0.98
		},
		grouping_page: groupingPage
	};
}

function json(body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status: 200,
		headers: { 'Content-Type': 'application/json' }
	});
}

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('ChannelFollowUpGroupPanel', () => {
	it('expands every member with lifecycle metadata and writes an exact cannot-link', async () => {
		const representative = followUp('ann-1', 'Update A', true, 'distill:rev-1');
		const related = followUp('ann-2', 'Update B', false, 'distill:rev-2');
		const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = String(input);
			if (url.includes('/follow-ups/groups/cluster-1/members')) {
				return json({ cluster: representative.grouping, items: [representative, related], total: 2 });
			}
			if (url.endsWith('/attention-learning/pair-corrections')) {
				return json({
					pair_label_id: 'pair-1',
					inserted: true,
					label: 'not_duplicate',
					canonical_left_id: 'ann-1',
					canonical_right_id: 'ann-2',
					affected_cluster_ids: ['cluster-1', 'cluster-2'],
					grouping_generation: 8,
					recomputed: true
				});
			}
			throw new Error(`unexpected request ${url}: ${String(init?.body)}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(ChannelFollowUpGroupPanel, { followUp: representative });

		await fireEvent.click(screen.getByRole('button', { name: /1 related update/ }));
		expect(await screen.findByText('All 2 members loaded.')).toBeInTheDocument();
		expect(screen.getAllByText('Source Gmail · personal')).toHaveLength(2);
		expect(screen.getAllByText('Route You · Needs reply')).toHaveLength(2);
		expect(screen.getByText('Rank 2 baseline → 2 learned')).toBeInTheDocument();
		expect(screen.getAllByRole('button', { name: 'Useful' })).toHaveLength(2);

		await fireEvent.click(
			screen.getByRole('button', {
				name: 'Not duplicate: Update B versus representative'
			})
		);
		expect(await screen.findByText(/Pair correction recorded · generation 8/)).toBeInTheDocument();
		const correctionCall = fetchMock.mock.calls.find(([url]) =>
			String(url).endsWith('/attention-learning/pair-corrections')
		);
		expect(JSON.parse(String(correctionCall?.[1]?.body))).toEqual({
			event_id: expect.any(String),
			surface: 'follow_up',
			left: { candidate_id: 'ann-1', source_revision: 'distill:rev-1' },
			right: { candidate_id: 'ann-2', source_revision: 'distill:rev-2' },
			label: 'not_duplicate'
		});
	});

	it('labels shadow grouping as a preview without claiming cards were collapsed', () => {
		const representative = followUp('ann-1', 'Update A', true, 'distill:rev-1');
		representative.grouping_page = { ...groupingPage, mode: 'shadow' };
		render(ChannelFollowUpGroupPanel, { followUp: representative });
		expect(screen.getByText(/Grouping preview · 1 related update/)).toBeInTheDocument();
		expect(screen.getByText(/current cards remain separate/)).toBeInTheDocument();
		expect(screen.queryByText(/representative shown/)).not.toBeInTheDocument();
	});
});
