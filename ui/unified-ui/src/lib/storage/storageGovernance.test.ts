import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
	applyAttentionLearningRetention,
	attentionLearningMaintenanceReportFromUnknown,
	clearCompactionMetrics,
	compactDatabases,
	fetchStorageSnapshot,
	maintenanceStatusFromUnknown,
	formatStorageBytes,
	optimizeAttentionLearning,
	previewAttentionLearningRetention,
	reclaimAttentionLearningSpace,
	storageMaintenanceReportFromUnknown,
	storageSnapshotFromUnknown
} from './storageGovernance';

const compactionMetrics = {
	schema_version: 1,
	healthy: true,
	warning: null,
	storage_bytes: 640,
	allocated_bytes: 4096,
	event_count: 1,
	retained_event_limit: 256,
	total_bytes_reclaimed: 1024,
	total_files_compacted: 8,
	total_query_files_avoided: 7,
	total_compacted_bytes_written: 256,
	last_compaction_at_ms: 100,
	areas: [{
		kind: 'parquet', area: 'events', runs: 1, partitions_compacted: 1,
		files_compacted: 8, query_files_avoided: 7, bytes_reclaimed: 1024,
		compacted_bytes_written: 0, rows_compacted: 80, last_compacted_at_ms: 100
	}],
	recent_events: [{
		id: '01TEST', completed_at_ms: 100, trigger: 'manual', kind: 'parquet', area: 'events',
		partitions_compacted: 1, files_before: 9, files_after: 2, files_compacted: 8,
		query_files_avoided: 7, bytes_before: 2048, bytes_after: 1024, bytes_reclaimed: 1024,
		compacted_bytes_written: 0, rows_compacted: 80, duration_ms: 12
	}]
};

const snapshot = {
	principal: 'owner',
	workspace: 'default',
	generated_at_ms: 100,
	total_size_bytes: 2048,
	total_allocated_bytes: 1024,
	entries: [
		{
			id: 'analytics_duckdb',
			label: 'Analytics catalog',
			kind: 'duck_db',
			safety_class: 'regenerable',
			relative_path: 'analytics/analytics.duckdb',
			size_bytes: 2048,
			allocated_bytes: 1024,
			wal_bytes: 256,
			file_count: 1,
			inventory_complete: true,
			row_count: 42,
			oldest_partition: null,
			newest_partition: null,
			retention_days: null,
			policy: 'rebuildable',
			actions: []
		}
	],
	compaction_metrics: compactionMetrics,
	safeguards: ['verified']
};

