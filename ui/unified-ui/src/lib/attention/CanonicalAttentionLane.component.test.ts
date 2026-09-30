import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import CanonicalAttentionLane from './CanonicalAttentionLane.svelte';
import CanonicalProjectionDiagnostic from './CanonicalProjectionDiagnostic.svelte';
import { ATTENTION_DELIVERY_ENDPOINT } from './attentionDelivery';
import {
	deliveryFollowUpItem,
	deliveryPage,
	deliveryProjection,
	deliveryRoot
} from './attentionDelivery.testFixtures';
import type { CanonicalAttentionProjection } from './canonicalAttentionProjection';
import type { ResurfacingCard } from '$lib/today/resurfacingQueries';
import { attentionDeliveryStore } from '$lib/stores/attentionDeliveryStore';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import {
	followUpAttentionMutationKey,
	optimisticAttentionMutationQueue
} from './optimisticAttentionMutationQueue';

const DEFAULT_SCOPE = { principal: 'anonymous', workspace: 'default' };

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((next) => (resolve = next));
	return { promise, resolve };
}

function projection(): CanonicalAttentionProjection {
	const follow = {
		canonical_id: 'follow_up:ann-1', source_revision: null, origin_lane: 'follow_up' as const,
		served_lane: 'follow_up' as const, learned_lane: 'follow_up' as const,
		route_reason: 'baseline', route_applied: false,
		origin: { kind: 'follow_up' as const, annotation_id: 'ann-1', provider: 'gmail', account_alias: 'p', thread_id: 't' },
		group: { cluster_id: 'c-ann', representative_id: 'follow_up:ann-1', member_ids: ['follow_up:ann-1'], member_count: 1 },
		actions: [{ id: 'dismiss', kind: 'dismiss' as const, label: 'Dismiss message', method: 'post' as const, href: '/api/magician/v2/channel-assist/annotations/ann-1/dismiss', requires_confirmation: true }],
		payload: { kind: 'follow_up' as const, annotation_id: 'ann-1', subject: 'First server item', sender: null, summary: 'Message summary', label: 'needs_reply', reason: null, received_at: 2, due_text: null, due_at: null, open_url: null }
	};
	const worth = {
		canonical_id: 'worth_a_look:worth-1', source_revision: null, origin_lane: 'worth_a_look' as const,
		served_lane: 'follow_up' as const, learned_lane: 'follow_up' as const,
		route_reason: 'learned_route_applied', route_applied: true,
		origin: { kind: 'worth_a_look' as const, candidate_id: 'worth-1', source_kind: 'web', source_ref: 'https://example.test' },
		group: { cluster_id: 'c-worth', representative_id: 'worth_a_look:worth-1', member_ids: ['worth_a_look:worth-1'], member_count: 1 },
		actions: [{ id: 'useful', kind: 'useful' as const, label: 'Useful', method: 'post' as const, href: '/api/magician/v2/channel-assist/resurfacing/worth-1/action', requires_confirmation: false }],
		payload: { kind: 'worth_a_look' as const, candidate_id: 'worth-1', line: 'Second server item', why_now: 'Due soon', summary: 'Worth summary', source_title: 'Source', source_kind: 'web', source_ref: 'https://example.test', open_url: 'https://example.test', temporal_anchor_at: 1, brief: null }
	};
	return {
		schema_version: 1, status: 'succeeded', projection_id: 'projection-1', universe_digest: 'digest', source_generation_token: null, created_at: 3,
		policy: { mode: 'canary', snapshot_id: 'route-1', model_version: 'model-1', seed_identity: 'p:w', canary_fraction: 0.1 },
		integrity: { load_complete: true, exact_once: true, source_total: 2, follow_up_source_total: 1, worth_a_look_source_total: 1, reconciled_total: 2, grouped_member_total: 2, materialized_total: 2, follow_up_lane_total: 2, worth_a_look_lane_total: 0, non_surfaced_total: 0, duplicate_hidden_total: 0, unmatched_total: 0, fallback_reason: null },
		duplicate_aliases: [],
		diagnostics: null,
		cross_lane_reconciliation: { schema_version: 1, status: 'succeeded', reason: null, authoritative_lane: 'follow_up', principal: 'p', workspace: 'w', follow_up_source_total: 1, worth_a_look_source_total: 1, raw_source_total: 2, unique_source_total: 2, duplicate_hidden_total: 0, alias_record_total: 0, alias_records_returned: 0, aliases_truncated: false, reconciliation_digest: 'reconciliation-1' },
		lanes: { follow_up: [follow, worth], worth_a_look: [], non_surfaced: [] }
	};
}

