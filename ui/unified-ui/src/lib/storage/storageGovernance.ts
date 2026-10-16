import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { LONG_FETCH_TIMEOUT_MS, timedFetch } from '$lib/shared/fetch';

export type StorageKind = 'duck_db' | 'sqlite' | 'parquet' | 'journal' | 'directory';
export type StorageSafetyClass =
	| 'authoritative'
	| 'lifecycle_managed'
	| 'regenerable'
	| 'observability'
	| 'restricted';
export type DuckDbTarget = 'analytics' | 'channel_assist' | 'ui_threads' | 'social';
export type AppStoreMaintenanceOperation = 'verify' | 'optimize' | 'reclaim';
export interface AppStoreMaintenanceReport {
	principal: string;
	workspace: string;
	relative_path: string;
	operation: AppStoreMaintenanceOperation;
	encrypted: boolean;
	integrity_ok: boolean;
	completed_at: string;
	duration_ms: number;
	database_bytes_before: number;
	database_bytes_after: number;
	wal_bytes_after: number;
	shm_bytes_after: number;
	page_count: number;
	free_pages: number;
	checkpoint_busy: boolean;
}
export type CompactionMetricTrigger = 'manual' | 'startup' | 'scheduled_full' | 'scheduled_hot';
export type CompactionMetricKind = 'duck_db' | 'parquet' | 'canonical_llm';

export interface StorageActionDescriptor {
	id: string;
	label: string;
	description: string;
	confirmation: string | null;
	destructive: boolean;
}

export interface StorageEntry {
	id: string;
	label: string;
	kind: StorageKind;
	safety_class: StorageSafetyClass;
	relative_path: string;
	size_bytes: number;
	allocated_bytes: number;
	wal_bytes: number;
	shm_bytes?: number;
	file_count: number;
	inventory_complete: boolean;
	row_count: number | null;
	oldest_partition: string | null;
	newest_partition: string | null;
	retention_days: number | null;
	policy: string;
	actions: StorageActionDescriptor[];
}

export interface StorageSnapshot {
	principal: string;
	workspace: string;
	generated_at_ms: number;
	total_size_bytes: number;
	total_allocated_bytes: number;
	entries: StorageEntry[];
	compaction_metrics: CompactionMetricsSnapshot;
	safeguards: string[];
}

export interface CompactionMetricEvent {
	id: string;
	completed_at_ms: number;
	trigger: CompactionMetricTrigger;
	kind: CompactionMetricKind;
	area: string;
	partitions_compacted: number;
	files_before: number;
	files_after: number;
	files_compacted: number;
	query_files_avoided: number;
	bytes_before: number;
	bytes_after: number;
	bytes_reclaimed: number;
	compacted_bytes_written: number;
	rows_compacted: number;
	duration_ms: number;
}

export interface CompactionAreaMetrics {
	kind: CompactionMetricKind;
	area: string;
	runs: number;
	partitions_compacted: number;
	files_compacted: number;
	query_files_avoided: number;
	bytes_reclaimed: number;
	compacted_bytes_written: number;
	rows_compacted: number;
	last_compacted_at_ms: number;
}

export interface CompactionMetricsSnapshot {
	schema_version: number;
	healthy: boolean;
	warning: string | null;
	storage_bytes: number;
	allocated_bytes: number;
	event_count: number;
	retained_event_limit: number;
	total_bytes_reclaimed: number;
	total_files_compacted: number;
	total_query_files_avoided: number;
	total_compacted_bytes_written: number;
	last_compaction_at_ms: number | null;
	areas: CompactionAreaMetrics[];
	recent_events: CompactionMetricEvent[];
}

export interface DuckDbCompactionReport {
	database: string;
	bytes_before: number;
	bytes_after: number;
	bytes_reclaimed: number;
	table_count: number;
	row_count: number;
}

export interface StorageMaintenanceReport {
	principal: string;
	workspace: string;
	started_at_ms: number;
	completed_at_ms: number;
	duckdb: DuckDbCompactionReport[];
	parquet: CompactionRunStats | null;
	canonical_llm: CompactionRunStats | null;
	retention: Record<string, number> | null;
}

