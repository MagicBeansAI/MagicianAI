import { render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/confirmationStore', () => ({
	requestConfirmation: vi.fn().mockResolvedValue(true)
}));
vi.mock('$lib/shared/stores/notifications', () => ({
	showError: vi.fn(),
	showSuccess: vi.fn()
}));

import { requestConfirmation } from '$lib/stores/confirmationStore';

import StorageGovernancePanel from './StorageGovernancePanel.svelte';
import { formatStorageBytes, type StorageSnapshot } from './storageGovernance';

const snapshot: StorageSnapshot = {
	principal: 'owner',
	workspace: 'default',
	generated_at_ms: 100,
	total_size_bytes: 1024 ** 3,
	total_allocated_bytes: 1024 ** 3,
	entries: [
		{
			id: 'channel_assist_duckdb',
			label: 'Comms intelligence',
			kind: 'duck_db',
			safety_class: 'lifecycle_managed',
			relative_path: 'mail_assist/mail_assist.duckdb',
			size_bytes: 900_000_000,
			allocated_bytes: 900_000_000,
			wal_bytes: 0,
			file_count: 1,
			inventory_complete: true,
			row_count: 60_000,
			oldest_partition: null,
			newest_partition: null,
			retention_days: null,
			policy: 'No age deletion.',
			actions: []
		},
		{
			id: 'memory_events',
			label: 'Memory diagnostics',
			kind: 'parquet',
			safety_class: 'observability',
			relative_path: 'analytics/memory_events',
			size_bytes: 100_000_000,
			allocated_bytes: 100_000_000,
			wal_bytes: 0,
			file_count: 12_000,
			inventory_complete: true,
			row_count: null,
			oldest_partition: '2026-05-01',
			newest_partition: '2026-07-24',
			retention_days: 90,
			policy: 'Recall diagnostics.',
			actions: []
		},
		{
			id: 'attention_learning_sqlite',
			label: 'Attention learning',
			kind: 'sqlite',
			safety_class: 'lifecycle_managed',
			relative_path: 'attention_learning.db',
			size_bytes: 19_000_000_000,
			allocated_bytes: 19_000_004_096,
			wal_bytes: 8_192,
			file_count: 2,
			inventory_complete: true,
			row_count: null,
			oldest_partition: null,
			newest_partition: null,
			retention_days: null,
			policy: 'Scope-owned evidence in one shared physical SQLite file.',
			actions: []
		},
		{
			id: 'app_packages',
			label: 'App packages',
			kind: 'directory',
			safety_class: 'lifecycle_managed',
			relative_path: 'apps/packages',
			size_bytes: 24_000,
			allocated_bytes: 28_672,
			wal_bytes: 0,
			file_count: 3,
			inventory_complete: false,
			row_count: null,
			oldest_partition: null,
			newest_partition: null,
			retention_days: null,
			policy: 'Future app registry owned; inspect only.',
			actions: []
		}
	],
	compaction_metrics: {
		schema_version: 1,
		healthy: true,
		warning: null,
		storage_bytes: 768,
		allocated_bytes: 4096,
		event_count: 2,
		retained_event_limit: 256,
		total_bytes_reclaimed: 4096,
		total_files_compacted: 12,
		total_query_files_avoided: 10,
		total_compacted_bytes_written: 1024,
		last_compaction_at_ms: 100,
		areas: [{
			kind: 'parquet', area: 'memory_events', runs: 2, partitions_compacted: 2,
			files_compacted: 12, query_files_avoided: 10, bytes_reclaimed: 4096,
			compacted_bytes_written: 1024, rows_compacted: 120, last_compacted_at_ms: 100
		}],
		recent_events: [{
			id: '01TEST', completed_at_ms: 100, trigger: 'scheduled_full', kind: 'parquet',
			area: 'memory_events', partitions_compacted: 1, files_before: 7, files_after: 1,
			files_compacted: 6, query_files_avoided: 5, bytes_before: 4096, bytes_after: 2048,
			bytes_reclaimed: 2048, compacted_bytes_written: 512, rows_compacted: 60, duration_ms: 12
		}]
	},
	safeguards: ['Verify before swap']
};

