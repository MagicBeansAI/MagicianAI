import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	APP_WIDGET_MAX_STALENESS_MS,
	APP_WIDGET_REFRESH_FLOOR_MS,
	appPageSlotId,
	appWidgetRefreshDeadline,
	appWidgetWithinStaleness,
	appSurfaceSlotPage,
	fetchAppSlotSettings,
	launchEmptyInputWidgetAction,
	mutateAppSlotAssignment,
	parseAppIndicatorList,
	parseAppResolvedSlot,
	parseAppSlotSettingsPage,
	parseAppWidgetRenderBatch,
	renderAppWidgetBatch,
	resolveAppSlots
} from './appWidgets';

const DIGEST = `blake3:${'a'.repeat(64)}`;

function widgetPage() {
	return {
		schema_version: 1,
		revision: DIGEST,
		etag: DIGEST,
		rendered_at: '2026-09-02T10:00:00Z',
		refresh_after: '2026-09-02T10:01:00Z',
		widgets: [{
			installation_id: 'claims-review', widget_id: 'queue', title: 'Queue',
			installation_generation: 2, revision: DIGEST, rendered_at: '2026-09-02T10:00:00Z', refresh_after: '2026-09-02T10:01:00Z',
			state: 'ready', model: {
				model: 'list', rows: [{ entity: 'claim', record_id: 'claim-1', record_revision: 1, fields: { title: '<script>no</script>' } }],
				hints: { display_field: 'title' }, actions: [{ action_id: 'sync', label: 'Sync' }]
			}
		}]
	};
}

afterEach(() => vi.unstubAllGlobals());

