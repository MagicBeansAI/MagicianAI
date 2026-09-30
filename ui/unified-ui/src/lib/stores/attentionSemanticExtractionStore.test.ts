import { get } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { attentionSemanticExtractionStore } from './attentionSemanticExtractionStore';
import { scopeIdentityStore } from './scopeIdentityStore';

function counts(active: number) {
	return {
		active_total: active,
		compatible_revision: active,
		succeeded: active,
		missing: 0,
		invalid: 0,
		pending: 0,
		in_flight: 0,
		retry: 0,
		dead: 0,
		coverage: active === 0 ? 0 : 1
	};
}

function response(): Response {
	return new Response(JSON.stringify({
		semantic_extraction_health: {
			schema_version: 1,
			enabled: true,
			paused: false,
			pause_reason: null,
			degradation_reason: null,
			contract: {
				semantic_schema_version: 2,
				extractor_contract: 'attention-semantic-v2',
				prompt_version: 'attention-extract-v4',
				model: 'local',
				profile: 'background'
			},
			checkpoint: {
				cursor: null,
				updated_at: null,
				lease_owner: null,
				lease_expires_at: null
			},
			totals: counts(2),
			surfaces: { follow_up: counts(1), worth_a_look: counts(1) },
			queue: {
				pending: 0,
				in_flight: 0,
				active_in_flight: 0,
				expired_in_flight: 0,
				retry: 0,
				dead: 0,
				next_retry_at: null,
				oldest_ready_at: null,
				last_succeeded_at: null
			}
		}
	}), { status: 200 });
}

afterEach(() => {
	attentionSemanticExtractionStore.clear();
	scopeIdentityStore.reset();
	vi.unstubAllGlobals();
});

describe('attention semantic extraction store', () => {
	it('coalesces same-scope diagnostics and reuses the bounded fresh result', async () => {
		scopeIdentityStore.observe('p', 'w');
		let resolveRequest!: (value: Response) => void;
		const request = new Promise<Response>((resolve) => (resolveRequest = resolve));
		const fetchMock = vi.fn().mockReturnValue(request);
		vi.stubGlobal('fetch', fetchMock);

		const first = attentionSemanticExtractionStore.refresh('p:w');
		const second = attentionSemanticExtractionStore.refresh('p:w');
		expect(second).toBe(first);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		resolveRequest(response());
		await Promise.all([first, second]);
		expect(get(attentionSemanticExtractionStore).health?.totals.active_total).toBe(2);

		await attentionSemanticExtractionStore.refresh('p:w');
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('aborts an obsolete scope without publishing its late diagnostics', async () => {
		let firstSignal: AbortSignal | undefined;
		let resolveFirst!: (value: Response) => void;
		const firstResponse = new Promise<Response>((resolve) => (resolveFirst = resolve));
		vi.stubGlobal('fetch', vi.fn()
			.mockImplementationOnce((_url: string, init?: RequestInit) => {
				firstSignal = init?.signal ?? undefined;
				return firstResponse;
			})
			.mockResolvedValueOnce(response()));

		scopeIdentityStore.observe('old', 'scope');
		const stale = attentionSemanticExtractionStore.refresh('old:scope');
		scopeIdentityStore.observe('new', 'scope');
		await attentionSemanticExtractionStore.refresh('new:scope');
		expect(firstSignal?.aborted).toBe(true);
		resolveFirst(response());
		await stale;
		expect(get(attentionSemanticExtractionStore).loadedScopeKey).toBe('new:scope');
	});
});