export type AttentionLearningMaintenanceOperation =
	| 'optimize'
	| 'retention_preview'
	| 'retention_apply'
	| 'reclaim';

export interface AttentionRetentionReport {
	principal: string;
	workspace: string;
	apply: boolean;
	cutoff_at: number | null;
	affected_rows: Record<string, number>;
}

export interface AttentionOptimizeReport {
	database: string;
	started_at: number;
	completed_at: number;
	analyzed_tables: string[];
	bytes_before: number;
	bytes_after: number;
}

export interface AttentionReclaimReport {
	database: string;
	started_at: number;
	completed_at: number;
	bytes_before: number;
	bytes_after: number;
	bytes_reclaimed: number;
	table_count: number;
	row_count: number;
	integrity_check: string;
}

export interface AttentionLearningMaintenanceReport {
	principal: string;
	workspace: string;
	operation: AttentionLearningMaintenanceOperation;
	started_at_ms: number;
	completed_at_ms: number;
	retention_days: number | null;
	optimize: AttentionOptimizeReport | null;
	retention: AttentionRetentionReport | null;
	reclaim: AttentionReclaimReport | null;
}

export interface CompactionRunStats {
	partitions_compacted: number;
	raw_files_compacted: number;
	raw_files_pruned: number;
	bytes_reclaimed: number;
}

function nullableRecord(value: unknown, field: string): Record<string, number> | null {
	if (value == null) return null;
	const raw = record(value);
	const normalized: Record<string, number> = {};
	for (const [key, item] of Object.entries(raw)) {
		normalized[key] = finiteNumber(item, `${field}.${key}`);
	}
	return normalized;
}

function nullableCompactionRun(value: unknown, field: string): CompactionRunStats | null {
	if (value == null) return null;
	const raw = record(value);
	return {
		partitions_compacted: finiteNumber(raw.partitions_compacted, `${field}.partitions_compacted`),
		raw_files_compacted: finiteNumber(raw.raw_files_compacted, `${field}.raw_files_compacted`),
		raw_files_pruned:
			raw.raw_files_pruned == null ? 0 : finiteNumber(raw.raw_files_pruned, `${field}.raw_files_pruned`),
		bytes_reclaimed:
			raw.bytes_reclaimed == null ? 0 : finiteNumber(raw.bytes_reclaimed, `${field}.bytes_reclaimed`)
	};
}

export function storageMaintenanceReportFromUnknown(value: unknown): StorageMaintenanceReport {
	const raw = record(value);
	if (!Array.isArray(raw.duckdb)) {
		throw new Error('Malformed storage maintenance response: duckdb');
	}
	return {
		principal: text(raw.principal, 'principal'),
		workspace: text(raw.workspace, 'workspace'),
		started_at_ms: finiteNumber(raw.started_at_ms, 'started_at_ms'),
		completed_at_ms: finiteNumber(raw.completed_at_ms, 'completed_at_ms'),
		duckdb: raw.duckdb.map((value) => {
			const report = record(value);
			return {
				database: text(report.database, 'duckdb.database'),
				bytes_before: finiteNumber(report.bytes_before, 'duckdb.bytes_before'),
				bytes_after: finiteNumber(report.bytes_after, 'duckdb.bytes_after'),
				bytes_reclaimed: finiteNumber(report.bytes_reclaimed, 'duckdb.bytes_reclaimed'),
				table_count: finiteNumber(report.table_count, 'duckdb.table_count'),
				row_count: finiteNumber(report.row_count, 'duckdb.row_count')
			};
		}),
		parquet: nullableCompactionRun(raw.parquet, 'parquet'),
		canonical_llm: nullableCompactionRun(raw.canonical_llm, 'canonical_llm'),
		retention: nullableRecord(raw.retention, 'retention')
	};
}

