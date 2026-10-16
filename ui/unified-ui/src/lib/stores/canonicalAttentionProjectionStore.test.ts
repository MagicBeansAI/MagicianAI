import { get } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { canonicalAttentionProjectionStore } from './canonicalAttentionProjectionStore';
import { scopeIdentityStore } from './scopeIdentityStore';

function validProjection() {
	return {
		schema_version: 1,
		status: 'baseline_fallback',
		projection_id: 'projection-1',
		universe_digest: 'digest-1',
		created_at: 1,
		policy: { mode: 'baseline', snapshot_id: null, model_version: null, seed_identity: 'p:w', canary_fraction: 0 },
		integrity: {
			load_complete: true, exact_once: true, source_total: 1,
			follow_up_source_total: 1, worth_a_look_source_total: 0,
			reconciled_total: 1, grouped_member_total: 1, materialized_total: 1,
			follow_up_lane_total: 1, worth_a_look_lane_total: 0, non_surfaced_total: 0,
			duplicate_hidden_total: 0, unmatched_total: 0, fallback_reason: 'disabled'
		},
		duplicate_aliases: [],
		cross_lane_reconciliation: {
			schema_version: 1, status: 'succeeded', reason: null, authoritative_lane: 'follow_up',
			principal: 'p', workspace: 'w', follow_up_source_total: 1,
			worth_a_look_source_total: 0, raw_source_total: 1, unique_source_total: 1,
			duplicate_hidden_total: 0, alias_record_total: 0, alias_records_returned: 0,
			aliases_truncated: false, reconciliation_digest: 'reconciliation-1'
		},
		lanes: {
			follow_up: [{
				canonical_id: 'follow_up:ann-1', source_revision: null, origin_lane: 'follow_up', served_lane: 'follow_up', learned_lane: 'follow_up',
				route_reason: 'baseline', route_applied: false,
				origin: { kind: 'follow_up', annotation_id: 'ann-1', provider: 'gmail', account_alias: 'p', thread_id: 't' },
				group: { cluster_id: 'c-1', representative_id: 'follow_up:ann-1', member_ids: ['follow_up:ann-1'], member_count: 1 },
				actions: [
					{ id: 'open_source', kind: 'open_source', label: 'Open', method: 'get', href: '/api/magician/v2/channel-assist/annotations/ann-1/message', requires_confirmation: false },
					{ id: 'approve', kind: 'approve', label: 'Approve', method: 'post', href: '/api/magician/v2/channel-assist/annotations/ann-1/approve', requires_confirmation: true },
					{ id: 'acknowledge', kind: 'acknowledge', label: 'Acknowledge', method: 'post', href: '/api/magician/v2/channel-assist/annotations/ann-1/acknowledge', requires_confirmation: false },
					{ id: 'useful', kind: 'useful', label: 'Useful', method: 'post', href: '/api/magician/v2/channel-assist/annotations/ann-1/useful', requires_confirmation: false },
					{ id: 'dismiss', kind: 'dismiss', label: 'Dismiss', method: 'post', href: '/api/magician/v2/channel-assist/annotations/ann-1/dismiss', requires_confirmation: true },
					{ id: 'snooze', kind: 'snooze', label: 'Snooze', method: 'post', href: '/api/magician/v2/channel-assist/annotations/ann-1/snooze', requires_confirmation: false }
				],
				payload: { kind: 'follow_up', annotation_id: 'ann-1', subject: null, sender: null, summary: null, label: null, reason: null, received_at: null, open_url: null }
			}],
			worth_a_look: [],
			non_surfaced: []
		}
	};
}

afterEach(() => {
	canonicalAttentionProjectionStore.clear();
	scopeIdentityStore.reset();
	vi.unstubAllGlobals();
});

