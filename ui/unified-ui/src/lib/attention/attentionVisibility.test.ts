// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type {
	AttentionDecisionItem,
	AttentionImpressionRequest,
	AttentionImpressionResult
} from './attentionRouting';
import {
	attentionFeedbackAttribution,
	resetVerifiedAttentionVisibilityForTests,
	verifiedAttentionVisibility
} from './attentionVisibility';

class FakeIntersectionObserver {
	static instances: FakeIntersectionObserver[] = [];
	readonly callback: IntersectionObserverCallback;
	disconnected = false;
	node: Element | null = null;

	constructor(callback: IntersectionObserverCallback) {
		this.callback = callback;
		FakeIntersectionObserver.instances.push(this);
	}

	observe(node: Element): void {
		this.node = node;
	}

	disconnect(): void {
		this.disconnected = true;
	}

	unobserve(): void {}
	takeRecords(): IntersectionObserverEntry[] { return []; }

	emit(isIntersecting: boolean, intersectionRatio: number): void {
		this.callback(
			[
				{
					isIntersecting,
					intersectionRatio,
					target: this.node
				} as IntersectionObserverEntry
			],
			this as unknown as IntersectionObserver
		);
	}
}

const decisionItem: AttentionDecisionItem = {
	decision_id: 'decision-1',
	candidate_id: 'candidate-1',
	source_revision: 'revision-1',
	baseline_route: 'follow_up',
	learned_route: 'follow_up',
	served_route: 'follow_up',
	routing_mode: 'baseline',
	routing_snapshot_id: null,
	routing_model_version: null,
	learned_route_confidence: null,
	utility_margin: null,
	route_reason: 'baseline_mode',
	served_rank: 1,
	selected: true,
	route_applied: false,
	canary_assigned: false
};

function verified(request: AttentionImpressionRequest, deduplicated = false): AttentionImpressionResult {
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
			min_visible_ms: 1_000,
			visibility_rule_version: request.visibility_rule_version,
			verified: true,
			deduplicated
		}
	};
}