const attentionOperations = new Set<AttentionLearningMaintenanceOperation>([
	'optimize',
	'retention_preview',
	'retention_apply',
	'reclaim'
]);

export function attentionLearningMaintenanceReportFromUnknown(
	value: unknown
): AttentionLearningMaintenanceReport {
	const raw = record(value);
	const operation = text(raw.operation, 'attention.operation') as AttentionLearningMaintenanceOperation;
	if (!attentionOperations.has(operation)) {
		throw new Error('Malformed storage response: attention.operation');
	}
	const optimizeRaw = raw.optimize == null ? null : record(raw.optimize);
	const retentionRaw = raw.retention == null ? null : record(raw.retention);
	const reclaimRaw = raw.reclaim == null ? null : record(raw.reclaim);
	const analyzedTables = optimizeRaw?.analyzed_tables;
	if (optimizeRaw && !Array.isArray(analyzedTables)) {
		throw new Error('Malformed storage response: attention.optimize.analyzed_tables');
	}
	return {
		principal: text(raw.principal, 'attention.principal'),
		workspace: text(raw.workspace, 'attention.workspace'),
		operation,
		started_at_ms: finiteNumber(raw.started_at_ms, 'attention.started_at_ms'),
		completed_at_ms: finiteNumber(raw.completed_at_ms, 'attention.completed_at_ms'),
		retention_days: nullableNumber(raw.retention_days, 'attention.retention_days'),
		optimize: optimizeRaw
			? {
					database: text(optimizeRaw.database, 'attention.optimize.database'),
					started_at: finiteNumber(optimizeRaw.started_at, 'attention.optimize.started_at'),
					completed_at: finiteNumber(optimizeRaw.completed_at, 'attention.optimize.completed_at'),
					analyzed_tables: (analyzedTables as unknown[]).map((item, index) =>
						text(item, `attention.optimize.analyzed_tables.${index}`)
					),
					bytes_before: finiteNumber(optimizeRaw.bytes_before, 'attention.optimize.bytes_before'),
					bytes_after: finiteNumber(optimizeRaw.bytes_after, 'attention.optimize.bytes_after')
			  }
			: null,
		retention: retentionRaw
			? {
					principal: text(retentionRaw.principal, 'attention.retention.principal'),
					workspace: text(retentionRaw.workspace, 'attention.retention.workspace'),
					apply: boolean(retentionRaw.apply, 'attention.retention.apply'),
					cutoff_at: nullableNumber(retentionRaw.cutoff_at, 'attention.retention.cutoff_at'),
					affected_rows: nullableRecord(
						retentionRaw.affected_rows,
						'attention.retention.affected_rows'
					) ?? {}
			  }
			: null,
		reclaim: reclaimRaw
			? {
					database: text(reclaimRaw.database, 'attention.reclaim.database'),
					started_at: finiteNumber(reclaimRaw.started_at, 'attention.reclaim.started_at'),
					completed_at: finiteNumber(reclaimRaw.completed_at, 'attention.reclaim.completed_at'),
					bytes_before: finiteNumber(reclaimRaw.bytes_before, 'attention.reclaim.bytes_before'),
					bytes_after: finiteNumber(reclaimRaw.bytes_after, 'attention.reclaim.bytes_after'),
					bytes_reclaimed: finiteNumber(reclaimRaw.bytes_reclaimed, 'attention.reclaim.bytes_reclaimed'),
					table_count: finiteNumber(reclaimRaw.table_count, 'attention.reclaim.table_count'),
					row_count: finiteNumber(reclaimRaw.row_count, 'attention.reclaim.row_count'),
					integrity_check: text(reclaimRaw.integrity_check, 'attention.reclaim.integrity_check')
			  }
			: null
	};
}

const storageKinds = new Set<StorageKind>([
	'duck_db',
	'sqlite',
	'parquet',
	'journal',
	'directory'
]);
const safetyClasses = new Set<StorageSafetyClass>([
	'authoritative',
	'lifecycle_managed',
	'regenerable',
	'observability',
	'restricted'
]);

