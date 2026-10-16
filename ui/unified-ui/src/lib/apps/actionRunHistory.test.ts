import { describe, expect, it } from 'vitest';
import {
	appActionSubmissionIsCurrent,
	clearAppActionLaunchIntent,
	getAppActionRunStorage,
	launchIntentInput,
	loadAppActionLaunchIntent,
	loadAppActionRunHistory,
	rememberAppActionRun,
	restoreAppActionRun,
	stageAppActionLaunchIntent,
	type AppActionRunStorage
} from './actionRunHistory';
import type { AppActionRun } from './appDirectory';

interface MemoryStorage extends AppActionRunStorage {
	dump(): string;
}

function memoryStorage(): MemoryStorage {
	const values = new Map<string, string>();
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => values.set(key, value),
		dump: () => [...values.values()].join('')
	};
}

function run(index: number, terminal = false): AppActionRun {
	const taskId = `task_app_${String(index).padStart(64, '0')}`;
	return {
		run_handle: {
			protocol_version: '1',
			run_ref: `run:app-action:${taskId}`,
			installation_id: 'install-plan',
			action_id: 'create_plan'
		},
		run_ref: `run:app-action:${taskId}`,
		status: terminal ? 'completed' : 'running',
		terminal,
		result_withheld: false,
		...(terminal ? { result: {
			action_id: 'create_plan', run_ref: `run:app-action:${taskId}`,
			status: 'completed' as const,
			output: {
				protocol_version: '1' as const,
				source: 'app_action' as const,
				scope_binding_ref: 'scope:one',
				installation_id: 'install-plan',
				package_revision_ref: 'package:one',
				schema_revision: 1,
				grant_revision: 1,
				value_schema_ref: 'schema:result',
				value: { secret: 'must-not-persist' },
				source_refs: [],
				handling_labels: {
					classification: 'ordinary' as const,
					model_processing: 'none' as const,
					policy_digest: `blake3:${'1'.repeat(64)}`,
					provenance_digest: `blake3:${'2'.repeat(64)}`
				},
				content_digest: `blake3:${'3'.repeat(64)}`,
				produced_at: '2026-08-23T00:00:00Z'
			},
			mutation_receipt_refs: [],
			external_effect_receipt_refs: []
		} } : {})
	};
}