describe('storage governance client', () => {
	beforeEach(() => {
		vi.restoreAllMocks();
		vi.stubGlobal('fetch', vi.fn());
	});

	it('strictly normalizes typed inventory and rejects unknown classes', () => {
		expect(storageSnapshotFromUnknown(snapshot).entries[0].row_count).toBe(42);
		expect(storageSnapshotFromUnknown(snapshot).entries[0].shm_bytes).toBeUndefined();
		expect(storageSnapshotFromUnknown({...snapshot, entries: [{...snapshot.entries[0], shm_bytes: 32768}]}).entries[0].shm_bytes).toBe(32768);
		expect(() => storageSnapshotFromUnknown({...snapshot, entries: [{...snapshot.entries[0], shm_bytes: 'invalid'}]})).toThrow();
		expect(
			storageSnapshotFromUnknown({
				...snapshot,
				entries: [
					{
						...snapshot.entries[0],
						id: 'app_packages',
						kind: 'directory',
						safety_class: 'lifecycle_managed'
					}
				]
			}).entries[0]
		).toMatchObject({ id: 'app_packages', kind: 'directory' });
		expect(() =>
			storageSnapshotFromUnknown({
				...snapshot,
				entries: [{ ...snapshot.entries[0], safety_class: 'delete_everything' }]
			})
		).toThrow('unknown entry classification');
		const { inventory_complete: _inventoryComplete, ...missingCompleteness } = snapshot.entries[0];
		expect(() =>
			storageSnapshotFromUnknown({ ...snapshot, entries: [missingCompleteness] })
		).toThrow('entry.inventory_complete');
	});

	it('formats byte sizes without pretending physical fragmentation is inferable', () => {
		expect(formatStorageBytes(1024 ** 3)).toBe('1.00 GB');
	});

	it('rejects malformed or negative maintenance counters', () => {
		expect(() =>
			storageMaintenanceReportFromUnknown({
				principal: 'owner', workspace: 'default', started_at_ms: 1, completed_at_ms: 2,
				duckdb: [{ database: 'x', bytes_before: 1, bytes_after: 1, bytes_reclaimed: -1, table_count: 0, row_count: 0 }],
				parquet: null, canonical_llm: null, retention: null
			})
		).toThrow('duckdb.bytes_reclaimed');
	});

	it('loads a scope-aware snapshot and sends server-owned confirmations', async () => {
		const fetchMock = vi.mocked(fetch);
		fetchMock
			.mockResolvedValueOnce(new Response(JSON.stringify(snapshot), { status: 200 }))
			.mockResolvedValueOnce(
				new Response(
					JSON.stringify({
						principal: 'owner',
						workspace: 'default',
						started_at_ms: 1,
						completed_at_ms: 2,
						duckdb: [],
						parquet: null,
						canonical_llm: null,
						retention: null
					}),
					{ status: 200 }
				)
			);

		await expect(fetchStorageSnapshot()).resolves.toMatchObject({ principal: 'owner' });
		await compactDatabases(['analytics']);
		const [, init] = fetchMock.mock.calls[1];
		expect(JSON.parse(String(init?.body))).toEqual({
			targets: ['analytics'],
			confirmation: 'COMPACT DATABASE'
		});
	});

	it('clears only metrics with the server-owned exact confirmation', async () => {
		const fetchMock = vi.mocked(fetch);
		fetchMock.mockResolvedValueOnce(
			new Response(JSON.stringify({ ...compactionMetrics, event_count: 0, areas: [], recent_events: [] }), {
				status: 200
			})
		);
		await expect(clearCompactionMetrics()).resolves.toMatchObject({ event_count: 0 });
		const [, init] = fetchMock.mock.calls[0];
		expect(JSON.parse(String(init?.body))).toEqual({
			confirmation: 'CLEAR COMPACTION METRICS'
		});
	});

	it('strictly parses attention reports and sends three distinct action contracts', async () => {
		const base = {
			principal: 'owner', workspace: 'default', started_at_ms: 1, completed_at_ms: 2,
			retention_days: null, optimize: null, retention: null, reclaim: null
		};
		expect(attentionLearningMaintenanceReportFromUnknown({
			...base,
			operation: 'optimize',
			optimize: {
				database: 'attention_learning.db', started_at: 1, completed_at: 2,
				analyzed_tables: ['attention_impressions'], bytes_before: 9, bytes_after: 9
			}
		})).toMatchObject({ operation: 'optimize', optimize: { analyzed_tables: ['attention_impressions'] } });
		expect(() => attentionLearningMaintenanceReportFromUnknown({
			...base, operation: 'erase_everything'
		})).toThrow('attention.operation');

		const fetchMock = vi.mocked(fetch);
		fetchMock.mockImplementation(async (_url, _init) => {
			const url = String(_url);
			const operation = url.includes('/optimize')
				? 'optimize'
				: url.includes('retention-preview')
					? 'retention_preview'
					: url.includes('retention-apply')
						? 'retention_apply'
						: 'reclaim';
			return new Response(JSON.stringify({
				...base,
				operation,
				retention_days: operation.startsWith('retention') ? 90 : null,
				retention: operation.startsWith('retention')
					? { principal: 'owner', workspace: 'default', apply: operation === 'retention_apply', cutoff_at: 1, affected_rows: {} }
					: null,
				optimize: operation === 'optimize'
					? { database: 'x', started_at: 1, completed_at: 2, analyzed_tables: [], bytes_before: 1, bytes_after: 1 }
					: null,
				reclaim: operation === 'reclaim'
					? { database: 'x', started_at: 1, completed_at: 2, bytes_before: 1, bytes_after: 1, bytes_reclaimed: 0, table_count: 1, row_count: 1, integrity_check: 'ok' }
					: null
			}), { status: 200 });
		});

		await optimizeAttentionLearning();
		await previewAttentionLearningRetention(90);
		await applyAttentionLearningRetention(90);
		await reclaimAttentionLearningSpace();
		const bodies = fetchMock.mock.calls.map(([, init]) => JSON.parse(String(init?.body)));
		expect(bodies).toEqual([
			{ confirmation: 'OPTIMIZE ATTENTION' },
			{ retention_days: 90 },
			{ retention_days: 90, confirmation: 'CLEAN ATTENTION HISTORY' },
			{ confirmation: 'RECLAIM ATTENTION DATABASE' }
		]);
	});
});


describe('automatic maintenance status', () => {
 it('accepts persisted results and rejects malformed states', () => {
  const row = { database: 'channel_assist', state: 'completed', message: 'Storage optimization completed.', bytes_reclaimed: 1024, last_success_at_ms: 1000 };
  expect(maintenanceStatusFromUnknown([row])).toEqual([row]);
  expect(() => maintenanceStatusFromUnknown([{ ...row, state: 'invented' }])).toThrow();
  expect(() => maintenanceStatusFromUnknown([{ ...row, bytes_reclaimed: Number.NaN }])).toThrow();
 });
});