function record(value: unknown): Record<string, unknown> {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error('Malformed storage response');
	}
	return value as Record<string, unknown>;
}

function text(value: unknown, field: string): string {
	if (typeof value !== 'string') throw new Error(`Malformed storage response: ${field}`);
	return value;
}

function finiteNumber(value: unknown, field: string): number {
	if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) {
		throw new Error(`Malformed storage response: ${field}`);
	}
	return value;
}

function boolean(value: unknown, field: string): boolean {
	if (typeof value !== 'boolean') throw new Error(`Malformed storage response: ${field}`);
	return value;
}

function nullableText(value: unknown, field: string): string | null {
	return value == null ? null : text(value, field);
}

function nullableNumber(value: unknown, field: string): number | null {
	return value == null ? null : finiteNumber(value, field);
}

const metricKinds = new Set<CompactionMetricKind>(['duck_db', 'parquet', 'canonical_llm']);
const metricTriggers = new Set<CompactionMetricTrigger>([
	'manual',
	'startup',
	'scheduled_full',
	'scheduled_hot'
]);

function metricKind(value: unknown, field: string): CompactionMetricKind {
	const kind = text(value, field) as CompactionMetricKind;
	if (!metricKinds.has(kind)) throw new Error(`Malformed storage response: ${field}`);
	return kind;
}

function metricEventFromUnknown(value: unknown): CompactionMetricEvent {
	const raw = record(value);
	const trigger = text(raw.trigger, 'compaction_metrics.event.trigger') as CompactionMetricTrigger;
	if (!metricTriggers.has(trigger)) {
		throw new Error('Malformed storage response: compaction_metrics.event.trigger');
	}
	return {
		id: text(raw.id, 'compaction_metrics.event.id'),
		completed_at_ms: finiteNumber(raw.completed_at_ms, 'compaction_metrics.event.completed_at_ms'),
		trigger,
		kind: metricKind(raw.kind, 'compaction_metrics.event.kind'),
		area: text(raw.area, 'compaction_metrics.event.area'),
		partitions_compacted: finiteNumber(raw.partitions_compacted, 'compaction_metrics.event.partitions_compacted'),
		files_before: finiteNumber(raw.files_before, 'compaction_metrics.event.files_before'),
		files_after: finiteNumber(raw.files_after, 'compaction_metrics.event.files_after'),
		files_compacted: finiteNumber(raw.files_compacted, 'compaction_metrics.event.files_compacted'),
		query_files_avoided: finiteNumber(raw.query_files_avoided, 'compaction_metrics.event.query_files_avoided'),
		bytes_before: finiteNumber(raw.bytes_before, 'compaction_metrics.event.bytes_before'),
		bytes_after: finiteNumber(raw.bytes_after, 'compaction_metrics.event.bytes_after'),
		bytes_reclaimed: finiteNumber(raw.bytes_reclaimed, 'compaction_metrics.event.bytes_reclaimed'),
		compacted_bytes_written: finiteNumber(raw.compacted_bytes_written, 'compaction_metrics.event.compacted_bytes_written'),
		rows_compacted: finiteNumber(raw.rows_compacted, 'compaction_metrics.event.rows_compacted'),
		duration_ms: finiteNumber(raw.duration_ms, 'compaction_metrics.event.duration_ms')
	};
}

function areaMetricsFromUnknown(value: unknown): CompactionAreaMetrics {
	const raw = record(value);
	return {
		kind: metricKind(raw.kind, 'compaction_metrics.area.kind'),
		area: text(raw.area, 'compaction_metrics.area.area'),
		runs: finiteNumber(raw.runs, 'compaction_metrics.area.runs'),
		partitions_compacted: finiteNumber(raw.partitions_compacted, 'compaction_metrics.area.partitions_compacted'),
		files_compacted: finiteNumber(raw.files_compacted, 'compaction_metrics.area.files_compacted'),
		query_files_avoided: finiteNumber(raw.query_files_avoided, 'compaction_metrics.area.query_files_avoided'),
		bytes_reclaimed: finiteNumber(raw.bytes_reclaimed, 'compaction_metrics.area.bytes_reclaimed'),
		compacted_bytes_written: finiteNumber(raw.compacted_bytes_written, 'compaction_metrics.area.compacted_bytes_written'),
		rows_compacted: finiteNumber(raw.rows_compacted, 'compaction_metrics.area.rows_compacted'),
		last_compacted_at_ms: finiteNumber(raw.last_compacted_at_ms, 'compaction_metrics.area.last_compacted_at_ms')
	};
}