/** The rich Worth card the lane joins to `worth_a_look:worth-1` by candidate id. */
function worthCard(): ResurfacingCard {
	return {
		candidate_id: 'worth-1',
		line: 'Second server item',
		why_now: 'Due soon',
		source_title: 'Source',
		summary: 'Worth summary',
		source_kind: 'web',
		source_ref: 'https://example.test',
		open_url: 'https://example.test',
		detail_label: 'Details',
		temporal_anchor_at: 1,
		brief: null,
		// 'legacy', not 'ready': the union is 'v2' | 'legacy', and the parser
		// coerces anything that is not 'v2' to 'legacy' — so a fixture saying
		// 'ready' was exercising a shape the app can never receive.
		brief_status: 'legacy',
		content_revision: '1',
		source_updated: false,
		recommended_action: null,
		actions: [
			{ kind: 'create_task', label: 'Create task', requires_input: true, side_effect: 'creates_task' }
		]
	};
}

afterEach(() => {
	cleanup();
	attentionDeliveryStore.clear();
	optimisticAttentionMutationQueue.reset();
	scopeIdentityStore.reset();
	vi.unstubAllGlobals();
});

describe('CanonicalAttentionLane', () => {
	it('renders either origin in destination order with one projection diagnostic', async () => {
		render(CanonicalAttentionLane, { projection: projection(), lane: 'follow_up' });
		const list = await screen.findByRole('list', { name: 'Follow-ups' });
		const rows = within(list).getAllByRole('listitem');
		expect(rows).toHaveLength(2);
		expect(rows[0]).toHaveTextContent('First server item');
		expect(rows[1]).toHaveTextContent('Second server item');
		expect(rows[1]).toHaveTextContent('From Worth a look');
		expect(screen.getByText('Learned routing canary')).toBeInTheDocument();
		expect(screen.getByText('Exact union 2 unique/2 raw')).toBeInTheDocument();
	});

	it('does not dump the unpaged projection while delivery is still loading', async () => {
		const hung = deferred<Response>();
		vi.stubGlobal('fetch', vi.fn(() => hung.promise));
		render(CanonicalAttentionLane, { projection: projection(), lane: 'follow_up' });
		await waitFor(() => expect(screen.getByTestId('canonical-lane-loading')).toBeInTheDocument());
		expect(screen.queryByText('First server item')).not.toBeInTheDocument();
		hung.resolve(new Response(JSON.stringify({}), { status: 404 }));
	});

	it('prefers the joined Worth card why-now over the projection stub', async () => {
		const value = projection();
		const worth = value.lanes.follow_up[1];
		if (worth.payload.kind === 'worth_a_look') {
			worth.payload.why_now = 'Currently surfaced by the Worth-a-look curator';
		}
		render(CanonicalAttentionLane, {
			projection: value,
			lane: 'follow_up',
			worthCards: [{ ...worthCard(), why_now: 'you said Avoid vendor calls' }]
		});
		expect(await screen.findByText('you said Avoid vendor calls')).toBeInTheDocument();
		expect(
			screen.queryByText('Currently surfaced by the Worth-a-look curator')
		).not.toBeInTheDocument();
	});

	it('keeps the server verbs on a Worth row that also has its contextual menu', async () => {
		// The contextual menu is only the overflow control, so it has to sit
		// alongside the server-declared verbs rather than replace them. Rendering
		// it in their place drops Useful/Acknowledge/Dismiss and still looks like
		// a working row, which is why this asserts both halves are present.
		render(CanonicalAttentionLane, {
			projection: projection(),
			lane: 'follow_up',
			worthCards: [worthCard()]
		});
		const list = await screen.findByRole('list', { name: 'Follow-ups' });
		const worthRow = within(list).getAllByRole('listitem')[1];
		expect(within(worthRow).getByRole('button', { name: 'Useful' })).toBeInTheDocument();
		expect(within(worthRow).getByRole('button', { name: 'More actions' })).toBeInTheDocument();
	});

	it('posts a cross-routed Worth positive label only to the Worth lifecycle endpoint', async () => {
		const fetchMock = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) =>
			String(input).startsWith(ATTENTION_DELIVERY_ENDPOINT)
				? new Response(JSON.stringify({}), { status: 404 })
				: new Response(JSON.stringify({}), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		render(CanonicalAttentionLane, { projection: projection(), lane: 'follow_up' });

		await fireEvent.click(await screen.findByRole('button', { name: 'Useful' }));
		await waitFor(() => expect(fetchMock.mock.calls.some(([input]) =>
			String(input) === '/api/magician/v2/channel-assist/resurfacing/worth-1/action')).toBe(true));
		const actionCall = fetchMock.mock.calls.find(([input]) =>
			String(input) === '/api/magician/v2/channel-assist/resurfacing/worth-1/action');
		expect(JSON.parse(String(actionCall?.[1]?.body))).toEqual({ action: 'open' });
		expect(String(actionCall?.[0])).not.toContain('/annotations/');
	});

	it('loads decision-bound pages in position order without client sorting', async () => {
		const value = deliveryProjection([deliveryFollowUpItem(2), deliveryFollowUpItem(1)]);
		const root = deliveryRoot(value);
		const first = deliveryPage({
			root,
			items: value.lanes.follow_up.slice(0, 1),
			pageIndex: 0,
			pageStart: 0,
			pageSize: 1,
			cursor: null,
			nextCursor: 'opaque-page-1'
		});
		const second = deliveryPage({
			root,
			items: value.lanes.follow_up.slice(1),
			pageIndex: 1,
			pageStart: 1,
			pageSize: 1,
			cursor: 'opaque-page-1',
			nextCursor: null
		});
		const fetchMock = vi.fn(async (input: RequestInfo | URL) =>
			new Response(JSON.stringify(String(input).includes('cursor=opaque-page-1') ? second : first), {
				status: 200
			}));
		vi.stubGlobal('fetch', fetchMock);

		const view = render(CanonicalAttentionLane, {
			projection: value,
			lane: 'follow_up',
			page: 1,
			pageSize: 1
		});
		// A frozen delivery pages like any other lane rather than growing through a
		// Load-more button; the universe size is what makes the second page
		// reachable before its items have been fetched. `page` is owned by the
		// host, so seeking is driven the way the host drives it.
		await waitFor(() => expect(screen.getByText('Server item 2')).toBeInTheDocument());
		expect(screen.queryByText('Server item 1')).not.toBeInTheDocument();
		expect(screen.getAllByRole('button', { name: 'Next page' })[0]).toBeInTheDocument();

		await view.rerender({ projection: value, lane: 'follow_up', page: 2, pageSize: 1 });
		await waitFor(() => expect(screen.getByText('Server item 1').closest('li')).toHaveAttribute(
			'data-delivery-id',
			'delivery-1'
		));
		// Position order survives the seek: page 1 held position 1, page 2 holds
		// position 2, and neither was re-sorted client-side.
		const rows = within(screen.getByRole('list', { name: 'Follow-ups' })).getAllByRole('listitem');
		expect(rows.map((row) => row.textContent)).toEqual([
			expect.stringContaining('Server item 1')
		]);
		expect(fetchMock.mock.calls[1]?.[0]).toContain('cursor=opaque-page-1');
	});

	it('removes a decision-bound row before its queued API call settles', async () => {
		const value = deliveryProjection([deliveryFollowUpItem(1), deliveryFollowUpItem(2)]);
		const firstRoot = deliveryRoot(value);
		const firstPage = deliveryPage({
			root: firstRoot,
			items: value.lanes.follow_up,
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});
		const refreshed = deliveryProjection([deliveryFollowUpItem(2)]);
		refreshed.projection_id = 'projection-delivery-2';
		refreshed.universe_digest = 'universe-digest-2';
		const refreshedRoot = deliveryRoot(refreshed);
		const refreshedPage = deliveryPage({
			root: refreshedRoot,
			items: refreshed.lanes.follow_up,
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});
		let deliveryReads = 0;
		const actionResponse = deferred<Response>();
		const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
			if (String(input).startsWith(ATTENTION_DELIVERY_ENDPOINT)) {
				deliveryReads += 1;
				return new Response(JSON.stringify(deliveryReads === 1 ? firstPage : refreshedPage), {
					status: 200
				});
			}
			return actionResponse.promise;
		});
		vi.stubGlobal('fetch', fetchMock);
		const view = render(CanonicalAttentionLane, {
			projection: value,
			lane: 'follow_up',
			pageSize: 2
		});
		await waitFor(() => expect(screen.getByText('Server item 1').closest('li')).toHaveAttribute(
			'data-delivery-id',
			'delivery-0'
		));

		const firstRow = screen.getByText('Server item 1').closest('li')!;
		await fireEvent.click(within(firstRow).getByRole('button', { name: 'Useful' }));
		expect(screen.queryByText('Server item 1')).not.toBeInTheDocument();
		await waitFor(() => expect(fetchMock.mock.calls.some(([input]) =>
			String(input).endsWith('/annotations/ann-1/useful'))).toBe(true));
		actionResponse.resolve(new Response(JSON.stringify({}), { status: 200 }));
		await waitFor(() => expect(
			optimisticAttentionMutationQueue.status(followUpAttentionMutationKey('ann-1', DEFAULT_SCOPE))
		).toBe('committed'));
		expect(screen.queryByText('Server item 1')).not.toBeInTheDocument();

		await view.rerender({ projection: refreshed, lane: 'follow_up', pageSize: 2 });
		await waitFor(() => expect(screen.queryByText('Server item 1')).not.toBeInTheDocument());
		expect(screen.getByText('Server item 2')).toBeInTheDocument();
	});

	it('restores the exact decision-bound row when the queued API call fails', async () => {
		const value = deliveryProjection([deliveryFollowUpItem(1), deliveryFollowUpItem(2)]);
		const root = deliveryRoot(value);
		const page = deliveryPage({
			root,
			items: value.lanes.follow_up,
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});
		const actionResponse = deferred<Response>();
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) =>
			String(input).startsWith(ATTENTION_DELIVERY_ENDPOINT)
				? new Response(JSON.stringify(page), { status: 200 })
				: actionResponse.promise));

		render(CanonicalAttentionLane, { projection: value, lane: 'follow_up', pageSize: 2 });
		await waitFor(() => expect(screen.getByText('Server item 1')).toBeInTheDocument());
		const firstRow = screen.getByText('Server item 1').closest('li')!;
		await fireEvent.click(within(firstRow).getByRole('button', { name: 'Useful' }));
		expect(screen.queryByText('Server item 1')).not.toBeInTheDocument();

		actionResponse.resolve(new Response(JSON.stringify({ error: 'write failed' }), { status: 503 }));

		await waitFor(() => expect(screen.getByText('Server item 1')).toBeInTheDocument());
		const rows = within(screen.getByRole('list', { name: 'Follow-ups' })).getAllByRole('listitem');
		expect(rows.map((row) => row.textContent)).toEqual([
			expect.stringContaining('Server item 1'),
			expect.stringContaining('Server item 2')
		]);
		expect(
			optimisticAttentionMutationQueue.status(followUpAttentionMutationKey('ann-1', DEFAULT_SCOPE))
		).toBeNull();
	});

	it('reports canary-zero as inactive rather than implying learned activation', () => {
		const value = projection();
		value.policy.canary_fraction = 0;
		render(CanonicalProjectionDiagnostic, { projection: value });
		expect(screen.getByText('Learned routing inactive')).toBeInTheDocument();
	});

	it('shows duplicate aliases only as a compact diagnostic count', () => {
		const value = projection();
		value.integrity.duplicate_hidden_total = 1;
		value.integrity.source_total = 3;
		value.integrity.reconciled_total = 3;
		value.integrity.worth_a_look_source_total = 2;
		value.duplicate_aliases = [{
			owner_canonical_id: 'follow_up:ann-1',
			duplicate_canonical_id: 'worth_a_look:hidden-1',
			reason: 'exact_source_identity'
		}];
		Object.assign(value.cross_lane_reconciliation, {
			worth_a_look_source_total: 2,
			raw_source_total: 3,
			duplicate_hidden_total: 1,
			alias_record_total: 1,
			alias_records_returned: 1
		});
		render(CanonicalProjectionDiagnostic, { projection: value });
		expect(screen.getByTestId('canonical-duplicate-diagnostic')).toHaveTextContent('1 exact duplicate hidden');
		expect(screen.queryByText('worth_a_look:hidden-1')).not.toBeInTheDocument();
	});
});
