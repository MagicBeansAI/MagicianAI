import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	appLifecycleControls,
	appPackageExportAvailability,
	backupAppUpdate,
	commitAppPurge,
	commitAppDataImport,
	exportAppPackage,
	importAppPackage,
	prepareAppUpdatePlan,
	publishAppCandidate,
	previewAppPurge,
	fetchAppReenableReview,
	commitAppReenable,
	recoverRetainedAppLifecycleIntents,
	recoverRetainedAppDataImportCommits,
	runAppLifecycleOperation
} from './appLifecycle';
import type { AppDirectoryEntry } from './appDirectory';
import type { AppPurgePreview, AppPurgeTarget, AppUpdatePlanReceipt } from './appLifecycle';

const DIGEST = `blake3:${'a'.repeat(64)}`;
const OTHER_DIGEST = `blake3:${'b'.repeat(64)}`;
const PURGE_TARGETS: AppPurgeTarget[] = [
	'active_rows', 'append_only_record_revisions', 'database_wal_and_temp',
	'scalar_and_search_indexes', 'package_and_cache_bytes', 'retained_attachments',
	'export_archives', 'artifact_v2_references', 'memory_candidates_and_promotions',
	'analytics', 'debug_and_prompt_captures', 'evaluation_artifacts',
	'provider_side_continuations', 'routes_and_published_surfaces',
	'schedules_and_outbox', 'disclosure_sessions', 'directory_and_search_projections'
];

function updatePlan(state: AppUpdatePlanReceipt['state'] = 'dry_run_passed'): AppUpdatePlanReceipt {
	return {
		migration_run_id: 'migration-run:test', installation_id: 'install-one',
		attempt_id: 'attempt:update', attempt_kind: 'update', source_fence_digest: DIGEST,
		destination_package_revision_ref: 'package-revision:two', destination_schema_revision: 2,
		destination_dataset_generation: 2, permission_diff: {}, schema_diff_digest: DIGEST,
		surface_diff_digest: DIGEST, data_diff_digest: DIGEST, migration_operations: [],
		migration_plan_digest: null, update_plan_digest: OTHER_DIGEST, destructive: false,
		backup_required: false, dry_run_examined: 3, dry_run_representable: 3, state
	};
}

function purgePreview(): AppPurgePreview {
	return {
		protocol_version: 2,
		preview_ref: 'app-purge-preview:test',
		scope_binding_ref: 'scope:test',
		installation_id: 'install-one',
		installation_generation: 4,
		selection: { target: 'whole_installation' },
		inventory_entries: PURGE_TARGETS.map((target) => ({
			target, item_count: 0, byte_count: 0,
			maximum_classification: 'ordinary', inventory_digest: DIGEST
		})),
		inventory_snapshot_digest: DIGEST,
		preview_digest: DIGEST,
		observed_at: '2026-08-23T00:00:00Z',
		expires_at: '2026-08-23T00:05:00Z'
	};
}

function purgeReceipt(preview = purgePreview()) {
	const shared = new Set<AppPurgeTarget>([
		'database_wal_and_temp', 'package_and_cache_bytes', 'retained_attachments',
		'artifact_v2_references'
	]);
	const policy = new Set<AppPurgeTarget>([
		'export_archives', 'memory_candidates_and_promotions', 'analytics',
		'debug_and_prompt_captures', 'evaluation_artifacts'
	]);
	return {
		protocol_version: 2,
		receipt_ref: 'app-purge-receipt:test',
		approval_ref: 'app-purge-approval:test',
		scope_binding_ref: preview.scope_binding_ref,
		installation_id: preview.installation_id,
		installation_generation: preview.installation_generation,
		selection_kind: 'whole_installation',
		selection_digest: DIGEST,
		preview_digest: preview.preview_digest,
		outcomes: PURGE_TARGETS.map((target) => shared.has(target)
			? {
				target, status: 'retained_shared', affected_items: 0, affected_bytes: 0,
				outcome_digest: DIGEST, policy_or_ownership_ref: 'retention:shared-owner'
			}
			: policy.has(target)
				? {
					target, status: 'retained_by_policy', affected_items: 0, affected_bytes: 0,
					outcome_digest: DIGEST, policy_or_ownership_ref: 'retention:audit-policy'
				}
			: target === 'provider_side_continuations'
				? { target, status: 'provider_retention_unknown', affected_items: 0, affected_bytes: 0, outcome_digest: DIGEST }
				: { target, status: 'deleted', affected_items: 0, affected_bytes: 0, outcome_digest: DIGEST }),
		completion: 'completed_with_disclosed_retention',
		committed_at: '2026-08-23T00:00:01Z'
	};
}