describe('StorageGovernancePanel', () => {
	it('exposes owner App maintenance and shows health only after a successful scoped check', async () => {
		const user = userEvent.setup();
		const appSnapshot: StorageSnapshot = {...snapshot, entries: [...snapshot.entries, {
			...snapshot.entries[0], id: 'app_store_sqlite', label: 'App store', kind: 'sqlite',
			relative_path: 'apps/app_store.sqlite3', policy: 'Encrypted app records and receipts.',
			shm_bytes: 32768,
			actions: [{id: 'app_store_verify', label: 'Check integrity', description: 'Verify encrypted pages.', confirmation: 'CHECK APP DATABASE', destructive: false}]
		}]};
		const maintainAppStoreAction = vi.fn().mockResolvedValue({
			principal: 'owner', workspace: 'default', relative_path: 'apps/app_store.sqlite3', operation: 'verify',
			encrypted: true, integrity_ok: true, completed_at: '2026-09-09T11:00:00Z', duration_ms: 20,
			database_bytes_before: 1024, database_bytes_after: 1024, wal_bytes_after: 0, shm_bytes_after: 32768,
			page_count: 2, free_pages: 0, checkpoint_busy: false
		});
		render(StorageGovernancePanel, {loadSnapshot: vi.fn().mockResolvedValue(appSnapshot), maintainAppStoreAction});
		await screen.findByRole('heading', {name: 'App database'});
		expect(screen.getByText('scopes/owner/default/apps/app_store.sqlite3')).toBeInTheDocument();
		expect(screen.getByText(/32.*shared memory/)).toBeInTheDocument();
		expect(screen.getByText('Integrity has not been checked in this session.')).toBeInTheDocument();
		await user.click(screen.getByRole('button', {name: 'Check integrity'}));
		await waitFor(() => expect(maintainAppStoreAction).toHaveBeenCalledWith('verify'));
		await waitFor(() => expect(screen.getByText(/Integrity verified at/)).toBeInTheDocument());
		expect(screen.getByText(`${formatStorageBytes(1024)} · SQLCipher encrypted`)).toBeInTheDocument();
		expect(within(screen.getByRole('region', {name: 'App database'})).queryByRole('button', {name: 'Reclaim disk space'})).not.toBeInTheDocument();
	});

	it('renders scoped sizes, lifecycle policy, partition range, and guarded actions', async () => {
		render(StorageGovernancePanel, {
			loadSnapshot: vi.fn().mockResolvedValue(snapshot)
		});
		await waitFor(() => expect(screen.getByText('Comms intelligence')).toBeInTheDocument());
		expect(screen.getByText('No age deletion.')).toBeInTheDocument();
		expect(screen.getByText('Memory diagnostics')).toBeInTheDocument();
		expect(screen.getByText('App packages')).toBeInTheDocument();
		expect(screen.getByText('Directory · Partial inventory')).toBeInTheDocument();
		expect(screen.getByText('Future app registry owned; inspect only.')).toBeInTheDocument();
		expect(screen.getByText('12,000')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Apply 90-day retention' })).toBeInTheDocument();
		expect(screen.getByRole('heading', { name: 'Compaction impact' })).toBeInTheDocument();
		expect(screen.getByText('Reads avoided · retained')).toBeInTheDocument();
		expect(screen.getByText('768 B')).toBeInTheDocument();
		expect(screen.getByText('owner')).toBeInTheDocument();
		expect(screen.getByText('default')).toBeInTheDocument();
		expect(screen.queryByText(/delete old mail/i)).not.toBeInTheDocument();
	});

	it('shows remote activation as blocked when backup or remote health is missing', async () => {
		render(StorageGovernancePanel, {
			loadSnapshot: vi.fn().mockResolvedValue(snapshot),
			loadActivation: vi.fn().mockResolvedValue({
				ok: true,
				local_canonical: true,
				backend_unavailable: true,
				blocking: ['backup_stale_or_missing', 'remote_health_unavailable'],
				gates: { gate1_closed: true, gate2_closed: true, gate3_closed: true },
				qualified: [{ operation: 'inventory', qualified: true, reason: 'catalog projection' }]
			})
		});
		// The 'Remote activation' heading renders outside {#if activation}, so it
		// appears on mount and proves nothing about the activation load. Wait on
		// activation-derived content instead.
		await waitFor(() => expect(screen.getByText(/backup_stale_or_missing/)).toBeInTheDocument());
		expect(screen.getByRole('button', { name: /Cut over to remote/ })).toBeDisabled();
	});

	it('uses a guarded injected action to clear only the bounded metrics ledger', async () => {
		const user = userEvent.setup();
		const clearMetricsAction = vi.fn().mockResolvedValue({
			...snapshot.compaction_metrics,
			event_count: 0,
			storage_bytes: 0,
			allocated_bytes: 0,
			areas: [],
			recent_events: []
		});
		render(StorageGovernancePanel, {
			loadSnapshot: vi.fn().mockResolvedValue(snapshot),
			clearMetricsAction
		});
		await waitFor(() => expect(screen.getByRole('button', { name: 'Clear metrics' })).toBeInTheDocument());
		await user.click(screen.getByRole('button', { name: 'Clear metrics' }));
		await waitFor(() => expect(clearMetricsAction).toHaveBeenCalledTimes(1));
	});

	it('keeps clear available when malformed history has bytes but no readable events', async () => {
		const user = userEvent.setup();
		const clearMetricsAction = vi.fn().mockResolvedValue({
			...snapshot.compaction_metrics,
			healthy: true,
			warning: null,
			event_count: 0,
			storage_bytes: 0,
			allocated_bytes: 0,
			areas: [],
			recent_events: []
		});
		render(StorageGovernancePanel, {
			loadSnapshot: vi.fn().mockResolvedValue({
				...snapshot,
				compaction_metrics: {
					...snapshot.compaction_metrics,
					healthy: false,
					warning: 'Metrics history is malformed; clear it to recover.',
					event_count: 0,
					storage_bytes: 32,
					areas: [],
					recent_events: []
				}
			}),
			clearMetricsAction
		});
		const clear = await screen.findByRole('button', { name: 'Clear metrics' });
		expect(clear).toBeEnabled();
		await user.click(clear);
		await waitFor(() => expect(clearMetricsAction).toHaveBeenCalledTimes(1));
	});

	it('routes owner-specific compaction and governed retention through injected actions', async () => {
		const user = userEvent.setup();
		const compactDatabaseAction = vi.fn().mockResolvedValue({
			principal: 'owner', workspace: 'default', started_at_ms: 1, completed_at_ms: 2,
			duckdb: [{ database: 'mail.duckdb', bytes_before: 10, bytes_after: 5, bytes_reclaimed: 5, table_count: 2, row_count: 3 }],
			parquet: null, canonical_llm: null, retention: null
		});
		const retentionAction = vi.fn().mockResolvedValue({
			principal: 'owner', workspace: 'default', started_at_ms: 1, completed_at_ms: 2,
			duckdb: [], parquet: null, canonical_llm: null,
			retention: { partitions_scanned: 4, partitions_removed: 1, bytes_removed: 512 }
		});
		render(StorageGovernancePanel, {
			loadSnapshot: vi.fn().mockResolvedValue(snapshot),
			compactDatabaseAction,
			retentionAction
		});
		await waitFor(() => expect(screen.getByText('Comms intelligence')).toBeInTheDocument());
		await user.click(screen.getByRole('button', { name: 'Compact' }));
		await waitFor(() => expect(compactDatabaseAction).toHaveBeenCalledWith(['channel_assist']));
		await user.click(screen.getByRole('button', { name: 'Apply 90-day retention' }));
		await waitFor(() => expect(retentionAction).toHaveBeenCalledWith(90));
	});

	it('keeps attention optimization, previewed cleanup, and physical reclaim distinct', async () => {
		const user = userEvent.setup();
		const optimizeAttentionAction = vi.fn().mockResolvedValue({
			principal: 'owner', workspace: 'default', operation: 'optimize',
			started_at_ms: 1, completed_at_ms: 2, retention_days: null,
			optimize: {
				database: 'attention_learning.db', started_at: 1, completed_at: 2,
				analyzed_tables: ['attention_impressions'], bytes_before: 100, bytes_after: 100
			},
			retention: null, reclaim: null
		});
		const previewAttentionRetentionAction = vi.fn().mockResolvedValue({
			principal: 'owner', workspace: 'default', operation: 'retention_preview',
			started_at_ms: 1, completed_at_ms: 2, retention_days: 90, optimize: null,
			retention: {
				principal: 'owner', workspace: 'default', apply: false, cutoff_at: 10,
				affected_rows: { impressions: 12, decisions: 3 }
			},
			reclaim: null
		});
		const applyAttentionRetentionAction = vi.fn().mockResolvedValue({
			principal: 'owner', workspace: 'default', operation: 'retention_apply',
			started_at_ms: 1, completed_at_ms: 2, retention_days: 90, optimize: null,
			retention: {
				principal: 'owner', workspace: 'default', apply: true, cutoff_at: 10,
				affected_rows: { impressions: 12, decisions: 3 }
			},
			reclaim: null
		});
		const reclaimAttentionAction = vi.fn().mockResolvedValue({
			principal: 'owner', workspace: 'default', operation: 'reclaim',
			started_at_ms: 1, completed_at_ms: 2, retention_days: null,
			optimize: null, retention: null,
			reclaim: {
				database: 'attention_learning.db', started_at: 1, completed_at: 2,
				bytes_before: 100, bytes_after: 40, bytes_reclaimed: 60,
				table_count: 2, row_count: 15, integrity_check: 'ok'
			}
		});
		render(StorageGovernancePanel, {
			loadSnapshot: vi.fn().mockResolvedValue(snapshot),
			optimizeAttentionAction,
			previewAttentionRetentionAction,
			applyAttentionRetentionAction,
			reclaimAttentionAction
		});

		const apply = await screen.findByRole('button', { name: 'Apply cleanup' });
		expect(apply).toBeDisabled();
		vi.mocked(requestConfirmation).mockClear();
		await user.click(screen.getByRole('button', { name: 'Optimize now' }));
		await waitFor(() => expect(optimizeAttentionAction).toHaveBeenCalledTimes(1));
		expect(requestConfirmation).not.toHaveBeenCalled();
		await user.click(screen.getByRole('button', { name: 'Preview cleanup' }));
		await waitFor(() => expect(screen.getByText('15 eligible rows')).toBeInTheDocument());
		expect(apply).toBeEnabled();
		await user.click(apply);
		await waitFor(() => expect(applyAttentionRetentionAction).toHaveBeenCalledWith(90));
		await user.click(screen.getByRole('button', { name: 'Reclaim disk space' }));
		await waitFor(() => expect(reclaimAttentionAction).toHaveBeenCalledTimes(1));
	});
});


it('shows automatic maintenance without requiring an inventory scan to complete', async () => {
 render(StorageGovernancePanel, {
  loadSnapshot: () => new Promise(() => {}),
  loadMaintenance: async () => [{ database: 'channel_assist', state: 'running', message: 'Optimizing storage. Related requests may briefly wait.', last_success_at_ms: null, bytes_reclaimed: 0 }]
 });
 expect(await screen.findByText('Comms intelligence · running')).toBeInTheDocument();
 expect(screen.getByText('Optimizing storage. Related requests may briefly wait.')).toBeInTheDocument();
});