describe('app action run recovery history', () => {
	it('treats a throwing localStorage getter as unavailable', () => {
		const denied = Object.defineProperty({}, 'localStorage', {
			get: () => { throw new Error('denied'); }
		}) as { readonly localStorage: AppActionRunStorage };
		expect(getAppActionRunStorage(denied)).toBeNull();
		expect(getAppActionRunStorage(null)).toBeNull();
	});

	it('requires both captured scope and monotonic generation for UI ownership', () => {
		expect(appActionSubmissionIsCurrent(7, 'scope-a', 7, 'scope-a')).toBe(true);
		expect(appActionSubmissionIsCurrent(8, 'scope-a', 7, 'scope-a')).toBe(false);
		expect(appActionSubmissionIsCurrent(7, 'scope-b', 7, 'scope-a')).toBe(false);
	});

	it('persists only non-bearer control metadata and restores it per scope', () => {
		const storage = memoryStorage();
		const unsafe = run(1, true);
		unsafe.run_handle = {
			...unsafe.run_handle!,
			secret_capability: 'must-not-persist'
		} as NonNullable<AppActionRun['run_handle']>;
		rememberAppActionRun(storage, 'scope-a', unsafe, 1_000);

		const history = loadAppActionRunHistory(storage, 'scope-a', 1_001);
		expect(history).toHaveLength(1);
		expect(JSON.stringify(history)).not.toContain('must-not-persist');
		expect(storage.dump()).not.toContain('secret_capability');
		expect(Object.keys(history[0]!.run_handle).sort()).toEqual([
			'action_id', 'installation_id', 'protocol_version', 'run_ref'
		]);
		const restored = restoreAppActionRun(history[0]!);
		expect(restored).toMatchObject({ status: 'completed', terminal: true });
		expect(restored.result).toBeUndefined();
		expect(loadAppActionRunHistory(storage, 'scope-b', 1_001)).toEqual([]);
	});

	it('deduplicates updates and bounds one scope without evicting another first', () => {
		const storage = memoryStorage();
		rememberAppActionRun(storage, 'scope-b', run(100), 100);
		for (let index = 1; index <= 20; index += 1) {
			rememberAppActionRun(storage, 'scope-a', run(index), 100 + index);
		}
		rememberAppActionRun(storage, 'scope-a', { ...run(20), status: 'waiting' }, 200);

		const scopeA = loadAppActionRunHistory(storage, 'scope-a', 201);
		expect(scopeA).toHaveLength(16);
		expect(scopeA[0]?.status).toBe('waiting');
		expect(loadAppActionRunHistory(storage, 'scope-b', 201)).toHaveLength(1);
	});

	it('drops expired and malformed entries fail closed', () => {
		const storage = memoryStorage();
		rememberAppActionRun(storage, 'scope-a', run(1), 1_000);
		expect(loadAppActionRunHistory(storage, 'scope-a', 8 * 24 * 60 * 60 * 1_000)).toEqual([]);
		const malformed: AppActionRun = { ...run(2), run_ref: 'different' };
		rememberAppActionRun(storage, 'scope-a', malformed, 2_000);
		expect(loadAppActionRunHistory(storage, 'scope-a', 2_001)).toEqual([]);
	});

	it('requires terminal and disclosure flags to agree with status', () => {
		const storage = memoryStorage();
		rememberAppActionRun(storage, 'scope-a', {
			...run(1), status: 'completed', terminal: false
		}, 1_000);
		rememberAppActionRun(storage, 'scope-a', {
			...run(2), status: 'running', terminal: true
		}, 1_001);
		rememberAppActionRun(storage, 'scope-a', {
			...run(3), result_withheld: true
		}, 1_002);
		expect(loadAppActionRunHistory(storage, 'scope-a', 1_003)).toEqual([]);

		rememberAppActionRun(storage, 'scope-a', {
			...run(4, true), result_withheld: true
		}, 1_004);
		expect(loadAppActionRunHistory(storage, 'scope-a', 1_005)).toMatchObject([
			{ status: 'completed', terminal: true, result_withheld: true }
		]);
	});

	it('evicts the oldest entries until the encoded UTF-8 payload fits', () => {
		const storage = memoryStorage();
		const scopes: string[] = [];
		for (let index = 1; index <= 64; index += 1) {
			const scope = `scope-${index}:${'🙂'.repeat(480)}`;
			scopes.push(scope);
			rememberAppActionRun(storage, scope, run(index), index);
		}

		expect(new TextEncoder().encode(storage.dump()).byteLength).toBeLessThanOrEqual(64 * 1_024);
		expect(loadAppActionRunHistory(storage, scopes[scopes.length - 1]!, 65)).toHaveLength(1);
		expect(loadAppActionRunHistory(storage, scopes[0]!, 65)).toEqual([]);
	});

	it('durably retains exact pre-dispatch input and clears only the matching recovered launch', () => {
		const storage = memoryStorage();
		const intent = stageAppActionLaunchIntent(storage, {
			scope_key: 'scope-a',
			installation_id: 'install-plan',
			installation_generation: 7,
			package_revision_ref: 'package:seven',
			action_id: 'create_plan',
			idempotency_key: 'action-request:one',
			input: { title: 'Plan', nested: { order: 1 } }
		}, 1_000);

		expect(loadAppActionLaunchIntent(storage, 'scope-a', 'install-plan', 'create_plan', 1_001))
			.toEqual(intent);
		expect(launchIntentInput(intent)).toEqual({ nested: { order: 1 }, title: 'Plan' });
		expect(() => stageAppActionLaunchIntent(storage, {
			scope_key: 'scope-a', installation_id: 'install-plan', installation_generation: 7,
			package_revision_ref: 'package:seven', action_id: 'create_plan',
			idempotency_key: 'action-request:two', input: { title: 'Substituted' }
		}, 1_001)).toThrow(/recover the previous/i);

		clearAppActionLaunchIntent(storage, intent, 1_002);
		expect(loadAppActionLaunchIntent(storage, 'scope-a', 'install-plan', 'create_plan', 1_003))
			.toBeNull();
	});

	it('refuses dispatch intent staging when durable storage cannot verify the write', () => {
		const storage: AppActionRunStorage = {
			getItem: () => null,
			setItem: () => undefined
		};
		expect(() => stageAppActionLaunchIntent(storage, {
			scope_key: 'scope-a', installation_id: 'install-plan', installation_generation: 7,
			package_revision_ref: 'package:seven', action_id: 'create_plan',
			idempotency_key: 'action-request:one', input: { title: 'Plan' }
		}, 1_000)).toThrow(/could not be verified/i);
	});
});
