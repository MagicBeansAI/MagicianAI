import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	parseAttentionDeliveryImpressionReceipt,
	parseAttentionDeliveryPageResponse,
	parseAttentionDeliveryRefreshRequired,
	fetchAttentionDeliveryPage,
	postAttentionDeliveryImpression
} from './attentionDelivery';
import {
	deliveryPage,
	deliveryProjection,
	deliveryRoot
} from './attentionDelivery.testFixtures';

function clone<T>(value: T): T {
	return JSON.parse(JSON.stringify(value)) as T;
}

afterEach(() => vi.unstubAllGlobals());

describe('attention delivery protocol parser', () => {
	it('accepts a fully bound disabled-baseline first page', () => {
		const projection = deliveryProjection();
		const root = deliveryRoot(projection);
		const raw = deliveryPage({
			root,
			items: projection.lanes.follow_up.slice(0, 1),
			pageIndex: 0,
			pageStart: 0,
			pageSize: 1,
			cursor: null,
			nextCursor: 'opaque-page-1'
		});

		const parsed = parseAttentionDeliveryPageResponse(raw, 'follow_up', projection);

		expect(parsed?.root_decision.decision_id).toBe(root.decision_id);
		expect(parsed?.page.next_cursor).toBe('opaque-page-1');
		expect(parsed?.items.map((item) => item.candidate_id)).toEqual(['follow_up:ann-1']);
		expect(parsed?.health).toMatchObject({
			root_sample_count: 0,
			delivered_count: 1,
			remaining_count: 1,
			exact_revision_match: true,
			replay: false
		});
	});

	it('accepts a sampled canary root without conflating its policy with routing policy', () => {
		const projection = deliveryProjection();
		projection.status = 'succeeded';
		const root = {
			...deliveryRoot(projection),
			policy_snapshot_id: 'personal-bandit-snapshot-7',
			policy_model_version: 'personal-bandit-v2',
			posterior_version: 19,
			seed_identity: 'personal-bandit-seed'
		};
		const raw = deliveryPage({
			root,
			items: [...projection.lanes.follow_up].reverse(),
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});
		raw.status = 'succeeded';
		raw.fallback_reason = null;
		raw.items[0]!.root_policy_propensity = 0.37;
		raw.items[1]!.root_policy_propensity = 0.61;
		raw.health = {
			...raw.health,
			bandit_mode: 'canary',
			canary_assigned: true,
			applied: true,
			baseline_preserved: false,
			degradation_reason: null,
			root_sample_count: 1
		};

		const parsed = parseAttentionDeliveryPageResponse(raw, 'follow_up', projection);

		expect(parsed?.status).toBe('succeeded');
		expect(parsed?.root_decision.policy_snapshot_id).toBe('personal-bandit-snapshot-7');
		expect(parsed?.items.map((item) => item.candidate_id)).toEqual([
			'follow_up:ann-2',
			'follow_up:ann-1'
		]);
	});

	it('accepts the backend snapshot-missing degraded identity', () => {
		const projection = deliveryProjection();
		projection.status = 'succeeded';
		const root = {
			...deliveryRoot(projection),
			policy_snapshot_id: 'configured-but-missing',
			policy_model_version: null,
			seed_identity: 'baseline'
		};
		const raw = deliveryPage({
			root,
			items: projection.lanes.follow_up,
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});
		raw.fallback_reason = 'snapshot_missing_or_invalid';
		raw.health.bandit_mode = 'canary';
		raw.health.degradation_reason = 'snapshot_missing_or_invalid';

		expect(parseAttentionDeliveryPageResponse(raw, 'follow_up', projection)).not.toBeNull();
	});

	it('enforces the bounded fallback reason, mode, and root-sample matrix', () => {
		const projection = deliveryProjection();
		projection.status = 'succeeded';
		const root = deliveryRoot(projection);
		const raw = deliveryPage({
			root,
			items: projection.lanes.follow_up,
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});

		for (const mutate of [
			(value: typeof raw) => { value.health.root_sample_count = 2; },
			(value: typeof raw) => { value.fallback_reason = 'policy_shadow_only'; },
			(value: typeof raw) => {
				value.fallback_reason = 'snapshot_missing_or_invalid';
				value.health.degradation_reason = 'snapshot_missing_or_invalid';
			},
			(value: typeof raw) => {
				value.fallback_reason = 'policy_scope_not_canary';
				value.health.degradation_reason = 'policy_scope_not_canary';
				value.health.root_sample_count = 1;
			}
		]) {
			const drifted = clone(raw);
			mutate(drifted);
			expect(parseAttentionDeliveryPageResponse(drifted, 'follow_up', projection)).toBeNull();
		}
	});

	it('rejects projection, digest, order, count, replay, and propensity drift atomically', () => {
		const projection = deliveryProjection();
		const root = deliveryRoot(projection);
		const baseline = deliveryPage({
			root,
			items: projection.lanes.follow_up.slice(0, 1),
			pageIndex: 0,
			pageStart: 0,
			pageSize: 1,
			cursor: null,
			nextCursor: 'opaque-page-1'
		});
		const mutations: Array<(raw: ReturnType<typeof deliveryPage>) => void> = [
			(raw) => { raw.root_decision.projection_id = 'other-projection'; },
			(raw) => { raw.root_decision.universe_digest = 'other-digest'; },
			(raw) => { raw.items[0]!.position = 2; },
			(raw) => { raw.health.delivered_count = 2; },
			(raw) => { raw.health.replay = true; },
			(raw) => { raw.items[0]!.root_policy_propensity = 0.5; },
			(raw) => { raw.items[0]!.conditional_delivery_propensity = 0.5 as 1; }
		];

		for (const mutate of mutations) {
			const raw = clone(baseline);
			mutate(raw);
			expect(parseAttentionDeliveryPageResponse(raw, 'follow_up', projection)).toBeNull();
		}
	});

	it('accepts only the typed lane-bound refresh-required envelope', () => {
		const raw = {
			schema_version: 1,
			status: 'refresh_required',
			error: 'attention_delivery_refresh_required',
			reason: 'revision_drift',
			lane: 'follow_up',
			refresh_href: '/api/magician/v2/channel-assist/attention-learning/canonical-deliveries/follow_up'
		};

		expect(parseAttentionDeliveryRefreshRequired(raw, 'follow_up')?.reason).toBe('revision_drift');
		expect(parseAttentionDeliveryRefreshRequired({ ...raw, lane: 'worth_a_look' }, 'follow_up')).toBeNull();
		expect(parseAttentionDeliveryRefreshRequired({ ...raw, extra: true }, 'follow_up')).toBeNull();
	});

	it('keeps root-policy and conditional-delivery propensity distinct in receipts', () => {
		const receipt = parseAttentionDeliveryImpressionReceipt({
			impression_id: 'impression-1',
			event_id: 'event-1',
			decision_id: 'root-decision-1',
			candidate_id: 'follow_up:ann-1',
			source_revision: 'revision-1',
			surface: 'follow_up',
			accumulated_visible_ms: 750,
			min_visible_ms: 750,
			visibility_rule_version: 'delivery-visible-v1',
			verified: true,
			deduplicated: false,
			delivery_id: 'delivery-0',
			page_index: 0,
			position: 1,
			exposure_token: 'exposure-1',
			root_policy_propensity: 0.4,
			conditional_delivery_propensity: 1
		});

		expect(receipt?.root_policy_propensity).toBe(0.4);
		expect(receipt?.conditional_delivery_propensity).toBe(1);
	});

	it('sends delivery reads with matching scoped query and headers', async () => {
		const projection = deliveryProjection();
		const root = deliveryRoot(projection);
		const page = deliveryPage({
			root,
			items: projection.lanes.follow_up,
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});
		const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
			new Response(JSON.stringify(page), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		await fetchAttentionDeliveryPage({
			lane: 'follow_up',
			scope: { principal: 'person / one', workspace: 'team & one' },
			projection,
			pageSize: 2
		});

		const [input, init] = fetchMock.mock.calls[0]!;
		const url = new URL(String(input), 'https://magician.test');
		expect(url.searchParams.has('principal')).toBe(false);
		expect(url.searchParams.has('workspace')).toBe(false);
		expect(url.searchParams.get('page_size')).toBe('2');
		const headers = new Headers(init?.headers);
		expect(headers.get('X-Principal')).toBeNull();
		expect(headers.get('X-Workspace')).toBeNull();
	});

	it('rejects a receipt whose root propensity differs from the delivered item', async () => {
		const request = {
			event_id: 'event-1', decision_id: 'root-decision-1', candidate_id: 'follow_up:ann-1',
			source_revision: 'revision-1', surface: 'follow_up' as const, visible_ms: 750,
			visibility_rule_version: 'delivery-visible-v1', client_type: 'web' as const,
			client_version: 'test', viewport_class: 'wide', delivery_id: 'delivery-0',
			page_index: 0, position: 1, exposure_token: 'exposure-1'
		};
		vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({
			impression_id: 'impression-1', ...request, accumulated_visible_ms: 750,
			min_visible_ms: 750, verified: true, deduplicated: false,
			root_policy_propensity: 0.7, conditional_delivery_propensity: 1,
			visible_ms: undefined, client_type: undefined, client_version: undefined,
			viewport_class: undefined
		}), { status: 200 })));

		const result = await postAttentionDeliveryImpression(
			request,
			{ principal: 'anonymous', workspace: 'default' },
			0.4
		);

		expect(result).toMatchObject({ ok: false, error: 'Malformed delivery impression receipt' });
	});
});
