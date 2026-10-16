// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type {
	AttentionDeliveryImpressionRequest,
	AttentionDeliveryImpressionResult
} from './attentionDelivery';
import {
	deliveryPage,
	deliveryProjection,
	deliveryRoot
} from './attentionDelivery.testFixtures';
import {
	resetVerifiedAttentionDeliveryVisibilityForTests,
	verifiedAttentionDeliveryVisibility
} from './attentionDeliveryVisibility';

class FakeIntersectionObserver {
	static instances: FakeIntersectionObserver[] = [];
	readonly callback: IntersectionObserverCallback;
	readonly observe = vi.fn();
	readonly disconnect = vi.fn();

	constructor(callback: IntersectionObserverCallback) {
		this.callback = callback;
		FakeIntersectionObserver.instances.push(this);
	}

	trigger(isIntersecting: boolean, intersectionRatio: number): void {
		this.callback([{
			isIntersecting,
			intersectionRatio
		} as IntersectionObserverEntry], this as unknown as IntersectionObserver);
	}
}

function receipt(request: AttentionDeliveryImpressionRequest): AttentionDeliveryImpressionResult {
	return {
		ok: true,
		receipt: {
			impression_id: 'impression-1',
			event_id: request.event_id,
			decision_id: request.decision_id,
			candidate_id: request.candidate_id,
			source_revision: request.source_revision,
			surface: request.surface,
			accumulated_visible_ms: request.visible_ms,
			min_visible_ms: 750,
			visibility_rule_version: request.visibility_rule_version,
			verified: true,
			deduplicated: false,
			delivery_id: request.delivery_id,
			page_index: request.page_index,
			position: request.position,
			exposure_token: request.exposure_token,
			root_policy_propensity: 1,
			conditional_delivery_propensity: 1
		}
	};
}

function boundOptions(record?: (request: AttentionDeliveryImpressionRequest) => Promise<AttentionDeliveryImpressionResult>) {
	const projection = deliveryProjection();
	const root = deliveryRoot(projection);
	const response = deliveryPage({
		root,
		items: projection.lanes.follow_up.slice(0, 1),
		pageIndex: 0,
		pageStart: 0,
		pageSize: 1,
		cursor: null,
		nextCursor: 'opaque-next'
	});
	return {
		response,
		delivery: response.items[0]!,
		surface: 'follow_up' as const,
		scope: { principal: 'anonymous', workspace: 'default' },
		record
	};
}

beforeEach(() => {
	vi.useFakeTimers();
	FakeIntersectionObserver.instances = [];
	resetVerifiedAttentionDeliveryVisibilityForTests();
	vi.stubGlobal('IntersectionObserver', FakeIntersectionObserver as unknown as typeof IntersectionObserver);
});

afterEach(() => {
	resetVerifiedAttentionDeliveryVisibilityForTests();
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('verifiedAttentionDeliveryVisibility', () => {
	it('reports nothing on fetch or mount and records only after continuous verified dwell', async () => {
		const record = vi.fn(async (request: AttentionDeliveryImpressionRequest) => receipt(request));
		const node = document.createElement('li');
		const action = verifiedAttentionDeliveryVisibility(node, boundOptions(record));

		expect(record).not.toHaveBeenCalled();
		expect(node.dataset.attentionDeliveryId).toBe('delivery-0');
		expect(node.dataset.attentionExposureToken).toBe('exposure-1');
		FakeIntersectionObserver.instances[0]!.trigger(true, 0.5);
		await vi.advanceTimersByTimeAsync(400);
		FakeIntersectionObserver.instances[0]!.trigger(false, 0);
		await vi.advanceTimersByTimeAsync(750);
		expect(record).not.toHaveBeenCalled();

		FakeIntersectionObserver.instances[0]!.trigger(true, 0.5);
		await vi.advanceTimersByTimeAsync(750);

		expect(record).toHaveBeenCalledTimes(1);
		expect(record.mock.calls[0]?.[0]).toMatchObject({
			decision_id: 'root-decision-1',
			delivery_id: 'delivery-0',
			page_index: 0,
			position: 1,
			exposure_token: 'exposure-1',
			candidate_id: 'follow_up:ann-1',
			visible_ms: 750,
			visibility_rule_version: 'delivery-visible-v1'
		});
		expect(node.dataset.attentionDeliveryImpression).toBe('verified');
		action.destroy();
	});

	it('retries a transport failure with the identical event and delivery binding', async () => {
		const requests: AttentionDeliveryImpressionRequest[] = [];
		const record = vi.fn(async (request: AttentionDeliveryImpressionRequest) => {
			requests.push(request);
			return requests.length === 1
				? { ok: false as const, error: 'offline', code: null, retryable: true }
				: receipt(request);
		});
		const node = document.createElement('li');
		const action = verifiedAttentionDeliveryVisibility(node, boundOptions(record));

		FakeIntersectionObserver.instances[0]!.trigger(true, 0.5);
		await vi.advanceTimersByTimeAsync(750);
		await vi.advanceTimersByTimeAsync(500);

		expect(requests).toHaveLength(2);
		expect(requests[1]).toEqual(requests[0]);
		expect(requests[1]?.event_id).toBe(requests[0]?.event_id);
		expect(requests[1]?.exposure_token).toBe('exposure-1');
		action.destroy();
	});

	it('freezes the scoped transport with the visibility payload across retries', async () => {
		const calls: Array<[RequestInfo | URL, RequestInit | undefined]> = [];
		const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			calls.push([input, init]);
			if (calls.length === 1) {
				return new Response(JSON.stringify({ error: { message: 'offline', code: 'offline' } }), {
					status: 503
				});
			}
			const request = JSON.parse(String(init?.body)) as AttentionDeliveryImpressionRequest;
			const result = receipt(request);
			return new Response(JSON.stringify(result.ok ? result.receipt : null), {
				status: 200
			});
		});
		vi.stubGlobal('fetch', fetchMock);
		const options = boundOptions();
		const node = document.createElement('li');
		const action = verifiedAttentionDeliveryVisibility(node, options);

		FakeIntersectionObserver.instances[0]!.trigger(true, 0.5);
		options.scope.principal = 'changed-principal';
		options.scope.workspace = 'changed-workspace';
		await vi.advanceTimersByTimeAsync(750);
		await vi.advanceTimersByTimeAsync(500);

		expect(calls).toHaveLength(2);
		for (const [input, init] of calls) {
			const url = new URL(String(input), 'https://magician.test');
			expect(url.searchParams.has('principal')).toBe(false);
			expect(url.searchParams.has('workspace')).toBe(false);
			const headers = new Headers(init?.headers);
			expect(headers.get('X-Principal')).toBeNull();
			expect(headers.get('X-Workspace')).toBeNull();
		}
		expect(node.dataset.attentionDeliveryImpression).toBe('verified');
		action.destroy();
	});

	it('does not observe or report an item without an exact delivery binding', async () => {
		const record = vi.fn(async (request: AttentionDeliveryImpressionRequest) => receipt(request));
		const options = boundOptions(record);
		options.delivery = { ...options.delivery, exposure_token: '' };
		const action = verifiedAttentionDeliveryVisibility(document.createElement('li'), options);

		await vi.advanceTimersByTimeAsync(2_000);
		expect(FakeIntersectionObserver.instances).toHaveLength(0);
		expect(record).not.toHaveBeenCalled();
		action.destroy();
	});
});