function memoryStorage(): Storage {
	const values = new Map<string, string>();
	return {
		get length() { return values.size; },
		clear: () => values.clear(),
		getItem: (key) => values.get(key) ?? null,
		key: (index) => [...values.keys()][index] ?? null,
		removeItem: (key) => { values.delete(key); },
		setItem: (key, value) => { values.set(key, value); }
	};
}

function entry(status: AppDirectoryEntry['status']): AppDirectoryEntry {
	return {
		installation_id: 'install-one', name: 'One', description: 'One app',
		icon: { kind: 'monogram', value: 'O' }, package_version: '1.0.0',
		package_revision_ref: 'package:one', installation_generation: 4, status,
		views: [], actions: [], storage: {
			record_count: 0, revision_count: 0, payload_bytes: 0, attachment_bytes: 0
		}, record_count: 0, payload_bytes: 0
	};
}

describe('Apps lifecycle client', () => {
	afterEach(() => vi.unstubAllGlobals());

	it('shows unsupported owner operations with exact reasons instead of mounting authority', () => {
		expect(appLifecycleControls(entry('disabled'))).toContainEqual(expect.objectContaining({
			operation: 'reenable', available: true, label: expect.stringContaining('Review')
		}));
		expect(appLifecycleControls(entry('uninstalled_retained'))).toContainEqual(expect.objectContaining({
			operation: 'purge', available: true, dangerous: true
		}));
		expect(appPackageExportAvailability(entry('ready_for_review'))).toEqual(expect.objectContaining({
			available: false, reason: expect.stringContaining('awaits review')
		}));
		expect(appPackageExportAvailability(entry('quarantined'))).toEqual({ available: true });
	});

	it('accepts only the exact generation-bearing lifecycle receipt', async () => {
		vi.stubGlobal('localStorage', memoryStorage());
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
			installation_id: 'install-one', generation: 5, status: 'disabled', review_identity: null
		}), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(runAppLifecycleOperation(entry('enabled'), 'disable')).resolves.toEqual({
			installation_id: 'install-one', generation: 5, status: 'disabled', review_identity: null
		});
		expect(fetchMock.mock.calls[0]?.[0]).toBe('/api/magician/v2/apps/installations/install-one/disable');

		fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
			installation_id: 'install-one', generation: 5, status: 'disabled', task_id: 'raw'
		}), { status: 200 }));
		await expect(runAppLifecycleOperation(entry('enabled'), 'disable')).rejects.toThrow(/did not match/i);

		fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
			installation_id: 'install-one', generation: 6, status: 'disabled', review_identity: null
		}), { status: 200 }));
		await expect(runAppLifecycleOperation(entry('enabled'), 'disable')).rejects.toThrow(/did not match/i);
	});

	it('retains the exact reviewed re-enable body across response loss', async () => {
		const storage = memoryStorage();
		vi.stubGlobal('localStorage', storage);
		const review = {
			installation_id: 'install-one', installation_generation: 4,
			package_id: 'package:one', package_version: '1.0.0',
			package_content_digest: DIGEST, package_lock_digest: DIGEST,
			grant_revision: 1, grant_identity_digest: DIGEST,
			schema_revision: 1, schema_identity_digest: DIGEST,
			surface_revision: 1, surface_identity_digest: DIGEST,
			global_policy_revision: 1, implementation_identity_digest: DIGEST,
			review_digest: DIGEST
		};
		const fetchMock = vi.fn()
			.mockResolvedValueOnce(new Response(JSON.stringify(review), { status: 200 }))
			.mockRejectedValueOnce(new TypeError('connection reset'))
			.mockResolvedValueOnce(new Response(JSON.stringify({
				installation_id: 'install-one', generation: 5, status: 'enabled', review_identity: review
			}), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		const disabled = entry('disabled');
		const displayed = await fetchAppReenableReview(disabled);
		await expect(commitAppReenable(disabled, displayed)).rejects.toThrow(/connection reset/i);
		const firstBody = (fetchMock.mock.calls[1]?.[1] as RequestInit).body;
		await expect(commitAppReenable(disabled, displayed)).resolves.toMatchObject({ status: 'enabled' });
		expect((fetchMock.mock.calls[2]?.[1] as RequestInit).body).toBe(firstBody);
		expect(storage.length).toBe(0);
	});

	it('replays every retained lifecycle intent after a full-page response loss', async () => {
		const storage = memoryStorage();
		vi.stubGlobal('localStorage', storage);
		const review = {
			installation_id: 'install-one', installation_generation: 4,
			package_id: 'package:one', package_version: '1.0.0',
			package_content_digest: DIGEST, package_lock_digest: DIGEST,
			grant_revision: 1, grant_identity_digest: DIGEST,
			schema_revision: 1, schema_identity_digest: DIGEST,
			surface_revision: 1, surface_identity_digest: DIGEST,
			global_policy_revision: 1, implementation_identity_digest: DIGEST,
			review_digest: DIGEST
		};
		vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('response lost')));
		for (const [operation, status] of [
			['disable', 'enabled'], ['quarantine', 'enabled'], ['uninstall_retain', 'enabled'],
			['begin_update', 'enabled'], ['abort_update', 'update_pending']
		] as const) {
			await expect(runAppLifecycleOperation(entry(status), operation)).rejects.toThrow(/response lost/i);
		}
		await expect(commitAppReenable(entry('disabled'), review)).rejects.toThrow(/response lost/i);
		expect(storage.length).toBe(6);

		const replay = vi.fn(async (url: string, init: RequestInit) => {
			const request = JSON.parse(String(init.body)) as { expected_generation: number };
			const operation = url.split('/').at(-1);
			const status = operation === 'disable' ? 'disabled'
				: operation === 'quarantine' ? 'quarantined'
					: operation === 'uninstall' ? 'uninstalled_retained'
						: operation === 'update-begin' ? 'update_pending'
							: operation === 'update-abort' ? 'enabled' : 'enabled';
			return new Response(JSON.stringify({
				installation_id: 'install-one', generation: request.expected_generation + 1,
				status, review_identity: operation === 'reenable' ? review : null
			}), { status: 200 });
		});
		vi.stubGlobal('fetch', replay);
		const recovered = await recoverRetainedAppLifecycleIntents();
		expect(recovered.errors).toEqual([]);
		expect(recovered.receipts).toHaveLength(6);
		expect(storage.length).toBe(0);
	});

	it('imports only a bounded archive and proves foreign grants remain inert', async () => {
		const body = {
			state: 'staged_for_local_conformance', stage_outcome: 'created', package_id: 'package:one',
			source_publisher_identity: 'publisher:one', semantic_version: '1.0.0',
			package_content_digest: `blake3:${'a'.repeat(64)}`,
			requirements: {
				reverify_complete_bundle_digest: true, run_local_conformance: true,
				run_local_permission_review: true,
				rebuild_verify_and_sandbox_executable_content_if_present: true,
				foreign_grants_transfer: false, portable_evidence_is_advisory_only: true
			},
			local_identity_resolution_required: true, foreign_authority_transferred: false
		};
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 201 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(importAppPackage(new Blob([new Uint8Array([1, 2, 3])]))).resolves.toMatchObject({
			state: 'staged_for_local_conformance', foreign_authority_transferred: false
		});
		const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
		expect(new Headers(init.headers).get('Content-Type')).toBe('application/vnd.app-platform.package+zip');
	});

	it('publishes the exact staged bytes only as an inert review candidate', async () => {
		vi.stubGlobal('localStorage', memoryStorage());
		const staged = {
			state: 'staged_for_local_conformance' as const, stage_outcome: 'created' as const,
			package_id: 'package:one', source_publisher_identity: 'publisher:one',
			semantic_version: '1.0.0', package_content_digest: DIGEST,
			requirements: {
				reverify_complete_bundle_digest: true as const, run_local_conformance: true as const,
				run_local_permission_review: true as const,
				rebuild_verify_and_sandbox_executable_content_if_present: true as const,
				foreign_grants_transfer: false as const, portable_evidence_is_advisory_only: true as const
			},
			local_identity_resolution_required: true as const, foreign_authority_transferred: false as const
		};
		const requestId = 'candidate-request:test';
		const response = {
			request_id: requestId, state: 'ready_for_review', stage_outcome: 'already_present',
			publication_outcome: 'created', package_revision_ref: 'package-revision:one',
			attempt_id: 'attempt:one', installation_id: 'install-one', package_content_digest: DIGEST,
			dependency_lock_digest: DIGEST, local_publisher_identity: 'actor:owner',
			source_publisher_identity: 'publisher:one', activation_authority_granted: false
		};
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(response), { status: 201 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(publishAppCandidate(new Blob([new Uint8Array([1])]), staged, requestId))
			.resolves.toMatchObject({ state: 'ready_for_review', activation_authority_granted: false });
		const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
		const headers = new Headers(init.headers);
		expect(headers.get('X-Magician-App-Package-Id')).toBe('package:one');
		expect(headers.get('X-Magician-App-Content-Digest')).toBe(DIGEST);
	});

	it('binds an update candidate to the exact parked installation and attempt kind', async () => {
		vi.stubGlobal('localStorage', memoryStorage());
		const staged = {
			state: 'staged_for_local_conformance' as const, stage_outcome: 'created' as const,
			package_id: 'package:one', source_publisher_identity: 'publisher:one',
			semantic_version: '2.0.0', package_content_digest: DIGEST,
			requirements: {
				reverify_complete_bundle_digest: true as const, run_local_conformance: true as const,
				run_local_permission_review: true as const,
				rebuild_verify_and_sandbox_executable_content_if_present: true as const,
				foreign_grants_transfer: false as const, portable_evidence_is_advisory_only: true as const
			},
			local_identity_resolution_required: true as const, foreign_authority_transferred: false as const
		};
		const response = {
			request_id: 'candidate-request:update', state: 'update_pending', stage_outcome: 'already_present',
			publication_outcome: 'created', package_revision_ref: 'package-revision:two',
			attempt_id: 'attempt:update', installation_id: 'install-one', package_content_digest: DIGEST,
			dependency_lock_digest: DIGEST, local_publisher_identity: 'actor:owner',
			source_publisher_identity: 'publisher:one', activation_authority_granted: false
		};
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(response), { status: 201 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(publishAppCandidate(
			new Blob([new Uint8Array([1])]), staged, 'candidate-request:update',
			{ installation_id: 'install-one', attempt_kind: 'update' }
		)).resolves.toMatchObject({ state: 'update_pending', installation_id: 'install-one' });
		const headers = new Headers((fetchMock.mock.calls[0]?.[1] as RequestInit).headers);
		expect(headers.get('X-Magician-App-Target-Installation')).toBe('install-one');
		expect(headers.get('X-Magician-App-Attempt-Kind')).toBe('update');
	});

	it('keeps plan preparation and encrypted backup on one exact digest', async () => {
		const prepared = updatePlan();
		const ready = { ...prepared, backup_required: true, destructive: true, state: 'ready_to_switch' };
		const fetchMock = vi.fn()
			.mockResolvedValueOnce(new Response(JSON.stringify(prepared), { status: 200 }))
			.mockResolvedValueOnce(new Response(JSON.stringify(ready), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		const plan = await prepareAppUpdatePlan(
			{ installation_id: 'install-one', installation_generation: 4 }, 'attempt:update'
		);
		expect(JSON.parse(String((fetchMock.mock.calls[0]?.[1] as RequestInit).body))).toEqual({
			attempt_id: 'attempt:update', expected_parked_generation: 4, migration_operations: []
		});
		await expect(backupAppUpdate(plan, 'correct horse battery staple'))
			.resolves.toMatchObject({ update_plan_digest: OTHER_DIGEST, state: 'ready_to_switch' });
		const backupHeaders = new Headers((fetchMock.mock.calls[1]?.[1] as RequestInit).headers);
		expect(backupHeaders.get('X-Magician-App-Archive-Passphrase')).toBe('correct horse battery staple');
	});

	it('replays one exact reviewed data-import commit after response loss', async () => {
		const storage = memoryStorage();
		vi.stubGlobal('localStorage', storage);
		const requestId = 'data-import-commit:test';
		const receipt = {
			request_id: requestId, foreign_authority_transferred: false,
			receipt: {
				receipt_ref: 'receipt:data-import:test', approval_ref: 'approval:data-import:test',
				preview_digest: DIGEST, destination_installation_id: 'install-one',
				destination_installation_generation: 4, source_record_count: 2,
				created_count: 1, merged_count: 0, skipped_count: 1
			}
		};
		const fetchMock = vi.fn()
			.mockRejectedValueOnce(new TypeError('response lost'))
			.mockResolvedValueOnce(new Response(JSON.stringify(receipt), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(commitAppDataImport(
			'install-one', DIGEST, 'approval:data-import:test', requestId
		)).rejects.toThrow(/response lost/i);
		expect(storage.length).toBe(1);
		const recovered = await recoverRetainedAppDataImportCommits();
		expect(recovered.errors).toEqual([]);
		expect(recovered.receipts).toHaveLength(1);
		expect(storage.length).toBe(0);
		expect((fetchMock.mock.calls[1]?.[1] as RequestInit).body)
			.toBe((fetchMock.mock.calls[0]?.[1] as RequestInit).body);
	});

	it('rejects truncated or undeclared package exports', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(new Uint8Array([1, 2]), {
			status: 200,
			headers: {
				'content-type': 'application/vnd.app-platform.package+zip',
				'content-length': '3'
			}
		})));
		await expect(exportAppPackage('install-one')).rejects.toThrow(/truncated/i);
	});

	it('requires an exact complete server purge inventory', async () => {
		const preview = purgePreview();
		preview.inventory_entries = preview.inventory_entries.slice(1);
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(preview), {
			status: 200
		})));
		await expect(previewAppPurge(entry('uninstalled_retained'))).rejects.toThrow(/did not match/i);
	});

	it('retains one exact purge identity across an ambiguous POST and clears it after replay', async () => {
		const preview = purgePreview();
		const storage = memoryStorage();
		vi.stubGlobal('localStorage', storage);
		const fetchMock = vi.fn()
			.mockRejectedValueOnce(new TypeError('connection reset'))
			.mockResolvedValueOnce(new Response(JSON.stringify(purgeReceipt(preview)), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(commitAppPurge(entry('uninstalled_retained'), preview)).rejects.toThrow(/connection reset/i);
		expect(storage.length).toBe(1);
		const reopened = await previewAppPurge(entry('uninstalled_retained'));
		expect(reopened).toEqual(preview);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		await expect(commitAppPurge(entry('uninstalled_retained'), reopened)).resolves.toMatchObject({
			completion: 'completed_with_disclosed_retention'
		});
		const firstBody = (fetchMock.mock.calls[0]?.[1] as RequestInit).body;
		const secondBody = (fetchMock.mock.calls[1]?.[1] as RequestInit).body;
		expect(secondBody).toBe(firstBody);
		expect(storage.length).toBe(0);
	});

	it('rejects a receipt that retains a canonical installation authority surface', async () => {
		const preview = purgePreview();
		const receipt = purgeReceipt(preview);
		receipt.outcomes = receipt.outcomes.map((outcome) => outcome.target === 'active_rows'
			? { ...outcome, status: 'retained_shared', policy_or_ownership_ref: 'retention:forged' }
			: outcome) as typeof receipt.outcomes;
		vi.stubGlobal('localStorage', memoryStorage());
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(receipt), {
			status: 200
		})));
		await expect(commitAppPurge(entry('uninstalled_retained'), preview)).rejects.toThrow(/authority surface/i);
	});
});