export function compactionMetricsFromUnknown(value: unknown): CompactionMetricsSnapshot {
	const raw = record(value);
	if (!Array.isArray(raw.areas) || !Array.isArray(raw.recent_events)) {
		throw new Error('Malformed storage response: compaction_metrics');
	}
	return {
		schema_version: finiteNumber(raw.schema_version, 'compaction_metrics.schema_version'),
		healthy: boolean(raw.healthy, 'compaction_metrics.healthy'),
		warning: nullableText(raw.warning, 'compaction_metrics.warning'),
		storage_bytes: finiteNumber(raw.storage_bytes, 'compaction_metrics.storage_bytes'),
		allocated_bytes: finiteNumber(raw.allocated_bytes, 'compaction_metrics.allocated_bytes'),
		event_count: finiteNumber(raw.event_count, 'compaction_metrics.event_count'),
		retained_event_limit: finiteNumber(raw.retained_event_limit, 'compaction_metrics.retained_event_limit'),
		total_bytes_reclaimed: finiteNumber(raw.total_bytes_reclaimed, 'compaction_metrics.total_bytes_reclaimed'),
		total_files_compacted: finiteNumber(raw.total_files_compacted, 'compaction_metrics.total_files_compacted'),
		total_query_files_avoided: finiteNumber(raw.total_query_files_avoided, 'compaction_metrics.total_query_files_avoided'),
		total_compacted_bytes_written: finiteNumber(raw.total_compacted_bytes_written, 'compaction_metrics.total_compacted_bytes_written'),
		last_compaction_at_ms: nullableNumber(raw.last_compaction_at_ms, 'compaction_metrics.last_compaction_at_ms'),
		areas: raw.areas.map(areaMetricsFromUnknown),
		recent_events: raw.recent_events.map(metricEventFromUnknown)
	};
}

function actionFromUnknown(value: unknown): StorageActionDescriptor {
	const raw = record(value);
	return {
		id: text(raw.id, 'action.id'),
		label: text(raw.label, 'action.label'),
		description: text(raw.description, 'action.description'),
		confirmation: nullableText(raw.confirmation, 'action.confirmation'),
		destructive: raw.destructive === true
	};
}

function entryFromUnknown(value: unknown): StorageEntry {
	const raw = record(value);
	const kind = text(raw.kind, 'entry.kind') as StorageKind;
	const safetyClass = text(raw.safety_class, 'entry.safety_class') as StorageSafetyClass;
	if (!storageKinds.has(kind) || !safetyClasses.has(safetyClass)) {
		throw new Error('Malformed storage response: unknown entry classification');
	}
	if (!Array.isArray(raw.actions)) throw new Error('Malformed storage response: entry.actions');
	return {
		id: text(raw.id, 'entry.id'),
		label: text(raw.label, 'entry.label'),
		kind,
		safety_class: safetyClass,
		relative_path: text(raw.relative_path, 'entry.relative_path'),
		size_bytes: finiteNumber(raw.size_bytes, 'entry.size_bytes'),
		allocated_bytes: finiteNumber(raw.allocated_bytes, 'entry.allocated_bytes'),
		wal_bytes: finiteNumber(raw.wal_bytes, 'entry.wal_bytes'),
		shm_bytes: raw.shm_bytes === undefined ? undefined : finiteNumber(raw.shm_bytes, 'entry.shm_bytes'),
		file_count: finiteNumber(raw.file_count, 'entry.file_count'),
		inventory_complete: boolean(raw.inventory_complete, 'entry.inventory_complete'),
		row_count: nullableNumber(raw.row_count, 'entry.row_count'),
		oldest_partition: nullableText(raw.oldest_partition, 'entry.oldest_partition'),
		newest_partition: nullableText(raw.newest_partition, 'entry.newest_partition'),
		retention_days: nullableNumber(raw.retention_days, 'entry.retention_days'),
		policy: text(raw.policy, 'entry.policy'),
		actions: raw.actions.map(actionFromUnknown)
	};
}

