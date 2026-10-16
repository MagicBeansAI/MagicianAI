import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/environment', () => ({ browser: true, dev: false, building: false, version: 'test' }));
vi.mock('$lib/shared/fetch', () => ({ timedFetch: vi.fn(), LONG_FETCH_TIMEOUT_MS: 600_000 }));

import { timedFetch } from '$lib/shared/fetch';
import { eventHub } from '$lib/realtime/eventHub';
import { ensurePendingHitlBridge, pendingHitlCount, teardownPendingHitlBridge } from './pendingHitlStore';

const ask = (id: string) => ({
	event_type: 'HitlRequested',
	data: {
		correlation_id: id,
		source: 'clarification',
		input_type: 'text',
		prompt: 'Which account?',
		principal: 'owner',
		workspace: 'default',
		task_id: 'task-1'
	}
});
const resolved = (id: string) => ({ event_type: 'HitlResolved', data: { correlation_id: id } });
const ndjson = (frames: unknown[]) => frames.map((f) => JSON.stringify(f)).join('\n');

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe('pendingHitlStore on the event hub', () => {
	const urls: string[] = [];
	let backfillBody = '';

	beforeEach(() => {
		// No socket in unit tests: frames are injected into the hub.
		vi.stubGlobal('WebSocket', undefined);
		urls.length = 0;
		vi.mocked(timedFetch).mockImplementation(async (url) => {
			urls.push(String(url));
			return new Response(backfillBody);
		});
	});

	afterEach(() => {
		teardownPendingHitlBridge();
		vi.unstubAllGlobals();
	});

	it('backfills once over REST, then counts live asks from the hub', async () => {
		backfillBody = ndjson([ask('a'), ask('b'), resolved('a')]);
		ensurePendingHitlBridge();
		await flush();
		expect(urls).toHaveLength(1);
		expect(urls[0]).toContain('backfill_only=true');
		expect(urls[0]).toContain('category=hitl');
		expect(get(pendingHitlCount)).toBe(1);

		eventHub.ingestFrame(ask('c'));
		expect(get(pendingHitlCount)).toBe(2);
		eventHub.ingestFrame(resolved('b'));
		expect(get(pendingHitlCount)).toBe(1);
		// Not HITL: filtered out by the hub subscription.
		eventHub.ingestFrame({ event_type: 'TaskUpdated', data: { correlation_id: 'x' } });
		expect(get(pendingHitlCount)).toBe(1);
	});

	it('re-reads history on a hub resync, dropping asks resolved while away', async () => {
		backfillBody = ndjson([ask('a')]);
		ensurePendingHitlBridge();
		await flush();
		expect(get(pendingHitlCount)).toBe(1);

		backfillBody = ndjson([ask('a'), resolved('a'), ask('d')]);
		eventHub.ingestFrame({ type: 'hub.resync', reason: 'lagged' });
		await flush();
		expect(urls).toHaveLength(2);
		expect(get(pendingHitlCount)).toBe(1);
	});
});
