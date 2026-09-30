import { afterEach, describe, expect, it, vi } from 'vitest';
import type { MuijComponent } from '$lib/stores/muijStore';
import {
	buildSurfaceMutationRequest,
	AppSurfaceClientError,
	appSurfaceMutationFailureKind,
	appSurfaceRealtimeSupported,
	appChangeSignalDisposition,
	coerceAppSurfaceForm,
	diffAppSurfaceValues,
	fetchAppEntityChanges,
	fetchAppSurface,
	formatAppSurfaceTimestampInput,
	hydrateAppSurface,
	optimisticAppSurfaceRecords,
	parseAppEntityChangeEvent,
	resolveAppChangeHead,
	retainedAppSurfaceMutationRequest,
	surfaceInteractionIsAppOwned,
	type AppSurfaceHydration
} from './appSurfaceRuntime';

function hydration(componentType = 'EntityGrid'): AppSurfaceHydration {
	return {
		binding: {
			installation_id: 'install_1',
			surface_revision: 4,
			view_id: 'items',
			app_local_route: '/items'
		},
		surface: {
			installation_id: 'install_1',
			surface_revision: 4,
			view_id: 'items',
			interaction_mode: 'app',
			muij_document: {
				muij_version: '1',
				agent_id: 'app:install_1',
				generated_at: '2026-08-17T00:00:00Z',
				layout: [{
					id: 'app-shell',
					component_type: 'Container',
					props: {},
					children: [{
						id: 'app-query',
						component_type: componentType,
						source: 'app_entity_query_v1',
						query: 'view:items',
						props: {
							rows: [],
							viewBinding: {
								entity: 'item',
								viewKind: 'table',
								fields: ['title', 'status'],
								fieldBindings: [
									{ field: 'status', kind: 'enum', required: true, nullable: false, sortable: true, allowedValues: ['open', 'done'] },
									{ field: 'title', kind: 'text', required: true, nullable: false, sortable: true }
								]
							}
						}
					}]
				}]
			}
		},
		route_parameters: {},
		change_sequence: 12,
		page: {
			envelope: {
				installation_id: 'install_1',
				schema_revision: 2,
				value: [{
					entity: 'item',
					record_id: 'record_1',
					record_revision: 7,
					fields: { title: 'One', status: 'open' }
				}]
			},
			next_cursor: 'cursor:next'
		}
	};
}

afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe('app surface runtime', () => {
	it('treats realtime events only as bounded synchronization hints', () => {
		const signal = {
			installation_id: 'install_1', surface_revision: 4,
			first_change_sequence: 13, last_change_sequence: 13,
			changes: [{ entity: 'item', record_id: 'record_1', record_revision: 8, change_sequence: 13 }],
			reset_required: false
		};
		expect(appChangeSignalDisposition(12, signal)).toBe('synchronize');
		expect(appChangeSignalDisposition(13, signal)).toBe('ignore');
		expect(appChangeSignalDisposition(3, { ...signal, first_change_sequence: 8, last_change_sequence: 8 })).toBe('synchronize');
		expect(appChangeSignalDisposition(12, { ...signal, changes: [], reset_required: true })).toBe('reset');
		expect(() => appChangeSignalDisposition(-1, signal)).toThrow(/cursor is invalid/);
	});

	it('replaces a stale ahead-of-head cursor only for a canonical reset reload', () => {
		expect(resolveAppChangeHead(19, 4, 4, 7, false)).toBe(19);
		expect(resolveAppChangeHead(19, 4, 4, 7, true)).toBe(7);
		expect(resolveAppChangeHead(19, 4, 5, 2, false)).toBe(2);
	});

	it('accepts only bounded identifier-only entity signals for the active surface', () => {
		const event = {
			event_type: 'AgentEvent',
			data: {
				event: {
					event_type: 'app.entity.changed',
					payload: {
						installation_id: 'install_1',
						surface_revision: 4,
						first_change_sequence: 13,
						last_change_sequence: 14,
						reset_required: false,
						changes: [
							{ entity: 'item', record_id: 'record_1', record_revision: 8, change_sequence: 13 },
							{ entity: 'item', record_id: 'record_2', record_revision: 1, change_sequence: 14 }
						]
					}
				}
			}
		};
		expect(parseAppEntityChangeEvent(event, 'install_1', 4)?.last_change_sequence).toBe(14);
		expect(parseAppEntityChangeEvent(event, 'install_1')?.surface_revision).toBe(4);
		expect(parseAppEntityChangeEvent(event, 'install_2', 4)).toBeNull();
		expect(parseAppEntityChangeEvent({
			...event,
			data: { event: { ...event.data.event, payload: { ...event.data.event.payload, fields: { secret: true } } } }
		}, 'install_1', 4)).toBeNull();
		expect(parseAppEntityChangeEvent({
			...event,
			data: { event: { ...event.data.event, payload: {
				...event.data.event.payload,
				changes: [{ entity: 'item', record_id: 'record_1', record_revision: 8, change_sequence: 14 }]
			} } }
		}, 'install_1', 4)).toBeNull();
		const reset = {
			...event,
			data: { event: { ...event.data.event, payload: {
				...event.data.event.payload,
				changes: [],
				reset_required: true
			} } }
		};
		expect(parseAppEntityChangeEvent(reset, 'install_1', 4)?.reset_required).toBe(true);
		expect(parseAppEntityChangeEvent({
			...reset,
			data: { event: { ...reset.data.event, payload: {
				...reset.data.event.payload,
				changes: event.data.event.payload.changes
			} } }
		}, 'install_1', 4)).toBeNull();
	});

	it('reads a contiguous bounded delta and rejects response identity drift', async () => {
		const body = {
			installation_id: 'install_1',
			surface_revision: 4,
			after_change_sequence: 12,
			through_change_sequence: 13,
			current_change_sequence: 13,
			changes: [{ entity: 'item', record_id: 'record_1', record_revision: 8, change_sequence: 13 }],
			has_more: false,
			reset_required: false
		};
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 })));
		await expect(fetchAppEntityChanges('install_1', 4, 12)).resolves.toEqual(body);
		expect(fetch).toHaveBeenCalledWith(expect.stringContaining('after_change_sequence=12'), expect.objectContaining({
			headers: expect.any(Object)
		}));

		vi.mocked(fetch).mockResolvedValueOnce(new Response(JSON.stringify({ ...body, installation_id: 'install_other' }), { status: 200 }));
		await expect(fetchAppEntityChanges('install_1', 4, 12)).rejects.toThrow(/inconsistent/);
	});

	it('accepts an explicit canonical-reset delta without pretending it contains records', async () => {
		const body = {
			installation_id: 'install_1',
			surface_revision: 4,
			after_change_sequence: 12,
			through_change_sequence: 19,
			current_change_sequence: 19,
			changes: [],
			has_more: false,
			reset_required: true
		};
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 })));

		await expect(fetchAppEntityChanges('install_1', 4, 12)).resolves.toEqual(body);

		vi.mocked(fetch).mockResolvedValueOnce(new Response(JSON.stringify({
			...body,
			through_change_sequence: 18
		}), { status: 200 }));
		await expect(fetchAppEntityChanges('install_1', 4, 12)).rejects.toThrow(/inconsistent/);
	});
	it('hydrates one server-owned page without changing the agent interaction protocol', () => {
		const source = hydration();
		const viewBinding = source.surface.muij_document.layout[0].children?.[0].props.viewBinding as {
			fieldBindings: Array<Record<string, unknown>>;
		};
		viewBinding.fieldBindings.push({
			field: 'details', kind: 'markdown', required: false, nullable: true, sortable: false
		});
		source.page.envelope.value[0].fields.details = 'Full editable payload';
		const page = hydrateAppSurface(source, {
			page: 2,
			pageSize: 25,
			hasNextPage: true,
			sortField: 'title',
			sortDirection: 'descending'
		});
		const query = page.components[0].children?.[0];
		expect(query?.props.rows).toEqual([{
			__record_id: 'record_1',
			__record_revision: 7,
			title: 'One',
			status: 'open',
			details: 'Full editable payload'
		}]);
		expect(query?.props).toMatchObject({
			paginationMode: 'server',
			currentPage: 2,
			pageCount: 3,
			pageCountExact: false,
			sortKey: 'title',
			sortDir: 'desc'
		});
		expect(surfaceInteractionIsAppOwned({
			componentId: 'app-query',
			interaction: 'action',
			detail: {},
			sent: false
		})).toBe(true);
		expect(surfaceInteractionIsAppOwned({
			componentId: 'app-query',
			interaction: 'action',
			detail: {},
			sent: true
		})).toBe(false);
	});

	it('rejects a second data owner and a document beyond the iterative depth bound', () => {
		const duplicate = hydration();
		duplicate.surface.muij_document.layout.push({
			id: 'app-query-2',
			component_type: 'EntityGrid',
			source: 'app_entity_query_v1',
			query: 'view:items',
			props: {}
		});
		expect(() => hydrateAppSurface(duplicate, { page: 1, pageSize: 25, hasNextPage: true })).toThrow(/multiple data owners/);

		const deep = hydration();
		let node = deep.surface.muij_document.layout[0];
		for (let index = 0; index < 33; index += 1) {
			const child: MuijComponent = { id: `deep-${index}`, component_type: 'Container', props: {}, children: [] };
			node.children = [child];
			node = child;
		}
		expect(() => hydrateAppSurface(deep, { page: 1, pageSize: 25, hasNextPage: true })).toThrow(/too deep/);
	});

	it('hydrates only the closed bounded declarative component binding', () => {
		const declarative = hydration('Stack');
		const query = declarative.surface.muij_document.layout[0].children?.[0];
		if (!query) throw new Error('missing query fixture');
		query.props.viewBinding = {
			...(query.props.viewBinding as Record<string, unknown>),
			surfaceComponents: [
				{
					kind: 'section', id: 'overview', label: 'Overview', children: [
						{ kind: 'detail', id: 'item_detail', label: 'Item Detail', fields: ['title', 'status'] },
						{ kind: 'form', id: 'create_item', label: 'Create Item', fields: ['title', 'status'] }
					]
				},
				{ kind: 'list', id: 'item_list', label: 'Item List', fields: ['title'] },
				{ kind: 'table', id: 'item_table', label: 'Item Table', columns: ['status', 'title'] }
			]
		};
		const rendered = hydrateAppSurface(declarative, { page: 1, pageSize: 25, hasNextPage: true });
		expect(rendered.view.surfaceComponents).toHaveLength(3);
		expect(rendered.view.surfaceComponents[0]).toMatchObject({ kind: 'section', label: 'Overview' });
		expect(query.props.rows).toEqual([]);

		const unknownField = structuredClone(declarative);
		const unknownQuery = unknownField.surface.muij_document.layout[0].children?.[0];
		const binding = unknownQuery?.props.viewBinding as Record<string, unknown>;
		const surfaceComponents = binding.surfaceComponents as Array<Record<string, unknown>>;
		surfaceComponents[1].transport = 'https://attacker.invalid';
		expect(() => hydrateAppSurface(unknownField, { page: 1, pageSize: 25, hasNextPage: true })).toThrow(/component fields are invalid/);

		const tooMany = structuredClone(declarative);
		const tooManyQuery = tooMany.surface.muij_document.layout[0].children?.[0];
		const tooManyBinding = tooManyQuery?.props.viewBinding as Record<string, unknown>;
		tooManyBinding.surfaceComponents = Array.from({ length: 65 }, (_, index) => ({
			kind: 'detail', id: `detail_${index}`, label: `Detail ${index}`, fields: ['title']
		}));
		expect(() => hydrateAppSurface(tooMany, { page: 1, pageSize: 25, hasNextPage: true })).toThrow(/node bound/);
	});

	it('rejects malformed records and parent cycles without recursive traversal', () => {
		const malformed = hydration();
		malformed.page.envelope.value[0].entity = 'other';
		expect(() => hydrateAppSurface(malformed, { page: 1, pageSize: 25, hasNextPage: true })).toThrow(/invalid record page/);

		const cyclic = hydration('Tree');
		const query = cyclic.surface.muij_document.layout[0].children?.[0];
		if (!query) throw new Error('missing query fixture');
		query.props.viewBinding = {
			entity: 'item',
			viewKind: 'tree',
			fields: ['title', 'parent'],
			fieldBindings: [
				{ field: 'title', kind: 'text', required: true, nullable: false, sortable: true },
				{ field: 'parent', kind: 'reference', required: false, nullable: true, sortable: true, referenceEntity: 'item' }
			],
			labelField: 'title',
			parentField: 'parent',
			maxDepth: 3
		};
		cyclic.page.envelope.value = [
			{ entity: 'item', record_id: 'one', record_revision: 1, fields: { title: 'One', parent: 'two' } },
			{ entity: 'item', record_id: 'two', record_revision: 1, fields: { title: 'Two', parent: 'one' } }
		];
		cyclic.page.next_cursor = undefined;
		expect(() => hydrateAppSurface(cyclic, { page: 1, pageSize: 25, hasNextPage: false })).toThrow(/parent cycle/);

		const tooDeep = hydration('Tree');
		const tooDeepQuery = tooDeep.surface.muij_document.layout[0].children?.[0];
		if (!tooDeepQuery) throw new Error('missing deep tree query');
		tooDeepQuery.props.viewBinding = query.props.viewBinding;
		tooDeep.page.envelope.value = [
			{ entity: 'item', record_id: 'one', record_revision: 1, fields: { title: 'One' } },
			{ entity: 'item', record_id: 'two', record_revision: 1, fields: { title: 'Two', parent: 'one' } },
			{ entity: 'item', record_id: 'three', record_revision: 1, fields: { title: 'Three', parent: 'two' } },
			{ entity: 'item', record_id: 'four', record_revision: 1, fields: { title: 'Four', parent: 'three' } }
		];
		tooDeep.page.next_cursor = undefined;
		expect(() => hydrateAppSurface(tooDeep, { page: 1, pageSize: 25, hasNextPage: false })).toThrow(/declared depth/);

		const partial = hydration('Tree');
		const partialQuery = partial.surface.muij_document.layout[0].children?.[0];
		if (!partialQuery) throw new Error('missing tree query');
		partialQuery.props.viewBinding = query.props.viewBinding;
		partial.page.next_cursor = 'cursor:next';
		expect(() => hydrateAppSurface(partial, {
			page: 1,
			pageSize: 25,
			hasNextPage: true
		})).toThrow(/one canonical page/);
	});

	it('builds only the narrow surface mutation contract and coerces typed forms', () => {
		const request = buildSurfaceMutationRequest(hydration(), 'surface-client:test', {
			kind: 'update',
			record_id: 'record_1',
			expected_record_revision: 7,
			patch: { title: 'Two' }
		});
		expect(request).toEqual({
			protocol_version: '1',
			surface_revision: 4,
			view_id: 'items',
			client_mutation_id: 'surface-client:test',
			operation: {
				kind: 'update',
				record_id: 'record_1',
				expected_record_revision: 7,
				patch: { title: 'Two' }
			}
		});
		expect(request).not.toHaveProperty('entity');
		expect(request).not.toHaveProperty('installation_id');
		const fields = hydration().surface.muij_document.layout[0].children?.[0].props.viewBinding as { fieldBindings: Parameters<typeof coerceAppSurfaceForm>[0] };
		const values = coerceAppSurfaceForm(fields.fieldBindings, { status: 'done', title: 'Two' });
		expect(values).toEqual({ status: 'done', title: 'Two' });
		expect(diffAppSurfaceValues({ status: 'open', title: 'One' }, values)).toEqual({ status: 'done', title: 'Two' });
		const typedValues = coerceAppSurfaceForm([
			{ field: 'enabled', kind: 'boolean', required: true, nullable: true, sortable: true, allowedValues: [] },
			{ field: 'notes', kind: 'markdown', required: false, nullable: false, sortable: false, allowedValues: [] }
		], { enabled: '', notes: '  keep this spacing  ' });
		expect(typedValues).toEqual({ enabled: null, notes: '  keep this spacing  ' });
		const timestamp = '2026-08-17T10:11:12.345Z';
		expect(new Date(formatAppSurfaceTimestampInput(timestamp)).toISOString()).toBe(timestamp);
	});

	it('retains the exact idempotency key only when a save result is ambiguous', () => {
		const request = buildSurfaceMutationRequest(hydration(), 'surface-client:stable', {
			kind: 'create',
			values: { title: 'One', status: 'open' }
		});
		const networkFailure = new TypeError('connection closed');
		expect(appSurfaceMutationFailureKind(networkFailure)).toBe('ambiguous');
		expect(retainedAppSurfaceMutationRequest(request, networkFailure)).toBe(request);
		expect(retainedAppSurfaceMutationRequest(
			request,
			new AppSurfaceClientError('invalid', 422, 'invalid')
		)).toBeNull();
		expect(appSurfaceMutationFailureKind(
			new AppSurfaceClientError('stale', 409, 'stale')
		)).toBe('stale');
	});

	it('uses realtime only on browser origins and leaves custom schemes to bounded polling', () => {
		expect(appSurfaceRealtimeSupported('https:')).toBe(true);
		expect(appSurfaceRealtimeSupported('http:')).toBe(true);
		expect(appSurfaceRealtimeSupported('magapp:')).toBe(false);
	});

	it('rejects drifted per-view metadata before rendering', () => {
		const drifted = hydration();
		const query = drifted.surface.muij_document.layout[0].children?.[0];
		if (!query) throw new Error('missing query fixture');
		query.props.viewBinding = {
			...(query.props.viewBinding as Record<string, unknown>),
			labelField: 'title'
		};
		expect(() => hydrateAppSurface(drifted, { page: 1, pageSize: 25, hasNextPage: true })).toThrow(/view shape/);

		const hiddenSort = hydration();
		const hiddenBinding = hiddenSort.surface.muij_document.layout[0].children?.[0].props.viewBinding as {
			fieldBindings: Array<Record<string, unknown>>;
		};
		hiddenBinding.fieldBindings.push({
			field: 'details', kind: 'text', required: false, nullable: true, sortable: true
		});
		expect(() => hydrateAppSurface(hiddenSort, {
			page: 1,
			pageSize: 25,
			hasNextPage: true,
			sortField: 'details',
			sortDirection: 'ascending'
		})).toThrow(/sort is outside/);

		const mismatchedOwner = hydration();
		const mismatchedQuery = mismatchedOwner.surface.muij_document.layout[0].children?.[0];
		if (!mismatchedQuery) throw new Error('missing query fixture');
		mismatchedQuery.query = 'view:other';
		expect(() => hydrateAppSurface(
			mismatchedOwner,
			{ page: 1, pageSize: 25, hasNextPage: true }
		)).toThrow(/no data owner|does not match/);
	});

	it('applies bounded optimistic create, update, and delete projections', () => {
		const records = hydration().page.envelope.value;
		const updated = optimisticAppSurfaceRecords(records, 'item', 'surface-client:update', {
			kind: 'update',
			record_id: 'record_1',
			expected_record_revision: 7,
			patch: { title: 'Two' }
		}, 25);
		expect(updated[0].fields.title).toBe('Two');
		expect(records[0].fields.title).toBe('One');

		const created = optimisticAppSurfaceRecords(updated, 'item', 'surface-client:create', {
			kind: 'create',
			values: { title: 'New', status: 'open' }
		}, 1);
		expect(created).toHaveLength(1);
		expect(created[0].record_id).toBe('optimistic_surface-client_create');
		const optimisticHydration = hydration();
		optimisticHydration.page.envelope.value = created;
		expect(() => hydrateAppSurface(
			optimisticHydration,
			{ page: 1, pageSize: 25, hasNextPage: true }
		)).not.toThrow();

		const deleted = optimisticAppSurfaceRecords(updated, 'item', 'surface-client:delete', {
			kind: 'delete',
			record_id: 'record_1',
			expected_record_revision: 7
		}, 25);
		expect(deleted).toEqual([]);
	});

	it('keeps cursor and sort in the authenticated hydration request', async () => {
		const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(JSON.stringify(hydration()), {
			status: 200,
			headers: { 'Content-Type': 'application/json' }
		}));
		await fetchAppSurface('install_1', 'items/open', {
			cursor: 'cursor:next',
			sortField: 'title',
			sortDirection: 'descending'
		});
		const url = new URL(String(fetchMock.mock.calls[0][0]), 'http://localhost');
		expect(url.pathname).toBe('/api/magician/v2/apps/installations/install_1/surfaces/items/open');
		expect(url.searchParams.get('cursor')).toBe('cursor:next');
		expect(url.searchParams.get('sort_field')).toBe('title');
		expect(url.searchParams.get('sort_direction')).toBe('descending');
	});

	it('rejects declared and streamed oversized app responses before JSON parsing', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response('{}', {
			status: 200,
			headers: { 'Content-Length': String(8 * 1024 * 1024 + 1) }
		}));
		await expect(fetchAppSurface('install_1', '/')).rejects.toMatchObject({
			code: 'app_response_too_large'
		});

		const chunk = new Uint8Array(4 * 1024 * 1024 + 1);
		const stream = new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(chunk);
				controller.enqueue(chunk);
				controller.close();
			}
		});
		vi.mocked(fetch).mockResolvedValueOnce(new Response(stream, { status: 200 }));
		await expect(fetchAppSurface('install_1', '/')).rejects.toMatchObject({
			code: 'app_response_too_large'
		});
	});
});