describe('canonical attention projection store', () => {
	it('coalesces overlapping same-scope refreshes into one cancellable request', async () => {
		scopeIdentityStore.observe('p', 'w');
		let resolveRequest!: (response: Response) => void;
		const request = new Promise<Response>((resolve) => (resolveRequest = resolve));
		const fetchMock = vi.fn().mockReturnValue(request);
		vi.stubGlobal('fetch', fetchMock);

		const first = canonicalAttentionProjectionStore.refresh('p:w');
		const second = canonicalAttentionProjectionStore.refresh('p:w');
		expect(second).toBe(first);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		resolveRequest(new Response(JSON.stringify({
			canonical_attention_projection: validProjection()
		}), { status: 200 }));
		await Promise.all([first, second]);
		expect(get(canonicalAttentionProjectionStore).projection?.projection_id).toBe('projection-1');
	});

	it('aborts an owned request when the store is cleared', async () => {
		scopeIdentityStore.observe('p', 'w');
		let observedSignal: AbortSignal | undefined;
		vi.stubGlobal('fetch', vi.fn((_url: string, init?: RequestInit) => {
			observedSignal = init?.signal ?? undefined;
			return new Promise<Response>(() => undefined);
		}));
		void canonicalAttentionProjectionStore.refresh('p:w');
		await Promise.resolve();
		canonicalAttentionProjectionStore.clear();
		expect(observedSignal?.aborted).toBe(true);
		expect(get(canonicalAttentionProjectionStore)).toEqual({
			projection: null,
			isLoading: false,
			fallbackReason: null,
			loadedScopeKey: null
		});
	});

	it('publishes both lanes together after one valid complete-universe read', async () => {
		scopeIdentityStore.observe('p', 'w');
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
			canonical_attention_projection: validProjection()
		}), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		await canonicalAttentionProjectionStore.refresh('p:w');
		const state = get(canonicalAttentionProjectionStore);
		expect(state.projection?.projection_id).toBe('projection-1');
		expect(state.fallbackReason).toBeNull();
		expect(String(fetchMock.mock.calls[0][0])).not.toContain('principal=');
		const headers = new Headers(fetchMock.mock.calls[0][1]?.headers);
		expect(headers.get('X-Principal')).toBeNull();
		expect(headers.get('X-Workspace')).toBeNull();
	});

	it('retains a prior same-scope union atomically when the next envelope is incomplete', async () => {
		scopeIdentityStore.observe('p', 'w');
		const fetchMock = vi.fn()
			.mockResolvedValueOnce(new Response(JSON.stringify({ canonical_attention_projection: validProjection() }), { status: 200 }))
			.mockResolvedValueOnce(new Response(JSON.stringify({ canonical_attention_projection: { schema_version: 1 } }), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		await canonicalAttentionProjectionStore.refresh('p:w');
		expect(get(canonicalAttentionProjectionStore).projection).not.toBeNull();
		await canonicalAttentionProjectionStore.refresh('p:w');
		const state = get(canonicalAttentionProjectionStore);
		expect(state.projection?.projection_id).toBe('projection-1');
		expect(state.fallbackReason).not.toBeNull();
	});

	it('does not publish a late response from a superseded scope', async () => {
		let resolveFirst!: (response: Response) => void;
		const first = new Promise<Response>((resolve) => (resolveFirst = resolve));
		vi.stubGlobal('fetch', vi.fn()
			.mockReturnValueOnce(first)
			.mockResolvedValueOnce(new Response('{}', { status: 503 })));
		scopeIdentityStore.observe('old', 'scope');
		const stale = canonicalAttentionProjectionStore.refresh('old:scope');
		scopeIdentityStore.observe('new', 'scope');
		await canonicalAttentionProjectionStore.refresh('new:scope');
		resolveFirst(new Response(JSON.stringify({ canonical_attention_projection: validProjection() }), { status: 200 }));
		await stale;
		expect(get(canonicalAttentionProjectionStore)).toMatchObject({
			projection: null,
			loadedScopeKey: 'new:scope'
		});
	});

	it('keeps a typed incomplete fallback out of both lanes and exposes its stable reason', async () => {
		scopeIdentityStore.observe('p', 'w');
		const fallback = validProjection();
		fallback.status = 'baseline_fallback';
		fallback.integrity.load_complete = false;
		fallback.integrity.exact_once = false;
		fallback.integrity.source_total = 0;
		fallback.integrity.follow_up_source_total = 0;
		fallback.integrity.reconciled_total = 0;
		fallback.integrity.grouped_member_total = 0;
		fallback.integrity.materialized_total = 0;
		fallback.integrity.follow_up_lane_total = 0;
		fallback.integrity.fallback_reason = 'canonical_source_totals_changed';
		fallback.lanes.follow_up = [];
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
			canonical_attention_projection: fallback
		}), { status: 200 })));

		await canonicalAttentionProjectionStore.refresh('p:w');
		expect(get(canonicalAttentionProjectionStore)).toMatchObject({
			projection: null,
			fallbackReason: 'canonical_source_totals_changed',
			loadedScopeKey: 'p:w'
		});
	});
});