describe('app widget transport', () => {
	it('constructs the server page-qualified slot identity without normalization', () => {
		expect(appPageSlotId('/', 'primary')).toBe('page:2f:primary');
		expect(appPageSlotId('/observe', 'reviews')).toBe('page:2f6f627365727665:reviews');
		expect(() => appPageSlotId('/observe?all=1', 'reviews')).toThrow();
		expect(appSurfaceSlotPage('claims-review', 'queue')).toBe('/apps/claims-review/queue');
		expect(appSurfaceSlotPage('claims-review', 'queue:item')).toBeNull();
	});

	it('strictly decodes slot settings and exact current package restoration', () => {
		const packageBinding = { installation_id: 'claims-review', package_id: 'magician.claims-review', package_revision_ref: 'package:claims:1', package_content_digest: DIGEST, installation_generation: 2 };
		const slot = appPageSlotId('/', 'primary');
		const settings = parseAppSlotSettingsPage({
			head: { revision: 4, fence: 7 }, inventory_revision: DIGEST, assignments: [], assignments_truncated: false,
			picker: [{ widget: { package: packageBinding, widget_id: 'queue' }, title: 'Claim queue', suggested_slots: [{ page: '/', region: 'primary', slot_id: slot, system_default: false }], system_class: false }],
			picker_truncated: false
		});
		expect(settings.picker[0].widget.package.package_content_digest).toBe(DIGEST);
		expect(() => parseAppResolvedSlot({
			slot_id: slot, source: 'user', pinned_system_default: false, opted_out: false,
			widget: { pinned: { package: packageBinding, widget_id: 'queue' }, current: { package: { ...packageBinding, installation_generation: 3 }, widget_id: 'queue' }, restored_across_generation: false, assignment_compatibility: 'exact_digest_only' }
		}, slot)).toThrow();
		expect(() => parseAppSlotSettingsPage({
			head: { revision: 4, fence: 7 }, inventory_revision: DIGEST, assignments: [], assignments_truncated: false,
			picker: [], next_picker_cursor: '', picker_truncated: true
		})).toThrow();
	});

	it('uses bounded slot settings and ordered batch transports', async () => {
		const primary = appPageSlotId('/', 'primary');
		const secondary = appPageSlotId('/', 'secondary');
		const empty = (slot_id: string) => ({ slot_id, pinned_system_default: false, opted_out: false });
		const fetchMock = vi.fn()
			.mockResolvedValueOnce(new Response(JSON.stringify({ head: { revision: 0, fence: 1 }, inventory_revision: DIGEST, assignments: [], assignments_truncated: false, picker: [], picker_truncated: false })))
			.mockResolvedValueOnce(new Response(JSON.stringify({ assignments: [empty(primary), empty(secondary)] })));
		vi.stubGlobal('fetch', fetchMock);
		await fetchAppSlotSettings({ assignmentLimit: 12, pickerLimit: 32 });
		const assignments = await resolveAppSlots('/', ['primary', 'secondary']);
		expect(assignments.map((assignment) => assignment.slot_id)).toEqual([primary, secondary]);
		expect(String(fetchMock.mock.calls[0][0])).toContain('assignment_limit=12');
		expect(JSON.parse(String(fetchMock.mock.calls[1][1].body))).toEqual({ slot_ids: [primary, secondary] });
	});

	it('requires a slot mutation receipt to advance the exact fenced revision', async () => {
		const slot = appPageSlotId('/', 'primary');
		const request = { expected_revision: 4, write_fence: 7, mutation_id: 'slot-mutation:fixture', command: { command: 'opt_out' as const, slot_id: slot } };
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ mutation_id: request.mutation_id, head: { revision: 4, fence: 7 }, assignment: { slot_id: slot, pinned_system_default: false, opted_out: true } }))));
		await expect(mutateAppSlotAssignment(request)).rejects.toThrow('stale');
	});

	it('strictly decodes a bounded native model and rejects executable-looking additions', () => {
		expect(parseAppWidgetRenderBatch(widgetPage()).widgets[0].state).toBe('ready');
		expect(() => parseAppWidgetRenderBatch({ ...widgetPage(), app_js: 'alert(1)' })).toThrow();
	});

	it('retains the cached body only for a matching 304 ETag', async () => {
		const page = parseAppWidgetRenderBatch(widgetPage());
		const refreshAfter = new Date(Date.now() + 60_000).toISOString();
		const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 304, headers: {
			ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': refreshAfter
		} }));
		vi.stubGlobal('fetch', fetchMock);
		const result = await renderAppWidgetBatch(
			[{ installation_id: 'claims-review', widget_id: 'queue' }],
			{ etag: DIGEST, value: page, refreshAfter }
		);
		expect(result.value.widgets[0]).toMatchObject({ state: 'ready', revision: DIGEST });
		expect(result.refreshAfter).toBe(refreshAfter);
		expect(new Headers(fetchMock.mock.calls[0][1].headers).get('if-none-match')).toBe(`"${DIGEST}"`);
	});

	it('renews every retained item deadline on a 304', async () => {
		// Unchanged content revalidates as 304 on every refresh. Moving only the
		// batch deadline left each item on its first deadline, and the region then
		// dropped the items as expired.
		const page = parseAppWidgetRenderBatch(widgetPage());
		const renewed = new Date(Date.now() + 90_000).toISOString();
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 304, headers: {
			ETag: `"${DIGEST}"`, 'X-App-Widget-Refresh-After': renewed
		} })));
		const result = await renderAppWidgetBatch(
			[{ installation_id: 'claims-review', widget_id: 'queue' }],
			{ etag: DIGEST, value: page, refreshAfter: page.refresh_after }
		);
		expect(result.refreshAfter).toBe(renewed);
		expect(result.value.refresh_after).toBe(renewed);
		expect(result.value.widgets.map((item) => item.refresh_after)).toEqual([renewed]);
		expect(page.widgets[0].refresh_after).toBe('2026-09-02T10:01:00Z');
	});

	it('accepts a past, missing or regressed 304 deadline and waits out the floor', async () => {
		const page = parseAppWidgetRenderBatch(widgetPage());
		for (const header of [new Date(Date.now() - 5).toISOString(), null]) {
			vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 304, headers: {
				ETag: `"${DIGEST}"`, ...(header ? { 'X-App-Widget-Refresh-After': header } : {})
			} })));
			const before = Date.now();
			const result = await renderAppWidgetBatch(
				[{ installation_id: 'claims-review', widget_id: 'queue' }],
				{ etag: DIGEST, value: page, refreshAfter: new Date(Date.now() + 60_000).toISOString() }
			);
			const deadline = Date.parse(result.refreshAfter!);
			expect(deadline).toBeGreaterThanOrEqual(before + APP_WIDGET_REFRESH_FLOOR_MS);
			expect(deadline).toBeLessThan(before + APP_WIDGET_REFRESH_FLOOR_MS + 1_000);
			expect(result.value.widgets[0].refresh_after).toBe(result.refreshAfter);
		}
	});

	it('still refuses a 304 whose ETag does not match the retained body', async () => {
		const page = parseAppWidgetRenderBatch(widgetPage());
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 304, headers: {
			ETag: `"blake3:${'c'.repeat(64)}"`, 'X-App-Widget-Refresh-After': new Date(Date.now() + 60_000).toISOString()
		} })));
		await expect(renderAppWidgetBatch(
			[{ installation_id: 'claims-review', widget_id: 'queue' }],
			{ etag: DIGEST, value: page }
		)).rejects.toThrow('cache response was invalid');
	});

	it('accepts a fresh body whose deadline is already behind the local clock', async () => {
		// A shared server cache can hand back a batch that expires milliseconds
		// after the server's now — behind this client's clock by arrival.
		const renderedAt = new Date(Date.now() - 30_000).toISOString();
		const refreshAfter = new Date(Date.now() - 1).toISOString();
		const body = widgetPage();
		body.rendered_at = renderedAt;
		body.refresh_after = refreshAfter;
		body.widgets[0].rendered_at = renderedAt;
		body.widgets[0].refresh_after = refreshAfter;
		for (const withHeader of [true, false]) {
			vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { headers: {
				ETag: `"${DIGEST}"`, ...(withHeader ? { 'X-App-Widget-Refresh-After': refreshAfter } : {})
			} })));
			const before = Date.now();
			const result = await renderAppWidgetBatch([{ installation_id: 'claims-review', widget_id: 'queue' }]);
			expect(result.value.widgets[0].state).toBe('ready');
			expect(Date.parse(result.refreshAfter!)).toBeGreaterThanOrEqual(before + APP_WIDGET_REFRESH_FLOOR_MS);
		}
	});

	it('bounds how long a stale projection may stay on screen', () => {
		const now = Date.parse('2026-09-02T10:00:00Z');
		expect(appWidgetWithinStaleness({ refresh_after: '2026-09-02T09:59:00Z' }, now)).toBe(true);
		expect(appWidgetWithinStaleness({ refresh_after: new Date(now - APP_WIDGET_MAX_STALENESS_MS).toISOString() }, now)).toBe(false);
		expect(appWidgetRefreshDeadline('2026-09-02T10:05:00Z', now)).toBe('2026-09-02T10:05:00Z');
		expect(appWidgetRefreshDeadline('2026-09-02T10:00:01Z', now)).toBe(new Date(now + APP_WIDGET_REFRESH_FLOOR_MS).toISOString());
	});

	it('accepts only unexpired-shape materialized indicator projections', () => {
		const page = parseAppIndicatorList({
			schema_version: 1, revision: DIGEST, etag: DIGEST, generated_at: '2026-09-02T10:00:00Z',
			indicators: [{ installation_id: 'learning', installation_generation: 1, indicator_id: 'due', title: 'Due',
				revision: DIGEST, evaluated_at: '2026-09-02T10:00:00Z', expires_at: '2026-09-02T10:01:00Z', model: { kind: 'badge', count: 2 } }]
		});
		expect(page.indicators[0].model).toEqual({ kind: 'badge', count: 2 });
		expect(() => parseAppIndicatorList({
			...page,
			indicators: [{ ...page.indicators[0], model: { kind: 'badge', count: 10_000 } }]
		})).toThrow();
	});

	it('launches governed widget actions with exact empty input', async () => {
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
			run_handle: { protocol_version: '1', run_ref: 'run:app-action:fixture', installation_id: 'claims-review', action_id: 'sync' }
		}), { status: 200, headers: { 'Content-Type': 'application/json' } }));
		vi.stubGlobal('fetch', fetchMock);
		await launchEmptyInputWidgetAction(
			'claims-review',
			'sync',
			'widget-action:fixture',
			2,
			'package:claims-review:2'
		);
		expect(JSON.parse(String(fetchMock.mock.calls[0][1].body))).toEqual({
			idempotency_key: 'widget-action:fixture',
			input: {},
			expected_installation_binding: {
				generation: 2,
				package_revision_ref: 'package:claims-review:2'
			}
		});
	});
});
