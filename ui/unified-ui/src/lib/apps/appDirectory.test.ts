import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	AppRequestError,
	cancelAppActionRun,
	fetchAppActionContract,
	fetchAppActionRun,
	fetchAppDirectory,
	launchAppAction,
	isRetryableAppRequestError,
	parseAppDirectoryPage,
	setAppDirectoryActionPin
} from './appDirectory';

function fixture(): Record<string, unknown> {
	return {
		entries: [{
			installation_id: 'install-plan',
			name: 'learning-plan',
			description: 'A bounded plan app',
			icon: { kind: 'monogram', value: 'L' },
			package_version: '1.0.0',
			package_revision_ref: 'package-revision:fixture',
			installation_generation: 2,
			status: 'enabled',
			default_route: '/apps/install-plan',
			views: [{ view_id: 'plans', label: 'Plans', route: '/apps/install-plan', pinned: true }],
			actions: [{ action_id: 'create_plan', label: 'Create Plan', pinned: false }],
			last_opened_at: '2026-08-17T00:00:00.000000000Z',
			permissions: {
				granted_tools: 2,
				granted_context_reads: 1,
				granted_personal_data_projections: 0,
				background_execution: false,
				network_access: true
			},
			storage: { record_count: 3, revision_count: 4, payload_bytes: 512, attachment_bytes: 0 },
			record_count: 3,
			payload_bytes: 512
		}],
		next_cursor: 'opaque-cursor',
		has_more: true
	};
}