beforeEach(() => {
	vi.useFakeTimers();
	vi.setSystemTime(new Date('2026-07-29T10:00:00Z'));
	FakeIntersectionObserver.instances = [];
	resetVerifiedAttentionVisibilityForTests();
	vi.stubGlobal('IntersectionObserver', FakeIntersectionObserver);
	Object.defineProperty(document, 'visibilityState', {
		configurable: true,
		value: 'visible'
	});
});

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('verifiedAttentionVisibility', () => {
	it('records only after one continuous configured dwell and sends no private text', async () => {
		const requests: AttentionImpressionRequest[] = [];
		const record = vi.fn(async (request: AttentionImpressionRequest) => {
			requests.push(request);
			return verified(request);
		});
		const node = document.createElement('article');
		const policy = { min_visible_ms: 1_000, visibility_rule_version: 'visible-50-v1' };
		verifiedAttentionVisibility(node, {
			decision_item: decisionItem,
			impression_policy: policy,
			surface: 'follow_up',
			record
		});
		expect(attentionFeedbackAttribution(decisionItem, policy, 'follow_up')).toEqual({
			decision_id: 'decision-1',
			candidate_id: 'candidate-1',
			source_revision: 'revision-1'
		});
		const observer = FakeIntersectionObserver.instances[0];

		observer.emit(true, 0.75);
		await vi.advanceTimersByTimeAsync(700);
		observer.emit(false, 0);
		await vi.advanceTimersByTimeAsync(2_000);
		expect(record).not.toHaveBeenCalled();

		observer.emit(true, 0.75);
		await vi.advanceTimersByTimeAsync(999);
		expect(record).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);

		expect(record).toHaveBeenCalledTimes(1);
		expect(requests[0]).toMatchObject({
			decision_id: 'decision-1',
			candidate_id: 'candidate-1',
			source_revision: 'revision-1',
			surface: 'follow_up',
			visible_ms: 1_000,
			visibility_rule_version: 'visible-50-v1',
			client_type: 'web'
		});
		expect(JSON.stringify(requests[0])).not.toContain('subject');
		expect(node.dataset.attentionImpression).toBe('verified');
		expect(attentionFeedbackAttribution(decisionItem, policy, 'follow_up')).toEqual({
			decision_id: 'decision-1',
			candidate_id: 'candidate-1',
			source_revision: 'revision-1',
			impression_id: 'impression-1'
		});
	});

	it('attributes feedback on an unselected same-surface card without recording an impression', async () => {
		const policy = { min_visible_ms: 1_000, visibility_rule_version: 'visible-50-v1' };
		const item = { ...decisionItem, selected: false, served_rank: 0 };
		const record = vi.fn(async (request: AttentionImpressionRequest) => verified(request));
		const action = verifiedAttentionVisibility(document.createElement('article'), {
			decision_item: item,
			impression_policy: policy,
			surface: 'follow_up',
			record
		});
		// Explicit feedback retains its decision binding; passive impressions still
		// require a selected card and a verified dwell.
		expect(attentionFeedbackAttribution(item, policy, 'follow_up')).toEqual({
			decision_id: 'decision-1',
			candidate_id: 'candidate-1',
			source_revision: 'revision-1'
		});
		expect(FakeIntersectionObserver.instances).toHaveLength(0);
		await vi.advanceTimersByTimeAsync(policy.min_visible_ms);
		expect(record).not.toHaveBeenCalled();
		action.destroy();
	});

	it('does not attribute a missing or cross-surface card', () => {
		const policy = { min_visible_ms: 1_000, visibility_rule_version: 'visible-50-v1' };
		expect(attentionFeedbackAttribution(null, policy, 'follow_up')).toBeNull();
		expect(attentionFeedbackAttribution(undefined, policy, 'follow_up')).toBeNull();
		expect(attentionFeedbackAttribution(decisionItem, policy, 'worth_a_look')).toBeNull();
	});

	it('cancels an unfinished dwell on unmount', async () => {
		const record = vi.fn(async (request: AttentionImpressionRequest) => verified(request));
		const node = document.createElement('article');
		const action = verifiedAttentionVisibility(node, {
			decision_item: decisionItem,
			impression_policy: { min_visible_ms: 1_000, visibility_rule_version: 'visible-50-v1' },
			surface: 'follow_up',
			record
		});
		FakeIntersectionObserver.instances[0].emit(true, 0.75);
		await vi.advanceTimersByTimeAsync(500);
		action.destroy();
		await vi.advanceTimersByTimeAsync(2_000);
		expect(record).not.toHaveBeenCalled();
	});

	it('requires a fresh continuous dwell after the document is hidden and resumes while intersecting', async () => {
		const record = vi.fn(async (request: AttentionImpressionRequest) => verified(request));
		const node = document.createElement('article');
		verifiedAttentionVisibility(node, {
			decision_item: decisionItem,
			impression_policy: { min_visible_ms: 1_000, visibility_rule_version: 'visible-50-v1' },
			surface: 'follow_up',
			record
		});
		FakeIntersectionObserver.instances[0].emit(true, 0.75);
		await vi.advanceTimersByTimeAsync(700);
		Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
		document.dispatchEvent(new Event('visibilitychange'));
		await vi.advanceTimersByTimeAsync(1_000);
		expect(record).not.toHaveBeenCalled();

		Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
		document.dispatchEvent(new Event('visibilitychange'));
		await vi.advanceTimersByTimeAsync(999);
		expect(record).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);
		expect(record).toHaveBeenCalledTimes(1);
	});

	it('retries transport failure with the identical event id and is dedupe-safe', async () => {
		const requests: AttentionImpressionRequest[] = [];
		const record = vi.fn(async (request: AttentionImpressionRequest) => {
			requests.push({ ...request });
			if (requests.length === 1) {
				return {
					ok: false,
					error: 'temporary persistence failure',
					code: 'impression_persist_failed',
					retryable: true
				} as const;
			}
			return verified(request, true);
		});
		const node = document.createElement('article');
		verifiedAttentionVisibility(node, {
			decision_item: decisionItem,
			impression_policy: { min_visible_ms: 1_000, visibility_rule_version: 'visible-50-v1' },
			surface: 'follow_up',
			record
		});
		FakeIntersectionObserver.instances[0].emit(true, 0.75);
		await vi.advanceTimersByTimeAsync(1_000);
		await vi.advanceTimersByTimeAsync(500);

		expect(record).toHaveBeenCalledTimes(2);
		expect(requests[1]).toEqual(requests[0]);
		expect(requests[1].event_id).toBe(requests[0].event_id);
		expect(node.dataset.attentionImpressionDeduplicated).toBe('true');
	});
});
