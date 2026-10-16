import { beforeEach, describe, expect, it, vi } from 'vitest';

const timedFetch = vi.hoisted(() => vi.fn());

vi.mock('$lib/shared/fetch', () => ({ timedFetch }));
vi.mock('$lib/stores/scopeIdentityStore', () => ({
	appendCurrentScopeQuery: (params?: URLSearchParams) => {
		const scoped = new URLSearchParams(params);
		scoped.set('principal', 'anonymous');
		scoped.set('workspace', 'default');
		return scoped;
	},
	scopedRequestHeaders: (headers: Record<string, string>) => headers
}));

import { interpret, listMapsPage, permanentlyDeleteMap } from './api';

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

describe('Thinking Maps lifecycle list and permanent-delete client', () => {
	beforeEach(() => timedFetch.mockReset());

	it('requests the deleted lifecycle as a server-side paginated window', async () => {
		timedFetch.mockResolvedValue(
			jsonResponse({ maps: [], total: 0, offset: 50, limit: 25 })
		);

		await listMapsPage(25, 50, 'deleted');

		expect(timedFetch).toHaveBeenCalledTimes(1);
		const [input, init] = timedFetch.mock.calls[0] as [string, RequestInit];
		const url = new URL(input, 'http://localhost');
		expect(url.pathname).toBe('/api/magician/v2/thinking-maps');
		expect(Object.fromEntries(url.searchParams)).toEqual({
			limit: '25',
			offset: '50',
			lifecycle: 'deleted',
			principal: 'anonymous',
			workspace: 'default'
		});
		expect(init.method).toBeUndefined();
	});

	it('sends the irreversible server confirmation on permanent deletion', async () => {
		timedFetch.mockResolvedValue(
			jsonResponse({
				deleted: true,
				map_id: 'map / one',
				detached_sessions: 1,
				cleared_tutor_contexts: 2
			})
		);

		const result = await permanentlyDeleteMap('map / one');

		const [input, init] = timedFetch.mock.calls[0] as [string, RequestInit];
		const url = new URL(input, 'http://localhost');
		expect(url.pathname).toBe('/api/magician/v2/thinking-maps/map%20%2F%20one');
		expect(url.searchParams.get('confirm')).toBe('permanent');
		expect(url.searchParams.get('principal')).toBe('anonymous');
		expect(url.searchParams.get('workspace')).toBe('default');
		expect(init.method).toBe('DELETE');
		expect(result.deleted).toBe(true);
	});
});

describe('Thinking Maps interpretation focus', () => {
	beforeEach(() => timedFetch.mockReset());

	it('sends the selected node as a request-scoped interpretation focus', async () => {
		timedFetch.mockResolvedValue(jsonResponse({ outcome: 'no_operations' }));

		await interpret('map-1', {
			text: 'Continue from this branch',
			intent: 'continue_thinking',
			focus_node_id: 'node-7'
		});

		const [input, init] = timedFetch.mock.calls[0] as [string, RequestInit];
		const url = new URL(input, 'http://localhost');
		expect(url.pathname).toBe('/api/magician/v2/thinking-maps/map-1/interpret');
		expect(JSON.parse(String(init.body))).toMatchObject({
			text: 'Continue from this branch',
			intent: 'continue_thinking',
			focus_node_id: 'node-7'
		});
	});
});