describe('Apps directory transport', () => {
	afterEach(() => vi.unstubAllGlobals());

	it('accepts only the bounded metadata-only directory shape', () => {
		const page = parseAppDirectoryPage(fixture());
		expect(page.entries[0]?.name).toBe('learning-plan');
		expect(page.entries[0]?.views[0]?.pinned).toBe(true);
	});

	it('rejects record payload leakage and unknown nested fields', () => {
		const withPayload = fixture();
		(withPayload.entries as Array<Record<string, unknown>>)[0]!.records = [{ secret: 'no' }];
		expect(() => parseAppDirectoryPage(withPayload)).toThrow(/invalid response/i);

		const withUnknownViewField = fixture();
		const entry = (withUnknownViewField.entries as Array<Record<string, unknown>>)[0]!;
		(entry.views as Array<Record<string, unknown>>)[0]!.payload = { secret: 'no' };
		expect(() => parseAppDirectoryPage(withUnknownViewField)).toThrow(/invalid response/i);
	});

	it('requires a continuation cursor whenever the server says more exists', () => {
		const missingCursor = fixture();
		delete missingCursor.next_cursor;
		expect(() => parseAppDirectoryPage(missingCursor)).toThrow(/invalid response/i);
	});

	it('rejects cursor, route, identity, and collection inconsistencies', () => {
		const staleCursor = fixture();
		staleCursor.has_more = false;
		expect(() => parseAppDirectoryPage(staleCursor)).toThrow(/invalid response/i);

		const foreignRoute = fixture();
		const foreignEntry = (foreignRoute.entries as Array<Record<string, unknown>>)[0]!;
		(foreignEntry.views as Array<Record<string, unknown>>)[0]!.route = '/apps/install-other';
		expect(() => parseAppDirectoryPage(foreignRoute)).toThrow(/invalid response/i);

		const duplicateView = fixture();
		const duplicateEntry = (duplicateView.entries as Array<Record<string, unknown>>)[0]!;
		duplicateEntry.views = [
			...(duplicateEntry.views as Array<Record<string, unknown>>),
			{ view_id: 'plans', label: 'Duplicate', route: '/apps/install-plan/other', pinned: false }
		];
		expect(() => parseAppDirectoryPage(duplicateView)).toThrow(/invalid response/i);

		const tooManyActions = fixture();
		const wideEntry = (tooManyActions.entries as Array<Record<string, unknown>>)[0]!;
		wideEntry.actions = Array.from({ length: 257 }, (_, index) => ({
			action_id: `action_${index}`,
			label: `Action ${index}`,
			pinned: false
		}));
		expect(() => parseAppDirectoryPage(tooManyActions)).toThrow(/invalid response/i);
	});

	it('rejects counters that disagree with the canonical storage summary', () => {
		const mismatch = fixture();
		(mismatch.entries as Array<Record<string, unknown>>)[0]!.record_count = 99;
		expect(() => parseAppDirectoryPage(mismatch)).toThrow(/invalid response/i);
	});

	// Gate S3's wire field. The shell mounts only what a directory entry
	// carries, so a server that never sends `navigation` renders exactly like a
	// deployment with no navigating apps: silent, and indistinguishable from
	// working. `appNavigation.test.ts` pins the grammar in isolation; nothing
	// pinned that the directory envelope actually carries a declaration.
	it('carries a declared navigation entry through the directory envelope', () => {
		const declaration = {
			id: 'meetings_console',
			title: 'Meetings',
			route: '/meetings-console',
			placement: { kind: 'section', section: 'observe' },
			surface: { kind: 'custom_surface', entry_point: '/console', fallback_view: 'sessions' }
		};
		const declared = fixture();
		(declared.entries as Array<Record<string, unknown>>)[0]!.navigation = [declaration];
		expect(parseAppDirectoryPage(declared).entries[0]?.navigation).toEqual([declaration]);

		// An omitted field means "declares none", never an undefined every
		// consumer has to guard: a server that predates the projection, and an
		// app with nothing to contribute, both mount nothing.
		expect(parseAppDirectoryPage(fixture()).entries[0]?.navigation).toEqual([]);
	});

	it('refuses the whole entry when a navigation declaration is off shape', () => {
		const base = {
			id: 'meetings_console',
			title: 'Meetings',
			route: '/meetings-console',
			placement: { kind: 'route' },
			surface: { kind: 'view', view: 'sessions' }
		};
		for (const invalid of [
			{ ...base, id: 'not a name' },
			{ ...base, title: '   ' },
			{ ...base, route: 'meetings-console' },
			{ ...base, placement: { kind: 'modal' } },
			{ ...base, surface: { kind: 'view', view: 'sessions', extra: 1 } }
		]) {
			const offShape = fixture();
			(offShape.entries as Array<Record<string, unknown>>)[0]!.navigation = [invalid];
			expect(() => parseAppDirectoryPage(offShape)).toThrow(/invalid response/i);
		}
	});

	it('binds the pinned-view projection to the server query', async () => {
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(fixture()), {
			status: 200,
			headers: { 'content-type': 'application/json' }
		}));
		vi.stubGlobal('fetch', fetchMock);

		await fetchAppDirectory({ section: 'pinned', pinnedTargetKind: 'view', limit: 8 });

		const [url] = fetchMock.mock.calls[0] as [string, RequestInit];
		expect(url).toContain('section=pinned');
		expect(url).toContain('pinned_target_kind=view');
		expect(url).toContain('limit=8');
	});

	it('rejects a pinned target filter outside the pinned section before transport', async () => {
		const fetchMock = vi.fn();
		vi.stubGlobal('fetch', fetchMock);

		await expect(fetchAppDirectory({
			section: 'installed',
			pinnedTargetKind: 'view',
			limit: 8
		})).rejects.toThrow(/valid only for pinned apps/i);
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('rejects an oversized directory response while streaming it', async () => {
		const oversized = new Uint8Array(4 * 1024 * 1024 + 1);
		const fetchMock = vi.fn().mockResolvedValue(new Response(oversized, { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(fetchAppDirectory({ section: 'installed', limit: 8 }))
			.rejects.toThrow(/exceeded its size limit/i);
	});

	it('rejects malformed or negative declared response lengths', async () => {
		for (const contentLength of ['not-a-number', '-1', '1.5']) {
			const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(fixture()), {
				status: 200,
				headers: { 'content-length': contentLength }
			}));
			vi.stubGlobal('fetch', fetchMock);

			await expect(fetchAppDirectory({ section: 'installed', limit: 8 }))
				.rejects.toThrow(/invalid response/i);
		}
	});

	it('loads a bounded typed action contract and rejects invented field metadata', async () => {
		const valid = {
			action_id: 'create_plan',
			input: {
				type: 'object',
				fields: {
					title: { type: 'text', required: true, nullable: false },
					priority: {
						type: 'enum', required: false, nullable: false, values: ['low', 'high']
					}
				}
			}
		};
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(valid), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		const contract = await fetchAppActionContract('install-plan', 'create_plan');
		expect(contract.input.fields.priority?.values).toEqual(['low', 'high']);

		const invalid = structuredClone(valid);
		(invalid.input.fields.title as Record<string, unknown>).payload = 'not schema metadata';
		fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(invalid), { status: 200 }));
		await expect(fetchAppActionContract('install-plan', 'create_plan'))
			.rejects.toThrow(/contract was invalid/i);
	});

	it('launches actions without accepting client-shaped authority fields', async () => {
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
			run_handle: {
				protocol_version: '1',
				run_ref: 'run:app-action:task-1',
				installation_id: 'install-plan',
				action_id: 'create_plan'
			},
			execution_id: 'execution-1',
		}), { status: 202 }));
		vi.stubGlobal('fetch', fetchMock);

		await launchAppAction(
			'install-plan',
			'create_plan',
			'action-request:one',
			{ title: 'Learn Rust' }
		);
		const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
		expect(JSON.parse(String(init.body))).toEqual({
			idempotency_key: 'action-request:one',
			input: { title: 'Learn Rust' }
		});
		expect(String(init.body)).not.toContain('grant_revision');
		expect(String(init.body)).not.toContain('policy_digest');
		expect(fetchMock.mock.calls[0]?.[0]).toBe(
			'/api/magician/v2/apps/installations/install-plan/actions/create_plan/runs'
		);
	});

	it('pins an action as an action target, not as a view', async () => {
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
			installation_id: 'install-plan',
			updated_at: '2026-08-21T00:00:00Z'
		}), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		await setAppDirectoryActionPin('install-plan', 'create_plan', true);
		const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
		expect(JSON.parse(String(init.body))).toEqual({
			kind: 'pin',
			target_kind: 'action',
			target_id: 'create_plan',
			pinned: true
		});
	});

	it('polls only through the supported-public opaque run route', async () => {
		const runRef = 'run:app-action:task-1';
		const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({
				protocol_version: '1',
				run_handle: {
					protocol_version: '1', run_ref: runRef,
					installation_id: 'install-plan', action_id: 'create_plan'
				},
				execution_id: 'execution-1', status: 'running', terminal: false,
				result_withheld: false
			}), { status: 202 }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(fetchAppActionRun(runRef)).resolves.toMatchObject({
			run_ref: runRef, status: 'running', terminal: false
		});
		expect(fetchMock.mock.calls[0]?.[0]).toBe('/api/magician/v2/apps/action-runs/run%3Aapp-action%3Atask-1');
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('keeps blocked runs nonterminal without inventing client cancellation', async () => {
		const runRef = 'run:app-action:task_app_' + 'a'.repeat(64);
		const runHandle = {
			protocol_version: '1', run_ref: runRef,
			installation_id: 'install-plan', action_id: 'create_plan'
		};
		const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({
				protocol_version: '1', run_handle: runHandle,
				execution_id: 'execution-1', status: 'blocked', terminal: false,
				result_withheld: false
			}), { status: 202 }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(fetchAppActionRun(runRef)).resolves.toMatchObject({
			run_ref: runRef, status: 'blocked', terminal: false
		});
	});

	it('posts and correlates one generation-bound cancellation receipt', async () => {
		const runRef = 'run:app-action:task_app_' + 'c'.repeat(64);
		const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({
			protocol_version: '1', run_ref: runRef, generation: 1,
			idempotency_key: 'cancel:fixture:1', status: 'cancelling',
			requested_at: '2026-08-23T00:00:00Z'
		}), { status: 202 }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(cancelAppActionRun(runRef, 0, 'cancel:fixture:1')).resolves.toMatchObject({
			generation: 1, status: 'cancelling'
		});
		const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
		expect(JSON.parse(String(init.body))).toEqual({
			expected_generation: 0, idempotency_key: 'cancel:fixture:1'
		});
	});

	it('requires completed snapshots to include a result or an explicit withheld marker', async () => {
		const runRef = 'run:app-action:task_app_' + 'b'.repeat(64);
		const runHandle = {
			protocol_version: '1', run_ref: runRef,
			installation_id: 'install-plan', action_id: 'create_plan'
		};
		const fetchMock = vi.fn()
			.mockResolvedValueOnce(new Response(JSON.stringify({
				protocol_version: '1', run_handle: runHandle,
				status: 'completed', terminal: true, result_withheld: false
			}), { status: 200 }))
			.mockResolvedValueOnce(new Response(JSON.stringify({
				protocol_version: '1', run_handle: runHandle,
				status: 'completed', terminal: true, result_withheld: true
			}), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(fetchAppActionRun(runRef)).rejects.toThrow('result-disclosure state');
		await expect(fetchAppActionRun(runRef)).resolves.toMatchObject({
			status: 'completed', terminal: true, result_withheld: true
		});
	});

	it('retries only transport and explicitly transient HTTP failures', () => {
		expect(isRetryableAppRequestError(new TypeError('network'))).toBe(true);
		expect(isRetryableAppRequestError(new AppRequestError('busy', 503))).toBe(true);
		expect(isRetryableAppRequestError(new AppRequestError('unsupported', 501))).toBe(false);
		expect(isRetryableAppRequestError(new AppRequestError('revoked', 403))).toBe(false);
		expect(isRetryableAppRequestError(new AppRequestError('stale', 409))).toBe(false);
		expect(isRetryableAppRequestError(new Error('invalid contract'))).toBe(false);
	});

	it('keeps polling when a typed action result is waiting', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
			protocol_version: '1',
			action_id: 'create_plan',
			run_ref: 'run:app-action:task-1',
			status: 'waiting',
			mutation_receipt_refs: [],
			external_effect_receipt_refs: []
		}), { status: 200 })));

		await expect(fetchAppActionRun('run:app-action:task-1')).resolves.toMatchObject({
			run_ref: 'run:app-action:task-1', status: 'waiting', terminal: false,
			result: { status: 'waiting' }
		});
	});

	it('preserves typed output labels and receipts while rejecting cross-installation output', async () => {
		const runRef = 'run:app-action:task-typed';
		const result = {
			protocol_version: '1', action_id: 'create_plan', run_ref: runRef, status: 'completed',
			output: {
				protocol_version: '1', source: 'app_action', scope_binding_ref: 'scope:one',
				installation_id: 'install-plan', package_revision_ref: 'package:one',
				schema_revision: 2, grant_revision: 3, value_schema_ref: 'schema:result',
				value: { plan: 'bounded' },
				source_refs: [{ kind: 'entity_record', reference: 'record:one', revision: 4, fields: ['title'] }],
				handling_labels: {
					classification: 'personal', model_processing: 'local_only',
					policy_digest: `blake3:${'a'.repeat(64)}`,
					provenance_digest: `blake3:${'b'.repeat(64)}`
				},
				content_digest: `blake3:${'c'.repeat(64)}`, produced_at: '2026-08-23T00:00:00Z'
			},
			mutation_receipt_refs: ['mutation:one'], external_effect_receipt_refs: []
		};
		const response = {
			protocol_version: '1',
			run_handle: { protocol_version: '1', run_ref: runRef, installation_id: 'install-plan', action_id: 'create_plan' },
			status: 'completed', terminal: true, result_withheld: false, result
		};
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(response), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(fetchAppActionRun(runRef)).resolves.toMatchObject({
			result: {
				output: { value: { plan: 'bounded' }, handling_labels: { classification: 'personal' } },
				mutation_receipt_refs: ['mutation:one']
			}
		});

		result.output.installation_id = 'install-other';
		fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(response), { status: 200 }));
		await expect(fetchAppActionRun(runRef)).rejects.toThrow(/invalid status result/i);
	});
});
