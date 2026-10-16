import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import ResurfacingGroupPanel from './ResurfacingGroupPanel.svelte';
import type { ResurfacingCard } from './resurfacingQueries';

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

function card(id: string, line: string, isRepresentative: boolean, revision: string): ResurfacingCard {
	return {
		candidate_id: id,
		line,
		why_now: `${line} changed`,
		source_title: `${line} source`,
		summary: `${line} summary`,
		source_kind: isRepresentative ? 'comm' : 'web',
		source_ref: `source:${id}`,
		source_revision: revision,
		source_route: `route:${id}`,
		open_url: `https://example.test/${id}`,
		detail_label: 'Summary',
		temporal_anchor_at: isRepresentative ? 2 : 1,
		brief: null,
		brief_status: 'legacy',
		content_revision: revision,
		source_updated: false,
		recommended_action: null,
		actions: [],
		baseline_rank: isRepresentative ? 1 : 2,
		learned_rank: isRepresentative ? 1 : 2,
		rank_delta: 0,
		learning_score: 0.8,
		grouping: {
			cluster_id: 'cluster-1',
			representative_id: 'cand-1',
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

describe('ResurfacingGroupPanel', () => {
	it('expands all members and posts revision-bound positive pair evidence', async () => {
		const representative = card('cand-1', 'Policy update A', true, 'rev-1');
		const related = card('cand-2', 'Policy update B', false, 'rev-2');
		const fetchMock = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
			const url = String(input);
			if (url.includes('/resurfacing/groups/cluster-1/members')) {
				return json({
					cluster: representative.grouping,
					items: [representative, related],
					total: 2,
					cross_lane_reconciliation: {
						schema_version: 1,
						status: 'succeeded',
						reason: null,
						authoritative_lane: 'follow_up',
						principal: 'anonymous',
						workspace: 'default',
						follow_up_source_total: 1,
						worth_a_look_source_total: 2,
						raw_source_total: 2,
						visible_source_total: 2,
						duplicate_hidden_total: 0,
						raw_scanned_total: 2,
						visible_page_total: 2,
						duplicate_hidden_page_total: 0,
						reconciliation_digest: 'reconciliation-1'
					}
				});
			}
			if (url.endsWith('/attention-learning/pair-corrections')) {
				return json({
					pair_label_id: 'pair-1',
					inserted: true,
					label: 'same_underlying_item',
					canonical_left_id: 'cand-1',
					canonical_right_id: 'cand-2',
					affected_cluster_ids: ['cluster-1'],
					grouping_generation: 8,
					recomputed: true
				});
			}
			throw new Error(`unexpected request ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(ResurfacingGroupPanel, { card: representative });

		await fireEvent.click(screen.getByRole('button', { name: /1 related update/ }));
		expect(await screen.findByText('All 2 members loaded.')).toBeInTheDocument();
		expect(screen.getByText('Source web · source:cand-2')).toBeInTheDocument();
		expect(screen.getByText('Route route:cand-2')).toBeInTheDocument();
		expect(screen.getByText('Rank 2 baseline → 2 learned')).toBeInTheDocument();
		expect(screen.getAllByRole('button', { name: 'Dismiss' })).toHaveLength(2);

		await fireEvent.click(
			screen.getByRole('button', {
				name: 'Duplicate of this: Policy update B versus representative'
			})
		);
		expect(await screen.findByText(/Pair correction recorded · generation 8/)).toBeInTheDocument();
		const correctionCall = fetchMock.mock.calls.find(([url]) =>
			String(url).endsWith('/attention-learning/pair-corrections')
		);
		expect(JSON.parse(String(correctionCall?.[1]?.body))).toEqual({
			event_id: expect.any(String),
			surface: 'worth_a_look',
			left: { candidate_id: 'cand-1', source_revision: 'rev-1' },
			right: { candidate_id: 'cand-2', source_revision: 'rev-2' },
			label: 'same_underlying_item'
		});
	});

	it('keeps shadow grouping diagnostic-only', () => {
		const representative = card('cand-1', 'Policy update A', true, 'rev-1');
		representative.grouping_page = { ...groupingPage, mode: 'shadow' };
		render(ResurfacingGroupPanel, { card: representative });
		expect(screen.getByText(/Grouping preview · 1 related update/)).toBeInTheDocument();
		expect(screen.getByText(/current cards remain separate/)).toBeInTheDocument();
	});
});
