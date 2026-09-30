import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import AppSlotRegion, { slotShowsAssignment, widgetFillsRegion } from './AppSlotRegion.svelte';
import type { AppResolvedSlotAssignment, AppSlotPackageBinding, AppWidgetNativeModel, AppWidgetRenderItem, AppWidgetRenderRow } from './appWidgets';
import { scopeIdentityStore, setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';

const DIGEST = `blake3:${'b'.repeat(64)}`;
const RENDERED_AT = new Date().toISOString();
const REFRESH_AFTER = new Date(Date.now() + 60 * 60 * 1_000).toISOString();

beforeEach(() => scopeIdentityStore.reset());

function assignment(
	slotId: string,
	installation: string,
	widget: string,
	source: 'user' | 'workspace_default' = 'workspace_default',
	packageBindingOverride?: AppSlotPackageBinding
) {
	const packageBinding = packageBindingOverride ?? {
		installation_id: installation,
		package_id: `magician.${installation}`,
		package_revision_ref: `package:${installation}:1`,
		package_content_digest: DIGEST,
		installation_generation: 1
	};
	return {
		slot_id: slotId, source, pinned_system_default: true, opted_out: false,
		widget: { pinned: { package: packageBinding, widget_id: widget }, current: { package: packageBinding, widget_id: widget }, restored_across_generation: false, assignment_compatibility: 'exact_digest_only' }
	};
}

afterEach(() => {
	vi.unstubAllGlobals();
	window.localStorage.clear();
	scopeIdentityStore.reset();
});

describe('AppSlotRegion', () => {
	it('resolves page regions then renders the whole page target set in one batch', async () => {
		const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = String(input);
			if (url.endsWith('/slots/resolve-batch')) {
				const request = JSON.parse(String(init?.body));
				expect(request.slot_ids).toEqual(['page:2f:primary', 'page:2f:secondary']);
				return new Response(JSON.stringify({ assignments: [
					assignment('page:2f:primary', 'thinking-map', 'open-items'),
					assignment('page:2f:secondary', 'learning', 'due-items')
				] }));
			}
			if (url.endsWith('/widgets/render-batch')) {
				const request = JSON.parse(String(init?.body));
				expect(request.widgets).toEqual([
					{ installation_id: 'thinking-map', widget_id: 'open-items' },
					{ installation_id: 'learning', widget_id: 'due-items' }
				]);
				return new Response(JSON.stringify({ schema_version: 1, revision: DIGEST, etag: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER, widgets: request.widgets.map((target: { installation_id: string; widget_id: string }) => ({
					...target, installation_generation: 1, revision: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER, state: 'ready',
					title: target.installation_id, model: { model: 'list', rows: [], hints: {}, actions: [] }
				})) }), { headers: { ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': REFRESH_AFTER } });
			}
			throw new Error(`unexpected request ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(AppSlotRegion, { page: '/', regions: ['primary', 'secondary'], ariaLabel: 'Today app widgets' });
		expect(await screen.findByRole('region', { name: 'Today app widgets' })).toBeInTheDocument();
		expect(await screen.findByRole('article', { name: 'thinking-map' })).toBeInTheDocument();
		expect(await screen.findByRole('article', { name: 'learning' })).toBeInTheDocument();
		expect(fetchMock.mock.calls.filter(([url]) => String(url).endsWith('/widgets/render-batch'))).toHaveLength(1);
	});

	it('deduplicates one widget assigned to multiple page regions', async () => {
		const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = String(input);
			if (url.endsWith('/slots/resolve-batch')) {
				return new Response(JSON.stringify({ assignments: [
					assignment('page:2f:primary', 'learning', 'due-items'),
					assignment('page:2f:secondary', 'learning', 'due-items')
				] }));
			}
			const request = JSON.parse(String(init?.body));
			expect(request.widgets).toEqual([{ installation_id: 'learning', widget_id: 'due-items' }]);
			return new Response(JSON.stringify({
				schema_version: 1, revision: DIGEST, etag: DIGEST,
				rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER,
				widgets: [{ installation_id: 'learning', widget_id: 'due-items', installation_generation: 1,
					revision: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER,
					state: 'ready', model: { model: 'list', rows: [], hints: {}, actions: [] } }]
			}), { headers: { ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': REFRESH_AFTER } });
		});
		vi.stubGlobal('fetch', fetchMock);
		render(AppSlotRegion, { page: '/', regions: ['primary', 'secondary'] });
		expect(await screen.findAllByRole('article')).toHaveLength(2);
	});

	it('assigns an empty slot with one fenced, retry-stable picker mutation', async () => {
		const slotId = 'page:2f6f627365727665:reviews';
		const packageBinding = {
			installation_id: 'claims-review', package_id: 'magician.claims-review',
			package_revision_ref: 'package:claims:1', package_content_digest: DIGEST,
			installation_generation: 1
		};
		let assigned = false;
		let settingsReads = 0;
		const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = String(input);
			if (url.endsWith('/slots/resolve-batch')) return new Response(JSON.stringify({ assignments: [
				assigned ? assignment(slotId, 'claims-review', 'queue', 'user', packageBinding) : { slot_id: slotId, pinned_system_default: false, opted_out: false }
			] }));
			if (url.includes('/slot-assignments?')) {
				settingsReads += 1;
				return new Response(JSON.stringify(settingsReads === 1 ? {
					head: { revision: 0, fence: 1 }, inventory_revision: DIGEST, assignments: [], assignments_truncated: false,
					picker: [], next_picker_cursor: 'claims-review:before', picker_truncated: true
				} : {
					head: { revision: 0, fence: 2 }, inventory_revision: DIGEST, assignments: [], assignments_truncated: false,
					picker: [{ widget: { package: packageBinding, widget_id: 'queue' }, title: 'Claim queue', suggested_slots: [], system_class: false }],
					picker_truncated: false
				}));
			}
			if (url.endsWith('/slot-assignments') && init?.method === 'POST') {
				const request = JSON.parse(String(init.body));
				expect(request.expected_revision).toBe(0);
				expect(request.write_fence).toBe(2);
				expect(request.mutation_id).toMatch(/^slot-mutation:/);
				expect(request.command).toEqual({
					command: 'assign', slot_id: slotId, installation_id: 'claims-review', widget_id: 'queue',
					expected_candidate: { widget_id: 'queue', package: packageBinding }
				});
				assigned = true;
				return new Response(JSON.stringify({ mutation_id: request.mutation_id, head: { revision: 1, fence: 2 }, assignment: assignment(slotId, 'claims-review', 'queue', 'user', packageBinding) }));
			}
			if (url.endsWith('/widgets/render-batch')) {
				const request = JSON.parse(String(init?.body));
				return new Response(JSON.stringify({ schema_version: 1, revision: DIGEST, etag: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER, widgets: request.widgets.map((target: { installation_id: string; widget_id: string }) => ({
					...target, installation_generation: 1, revision: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER, state: 'ready', title: 'Claim queue', model: { model: 'list', rows: [], hints: {}, actions: [] }
				})) }), { headers: { ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': REFRESH_AFTER } });
			}
			throw new Error(`unexpected request ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(AppSlotRegion, { page: '/observe', region: 'reviews' });
		await fireEvent.click(await screen.findByRole('button', { name: 'Add a widget to reviews' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Load more widgets' }));
		await fireEvent.click(await screen.findByRole('button', { name: /Claim queue/ }));
		await waitFor(() => expect(fetchMock.mock.calls.some(([url, init]) => String(url).endsWith('/slot-assignments') && init?.method === 'POST')).toBe(true));
		expect(await screen.findByRole('article', { name: 'Claim queue' })).toBeInTheDocument();
	});

	it('shows an unavailable render as a native placeholder', async () => {
		const slotId = 'page:2f6f627365727665:reviews';
		const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.endsWith('/slots/resolve-batch')) return new Response(JSON.stringify({ assignments: [
				assignment(slotId, 'claims-review', 'queue')
			] }));
			if (url.endsWith('/widgets/render-batch')) return new Response(JSON.stringify({
				schema_version: 1, revision: DIGEST, etag: DIGEST,
				rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER,
				widgets: [{ installation_id: 'claims-review', widget_id: 'queue', installation_generation: 1,
					revision: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER,
					state: 'unavailable' }]
			}), { headers: { ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': REFRESH_AFTER } });
			throw new Error(`unexpected request ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(AppSlotRegion, { page: '/observe', region: 'reviews' });
		expect(await screen.findByText('This assigned widget is temporarily unavailable.')).toBeInTheDocument();
	});

	it('keeps a rendered widget through a past deadline, a 304 and a transient failure', async () => {
		vi.useFakeTimers({ shouldAdvanceTime: true });
		try {
			const slotId = 'page:2f6f627365727665:reviews';
			let renders = 0;
			const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.endsWith('/slots/resolve-batch')) return new Response(JSON.stringify({ assignments: [
					assignment(slotId, 'claims-review', 'queue')
				] }));
				if (url.endsWith('/widgets/render-batch')) {
					renders += 1;
					if (renders === 1) {
						// A shared server cache expired just before this arrived.
						const renderedAt = new Date(Date.now() - 30_000).toISOString();
						const refreshAfter = new Date(Date.now() - 1).toISOString();
						return new Response(JSON.stringify({
							schema_version: 1, revision: DIGEST, etag: DIGEST, rendered_at: renderedAt, refresh_after: refreshAfter,
							widgets: [{ installation_id: 'claims-review', widget_id: 'queue', installation_generation: 1,
								revision: DIGEST, rendered_at: renderedAt, refresh_after: refreshAfter,
								state: 'ready', title: 'Claim queue', model: { model: 'list', rows: [], hints: {}, actions: [] } }]
						}), { headers: { ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': refreshAfter } });
					}
					if (renders === 2) return new Response(null, { status: 304, headers: {
						ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': new Date(Date.now() - 1).toISOString()
					} });
					return new Response(JSON.stringify({ error: 'unavailable' }), { status: 503 });
				}
				throw new Error(`unexpected request ${url}`);
			});
			vi.stubGlobal('fetch', fetchMock);
			render(AppSlotRegion, { page: '/observe', region: 'reviews' });
			expect(await screen.findByRole('article', { name: 'Claim queue' })).toBeInTheDocument();
			expect(screen.queryByText('This assigned widget is temporarily unavailable.')).toBeNull();
			for (const expected of [2, 3]) {
				await vi.advanceTimersByTimeAsync(2_500);
				await waitFor(() => expect(renders).toBe(expected));
				await vi.advanceTimersByTimeAsync(0);
				expect(screen.getByRole('article', { name: 'Claim queue' })).toBeInTheDocument();
				expect(screen.queryByText('This assigned widget is temporarily unavailable.')).toBeNull();
			}
		} finally {
			vi.useRealTimers();
		}
	});

	it('shows a quiet loading state, not the placeholder, while the first render is in flight', async () => {
		const slotId = 'page:2f6f627365727665:reviews';
		let release: (response: Response) => void = () => {};
		const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.endsWith('/slots/resolve-batch')) return new Response(JSON.stringify({ assignments: [
				assignment(slotId, 'claims-review', 'queue')
			] }));
			if (url.endsWith('/widgets/render-batch')) return new Promise<Response>((resolve) => { release = resolve; });
			throw new Error(`unexpected request ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(AppSlotRegion, { page: '/observe', region: 'reviews' });
		expect(await screen.findByLabelText('Loading the widget for reviews')).toBeInTheDocument();
		expect(screen.queryByText('This assigned widget is temporarily unavailable.')).toBeNull();
		release(new Response(JSON.stringify({
			schema_version: 1, revision: DIGEST, etag: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER,
			widgets: [{ installation_id: 'claims-review', widget_id: 'queue', installation_generation: 1,
				revision: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER,
				state: 'ready', title: 'Claim queue', model: { model: 'list', rows: [], hints: {}, actions: [] } }]
		}), { headers: { ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': REFRESH_AFTER } }));
		expect(await screen.findByRole('article', { name: 'Claim queue' })).toBeInTheDocument();
		expect(screen.queryByLabelText('Loading the widget for reviews')).toBeNull();
	});

	it('reads a hidden workspace default as an empty slot, but keeps the placeholder for the viewer\'s own', async () => {
		const hidden = (slot_id: string, source: 'user' | 'workspace_default') => ({
			slot_id, source, pinned_system_default: source === 'workspace_default', opted_out: false, hidden_reason: 'package_unavailable'
		});
		const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.endsWith('/slots/resolve-batch')) return new Response(JSON.stringify({ assignments: [
				hidden('page:2f:primary', 'workspace_default'),
				hidden('page:2f:secondary', 'user')
			] }));
			throw new Error(`unexpected request ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(AppSlotRegion, { page: '/', regions: ['primary', 'secondary'] });
		expect(await screen.findByRole('button', { name: 'Add a widget to primary' })).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Remove the widget from primary' })).toBeNull();
		expect(screen.getAllByText('This assigned widget is temporarily unavailable.')).toHaveLength(1);
		expect(screen.getByRole('button', { name: 'Remove the widget from secondary' })).toBeInTheDocument();
		expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith('/widgets/render-batch'))).toBe(false);
	});

	it('refuses an old widget action after the bearer rotates ahead of visible scope', async () => {
		const slotId = 'page:2f6f627365727665:reviews';
		const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.endsWith('/slots/resolve-batch')) return new Response(JSON.stringify({ assignments: [
				assignment(slotId, 'claims-review', 'queue')
			] }));
			if (url.endsWith('/widgets/render-batch')) return new Response(JSON.stringify({
				schema_version: 1, revision: DIGEST, etag: DIGEST,
				rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER,
				widgets: [{ installation_id: 'claims-review', widget_id: 'queue', installation_generation: 1,
					revision: DIGEST, rendered_at: RENDERED_AT, refresh_after: REFRESH_AFTER,
					state: 'ready', title: 'Claim queue', model: { model: 'list', rows: [], hints: {}, actions: [{ action_id: 'sync', label: 'Sync' }] } }]
			}), { headers: { ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': REFRESH_AFTER } });
			throw new Error(`unexpected request ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(AppSlotRegion, { page: '/observe', region: 'reviews' });
		const action = await screen.findByRole('button', { name: 'Sync' });
		setCurrentScopeBearerToken('scope-token-two');
		await fireEvent.click(action);
		expect(await screen.findByText(/signed-in Apps scope changed/)).toBeInTheDocument();
		expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith('/runs'))).toBe(false);
	});

	it('replays a retryable widget refusal with the same durable idempotency identity', async () => {
		window.localStorage.clear();
		const slotId = 'page:2f6f627365727665:reviews';
		const launchBodies: Array<Record<string, unknown>> = [];
		const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = String(input);
			if (url.endsWith('/slots/resolve-batch')) return new Response(JSON.stringify({ assignments: [
				assignment(slotId, 'claims-review', 'queue')
			] }));
			if (url.endsWith('/widgets/render-batch')) {
				const renderedAt = new Date().toISOString();
				const refreshAfter = new Date(Date.now() + 60_000).toISOString();
				return new Response(JSON.stringify({
					schema_version: 1, revision: DIGEST, etag: DIGEST, rendered_at: renderedAt, refresh_after: refreshAfter,
					widgets: [{ installation_id: 'claims-review', widget_id: 'queue', installation_generation: 1,
						revision: DIGEST, rendered_at: renderedAt, refresh_after: refreshAfter,
						state: 'ready', title: 'Claim queue', model: { model: 'list', rows: [], hints: {}, actions: [{ action_id: 'sync', label: 'Sync' }] } }]
				}), { headers: { ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': refreshAfter } });
			}
			if (url.endsWith('/runs')) {
				launchBodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
				if (launchBodies.length === 1) return new Response(JSON.stringify({ error: 'too_early', message: 'Retry later.' }), { status: 425 });
				return new Response(JSON.stringify({
					run_handle: { protocol_version: '1', run_ref: 'run:app-action:fixture', installation_id: 'claims-review', action_id: 'sync' }
				}));
			}
			throw new Error(`unexpected request ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(AppSlotRegion, { page: '/observe', region: 'reviews' });
		const action = await screen.findByRole('button', { name: 'Sync' });
		await fireEvent.click(action);
		expect(await screen.findByText('Retry later.')).toBeInTheDocument();
		await fireEvent.click(action);
		await waitFor(() => expect(launchBodies).toHaveLength(2));
		expect(launchBodies[1].idempotency_key).toBe(launchBodies[0].idempotency_key);
		expect(launchBodies[1].expected_installation_binding).toEqual({
			generation: 1,
			package_revision_ref: 'package:claims-review:1'
		});
	});
});

describe('a widget that fills a region', () => {
	// The retreat this predicate feeds (gate M4) removes a first-party section
	// from the page. Every case below is a widget that renders *no records*, so
	// retreating for it would lose the section's rows and put nothing in their
	// place — the exact regression this pins shut.
	const stamp = {
		installation_id: 'meetings',
		widget_id: 'recent_meetings',
		installation_generation: 1,
		revision: DIGEST,
		rendered_at: RENDERED_AT,
		refresh_after: REFRESH_AFTER
	} as const;
	const row: AppWidgetRenderRow = {
		entity: 'meeting_thread',
		record_id: 'thread-1',
		record_revision: 1,
		fields: { title: 'Standup' }
	};
	const ready = (model: AppWidgetNativeModel, miniFrame = false): AppWidgetRenderItem => ({
		...stamp,
		state: 'ready',
		model,
		...(miniFrame ? { mini_frame: { entry_point: 'index.html', max_height_px: 320 } } : {})
	});

	it('counts a ready widget that projected records', () => {
		expect(widgetFillsRegion(ready({ model: 'table', columns: ['title'], rows: [row], hints: {}, actions: [] }))).toBe(true);
		expect(widgetFillsRegion(ready({ model: 'list', rows: [row], hints: {}, actions: [] }))).toBe(true);
		expect(widgetFillsRegion(ready({ model: 'detail', row, hints: {}, actions: [] }))).toBe(true);
	});

	it('refuses a ready widget that projected nothing', () => {
		// `ready` only says the query succeeded. An empty page still projects, and
		// `AppNativeWidget` renders "No items yet." for it.
		expect(widgetFillsRegion(ready({ model: 'table', columns: ['title'], rows: [], hints: {}, actions: [] }))).toBe(false);
		expect(widgetFillsRegion(ready({ model: 'list', rows: [], hints: {}, actions: [] }))).toBe(false);
		expect(widgetFillsRegion(ready({ model: 'timeline', rows: [], hints: {}, actions: [] }))).toBe(false);
		expect(widgetFillsRegion(ready({ model: 'detail', row: null, hints: {}, actions: [] }))).toBe(false);
	});

	it('refuses a declared mini-frame as the proof, and reads the native fallback instead', () => {
		// The frame is a page-bounded escalation this client may refuse, and the
		// runtime compiles the same-view native fallback either way. An empty
		// fallback beside a frame declaration proves nothing is on the page.
		expect(widgetFillsRegion(ready({ model: 'list', rows: [], hints: {}, actions: [] }, true))).toBe(false);
		expect(widgetFillsRegion(ready({ model: 'list', rows: [row], hints: {}, actions: [] }, true))).toBe(true);
	});

	it('refuses every state that renders a placeholder', () => {
		expect(widgetFillsRegion({ ...stamp, state: 'unavailable' })).toBe(false);
		expect(widgetFillsRegion({ ...stamp, state: 'unsupported', fallback: { kind: 'hide' } })).toBe(false);
	});
});

describe('a slot that shows its assignment', () => {
	const hidden = (source: 'user' | 'workspace_default', hidden_reason: NonNullable<AppResolvedSlotAssignment['hidden_reason']>): AppResolvedSlotAssignment => ({
		slot_id: 'page:2f:primary', source, pinned_system_default: false, opted_out: false, hidden_reason
	});

	it('hides a workspace default the viewer cannot act on', () => {
		expect(slotShowsAssignment(hidden('workspace_default', 'package_unavailable'))).toBe(false);
		expect(slotShowsAssignment(hidden('workspace_default', 'widget_no_longer_declared'))).toBe(false);
		expect(slotShowsAssignment({ slot_id: 'page:2f:primary', pinned_system_default: false, opted_out: false })).toBe(false);
	});

	it('keeps the viewer\'s own and actionable package states', () => {
		expect(slotShowsAssignment(hidden('user', 'package_unavailable'))).toBe(true);
		expect(slotShowsAssignment(hidden('workspace_default', 'disabled'))).toBe(true);
		expect(slotShowsAssignment(hidden('workspace_default', 'update_pending'))).toBe(true);
	});
});