export function storageSnapshotFromUnknown(value: unknown): StorageSnapshot {
	const raw = record(value);
	if (!Array.isArray(raw.entries) || !Array.isArray(raw.safeguards)) {
		throw new Error('Malformed storage response: missing entries or safeguards');
	}
	return {
		principal: text(raw.principal, 'principal'),
		workspace: text(raw.workspace, 'workspace'),
		generated_at_ms: finiteNumber(raw.generated_at_ms, 'generated_at_ms'),
		total_size_bytes: finiteNumber(raw.total_size_bytes, 'total_size_bytes'),
		total_allocated_bytes: finiteNumber(raw.total_allocated_bytes, 'total_allocated_bytes'),
		entries: raw.entries.map(entryFromUnknown),
		compaction_metrics: compactionMetricsFromUnknown(raw.compaction_metrics),
		safeguards: raw.safeguards.map((item, index) => text(item, `safeguards.${index}`))
	};
}

async function responseJson(response: Response): Promise<unknown> {
	const body = await response.json().catch(() => null);
	if (!response.ok) {
		const raw = body && typeof body === 'object' ? (body as Record<string, unknown>) : null;
		throw new Error(
			(typeof raw?.message === 'string' && raw.message) ||
				(typeof raw?.error === 'string' && raw.error) ||
				`Storage request failed (${response.status})`
		);
	}
	return body;
}

export async function fetchStorageSnapshot(): Promise<StorageSnapshot> {
	return storageSnapshotFromUnknown(
		await responseJson(
			await timedFetch('/api/magician/v2/storage', { headers: scopedRequestHeaders() })
		)
	);
}

export interface StorageActivationGates {
	gate1_closed: boolean;
	gate2_closed: boolean;
	gate3_closed: boolean;
}

export interface StorageQualifiedOperation {
	operation: string;
	qualified: boolean;
	reason: string;
}

export interface StorageActivationReport {
	ok: boolean;
	local_canonical: boolean;
	backend_unavailable: boolean;
	blocking: string[];
	gates: StorageActivationGates;
	qualified: StorageQualifiedOperation[];
}

export async function fetchStorageActivation(): Promise<StorageActivationReport | null> {
	try {
		const body = await responseJson(
			await timedFetch('/api/magician/v2/storage/activation', {
				headers: scopedRequestHeaders()
			})
		);
		const raw = record(body);
		const gates = record(raw.gates);
		return {
			ok: boolean(raw.ok, 'ok'),
			local_canonical: boolean(raw.local_canonical, 'local_canonical'),
			backend_unavailable: boolean(raw.backend_unavailable, 'backend_unavailable'),
			blocking: Array.isArray(raw.blocking)
				? raw.blocking.map((item, index) => text(item, `blocking.${index}`))
				: [],
			gates: {
				gate1_closed: boolean(gates.gate1_closed, 'gates.gate1_closed'),
				gate2_closed: boolean(gates.gate2_closed, 'gates.gate2_closed'),
				gate3_closed: boolean(gates.gate3_closed, 'gates.gate3_closed')
			},
			qualified: Array.isArray(raw.qualified)
				? raw.qualified.map((item, index) => {
						const row = record(item);
						return {
							operation: text(row.operation, `qualified.${index}.operation`),
							qualified: boolean(row.qualified, `qualified.${index}.qualified`),
							reason: text(row.reason, `qualified.${index}.reason`)
						};
					})
				: []
		};
	} catch {
		return null;
	}
}

export function appStoreMaintenanceReportFromUnknown(value: unknown): AppStoreMaintenanceReport {
	const raw = record(value);
	const operation = text(raw.operation, 'operation') as AppStoreMaintenanceOperation;
	if (!['verify', 'optimize', 'reclaim'].includes(operation)) throw new Error('Invalid App maintenance operation');
	return {
		principal: text(raw.principal, 'principal'), workspace: text(raw.workspace, 'workspace'),
		relative_path: text(raw.relative_path, 'relative_path'), operation,
		encrypted: boolean(raw.encrypted, 'encrypted'), integrity_ok: boolean(raw.integrity_ok, 'integrity_ok'),
		completed_at: text(raw.completed_at, 'completed_at'), duration_ms: finiteNumber(raw.duration_ms, 'duration_ms'),
		database_bytes_before: finiteNumber(raw.database_bytes_before, 'database_bytes_before'),
		database_bytes_after: finiteNumber(raw.database_bytes_after, 'database_bytes_after'),
		wal_bytes_after: finiteNumber(raw.wal_bytes_after, 'wal_bytes_after'),
		shm_bytes_after: finiteNumber(raw.shm_bytes_after, 'shm_bytes_after'),
		page_count: finiteNumber(raw.page_count, 'page_count'), free_pages: finiteNumber(raw.free_pages, 'free_pages'),
		checkpoint_busy: boolean(raw.checkpoint_busy, 'checkpoint_busy')
	};
}

export async function maintainAppStore(operation: AppStoreMaintenanceOperation): Promise<AppStoreMaintenanceReport> {
	const confirmation = {verify: 'CHECK APP DATABASE', optimize: 'OPTIMIZE APP DATABASE', reclaim: 'RECLAIM APP DATABASE'}[operation];
	return appStoreMaintenanceReportFromUnknown(await responseJson(await timedFetch('/api/magician/v2/storage/actions/app-store', {
		method: 'POST', headers: scopedRequestHeaders({'content-type': 'application/json'}),
		body: JSON.stringify({operation, confirmation}), timeoutMs: LONG_FETCH_TIMEOUT_MS
	})));
}

async function postMaintenance(
	path: string,
	body: Record<string, unknown>
): Promise<StorageMaintenanceReport> {
	return storageMaintenanceReportFromUnknown(await responseJson(
		await timedFetch(path, {
			method: 'POST',
			headers: scopedRequestHeaders({ 'content-type': 'application/json' }),
			body: JSON.stringify(body),
			timeoutMs: LONG_FETCH_TIMEOUT_MS
		})
	));
}

export function compactDatabases(targets: DuckDbTarget[]): Promise<StorageMaintenanceReport> {
	return postMaintenance('/api/magician/v2/storage/actions/compact-databases', {
		targets,
		confirmation: 'COMPACT DATABASE'
	});
}

export function compactParquet(): Promise<StorageMaintenanceReport> {
	return postMaintenance('/api/magician/v2/storage/actions/compact-parquet', {
		confirmation: 'COMPACT PARQUET'
	});
}

export function applyStorageRetention(retentionDays = 90): Promise<StorageMaintenanceReport> {
	return postMaintenance('/api/magician/v2/storage/actions/apply-retention', {
		retention_days: retentionDays,
		confirmation: 'APPLY RETENTION'
	});
}

async function postAttentionMaintenance(
	path: string,
	body: Record<string, unknown>,
	timeoutMs = LONG_FETCH_TIMEOUT_MS
): Promise<AttentionLearningMaintenanceReport> {
	return attentionLearningMaintenanceReportFromUnknown(
		await responseJson(
			await timedFetch(path, {
				method: 'POST',
				headers: scopedRequestHeaders({ 'content-type': 'application/json' }),
				body: JSON.stringify(body),
				timeoutMs
			})
		)
	);
}

export function optimizeAttentionLearning(): Promise<AttentionLearningMaintenanceReport> {
	return postAttentionMaintenance('/api/magician/v2/storage/actions/attention-learning/optimize', {
		confirmation: 'OPTIMIZE ATTENTION'
	});
}

export function previewAttentionLearningRetention(
	retentionDays: number
): Promise<AttentionLearningMaintenanceReport> {
	return postAttentionMaintenance(
		'/api/magician/v2/storage/actions/attention-learning/retention-preview',
		{ retention_days: retentionDays }
	);
}

export function applyAttentionLearningRetention(
	retentionDays: number
): Promise<AttentionLearningMaintenanceReport> {
	return postAttentionMaintenance(
		'/api/magician/v2/storage/actions/attention-learning/retention-apply',
		{
			retention_days: retentionDays,
			confirmation: 'CLEAN ATTENTION HISTORY'
		}
	);
}

export function reclaimAttentionLearningSpace(): Promise<AttentionLearningMaintenanceReport> {
	return postAttentionMaintenance(
		'/api/magician/v2/storage/actions/attention-learning/reclaim',
		{ confirmation: 'RECLAIM ATTENTION DATABASE' },
		// A multi-gigabyte verified rebuild can legitimately exceed the ordinary
		// ten-minute maintenance timeout. The backend remains the operation owner.
		2 * 60 * 60 * 1000
	);
}

export async function clearCompactionMetrics(): Promise<CompactionMetricsSnapshot> {
	return compactionMetricsFromUnknown(
		await responseJson(
			await timedFetch('/api/magician/v2/storage/actions/clear-compaction-metrics', {
				method: 'POST',
				headers: scopedRequestHeaders({ 'content-type': 'application/json' }),
				body: JSON.stringify({ confirmation: 'CLEAR COMPACTION METRICS' }),
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			})
		)
	);
}

export async function purgeFeedOrphans(): Promise<number> {
	const payload = (await responseJson(
		await timedFetch('/api/magician/v2/feed/purge-orphans', {
			method: 'POST',
			headers: scopedRequestHeaders()
		})
	)) as { removed_count?: number };
	return typeof payload.removed_count === 'number' ? payload.removed_count : 0;
}

export function formatStorageBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	const units = ['KB', 'MB', 'GB', 'TB'];
	let value = bytes / 1024;
	let unit = units[0];
	for (let index = 1; index < units.length && value >= 1024; index += 1) {
		value /= 1024;
		unit = units[index];
	}
	const digits = value >= 100 ? 0 : value >= 10 ? 1 : 2;
	return `${value.toFixed(digits)} ${unit}`;
}


export interface DatabaseMaintenanceStatus {
 database: 'channel_assist' | 'feed';
 state: 'idle' | 'running' | 'completed' | 'deferred' | 'failed' | 'disabled';
 message: string;
 last_success_at_ms: number | null;
 bytes_reclaimed: number;
}

export function maintenanceStatusFromUnknown(value: unknown): DatabaseMaintenanceStatus[] {
 if (!Array.isArray(value) || value.length > 2) throw new Error('Malformed maintenance status');
 return value.map((item) => {
  const raw = record(item);
  if (!['channel_assist', 'feed'].includes(raw.database as string)
   || !['idle', 'running', 'completed', 'deferred', 'failed', 'disabled'].includes(raw.state as string)) {
   throw new Error('Malformed maintenance status');
  }
  return {
   database: raw.database as DatabaseMaintenanceStatus['database'],
   state: raw.state as DatabaseMaintenanceStatus['state'],
   message: text(raw.message, 'message'),
   last_success_at_ms: raw.last_success_at_ms == null ? null : finiteNumber(raw.last_success_at_ms, 'last_success_at_ms'),
   bytes_reclaimed: finiteNumber(raw.bytes_reclaimed, 'bytes_reclaimed')
  };
 });
}

export async function fetchMaintenanceStatus(): Promise<DatabaseMaintenanceStatus[]> {
 return maintenanceStatusFromUnknown(await responseJson(
  await timedFetch('/api/magician/v2/storage/maintenance', { headers: scopedRequestHeaders() })
 ));
}
