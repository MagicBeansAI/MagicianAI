import { scopedRequestHeaders, scopedUiStorageKey } from '$lib/stores/scopeIdentityStore';
import type { AppDirectoryEntry } from './appDirectory';

const MAX_JSON_BYTES = 256 * 1024;
const MAX_PACKAGE_BYTES = 72 * 1024 * 1024;
const PACKAGE_MEDIA_TYPE = 'application/vnd.app-platform.package+zip';
const MAX_PORTABLE_ARCHIVE_BYTES = 132 * 1024 * 1024;
const ENCRYPTED_ARCHIVE_MEDIA_TYPE = 'application/vnd.magician.app-archive.encrypted+octet-stream';
const PLAINTEXT_ARCHIVE_MEDIA_TYPE = 'application/vnd.magician.app-archive+json';

export type AppInstallationStatus = AppDirectoryEntry['status'];
export type AppLifecycleOperation =
	| 'disable'
	| 'quarantine'
	| 'uninstall_retain'
	| 'revoke_grant'
	| 'begin_update'
	| 'abort_update';

export interface AppLifecycleReceipt {
	installation_id: string;
	generation: number;
	status: AppInstallationStatus;
	review_identity: AppReenableReviewIdentity | null;
}

export interface AppReenableReviewIdentity {
	installation_id: string;
	installation_generation: number;
	package_id: string;
	package_version: string;
	package_content_digest: string;
	package_lock_digest: string;
	grant_revision: number;
	grant_identity_digest: string;
	schema_revision: number;
	schema_identity_digest: string;
	surface_revision: number;
	surface_identity_digest: string;
	global_policy_revision: number;
	implementation_identity_digest: string;
	review_digest: string;
}

export interface AppLifecycleControl {
	operation: AppLifecycleOperation | 'reenable' | 'commit_update' | 'purge';
	label: string;
	dangerous: boolean;
	available: boolean;
	reason?: string;
}

export type AppPurgeTarget =
	| 'active_rows' | 'append_only_record_revisions' | 'database_wal_and_temp'
	| 'scalar_and_search_indexes' | 'package_and_cache_bytes' | 'retained_attachments'
	| 'export_archives' | 'artifact_v2_references' | 'memory_candidates_and_promotions'
	| 'analytics' | 'debug_and_prompt_captures' | 'evaluation_artifacts'
	| 'provider_side_continuations' | 'routes_and_published_surfaces'
	| 'schedules_and_outbox' | 'disclosure_sessions' | 'directory_and_search_projections';

export interface AppPurgeInventoryEntry {
	target: AppPurgeTarget;
	item_count: number;
	byte_count: number;
	maximum_classification: 'public' | 'ordinary' | 'personal' | 'sensitive' | 'secret';
	inventory_digest: string;
}

export interface AppPurgePreview {
	protocol_version: 2;
	preview_ref: string;
	scope_binding_ref: string;
	installation_id: string;
	installation_generation: number;
	selection: { target: 'whole_installation' };
	inventory_entries: AppPurgeInventoryEntry[];
	inventory_snapshot_digest: string;
	preview_digest: string;
	observed_at: string;
	expires_at: string;
}

export interface AppPurgeReceipt {
	protocol_version: 2;
	receipt_ref: string;
	approval_ref: string;
	scope_binding_ref: string;
	installation_id: string;
	installation_generation: number;
	selection_kind: 'whole_installation';
	selection_digest: string;
	preview_digest: string;
	outcomes: Array<{
		target: AppPurgeTarget;
		status: 'deleted' | 'cryptographically_erased' | 'retained_shared' | 'retained_by_policy' | 'provider_retention_unknown' | 'failed';
		affected_items: number;
		affected_bytes: number;
		outcome_digest: string;
		policy_or_ownership_ref?: string;
		failure_ref?: string;
	}>;
	completion: 'fully_erased' | 'completed_with_disclosed_retention' | 'incomplete';
	committed_at: string;
}

export interface AppPackageImportReceipt {
	state: 'staged_for_local_conformance';
	stage_outcome: 'created' | 'already_present';
	package_id: string;
	source_publisher_identity: string;
	semantic_version: string;
	package_content_digest: string;
	requirements: {
		reverify_complete_bundle_digest: true;
		run_local_conformance: true;
		run_local_permission_review: true;
		rebuild_verify_and_sandbox_executable_content_if_present: true;
		foreign_grants_transfer: false;
		portable_evidence_is_advisory_only: true;
	};
	local_identity_resolution_required: true;
	foreign_authority_transferred: false;
}

export interface AppCandidatePublicationReceipt {
	request_id: string;
	state: 'ready_for_review' | 'update_pending' | 'uninstalled_retained';
	stage_outcome: 'created' | 'already_present';
	publication_outcome: 'created' | 'already_present';
	package_revision_ref: string;
	attempt_id: string;
	installation_id: string;
	package_content_digest: string;
	dependency_lock_digest: string;
	local_publisher_identity: string;
	source_publisher_identity: string;
	activation_authority_granted: false;
}

export interface AppCandidatePublicationTarget {
	installation_id: string;
	attempt_kind: 'update' | 'reinstall';
}

export type AppUpdateCoordinatorState =
	| 'dry_run_passed'
	| 'backup_recorded'
	| 'ready_to_switch'
	| 'switched'
	| 'rewind_review_pending'
	| 'aborted'
	| 'rolled_back';

export interface AppUpdatePlanReceipt {
	migration_run_id: string;
	installation_id: string;
	attempt_id: string;
	attempt_kind: 'update' | 'reinstall';
	source_fence_digest: string;
	destination_package_revision_ref: string;
	destination_schema_revision: number;
	destination_dataset_generation: number;
	permission_diff: Record<string, unknown>;
	schema_diff_digest: string;
	surface_diff_digest: string;
	data_diff_digest: string;
	migration_operations: Array<Record<string, unknown>>;
	migration_plan_digest: string | null;
	update_plan_digest: string;
	destructive: boolean;
	backup_required: boolean;
	dry_run_examined: number;
	dry_run_representable: number;
	state: AppUpdateCoordinatorState;
}

export interface AppUpdateRollbackReceipt {
	rollback_ref: string;
	request_id: string;
	kind: 'code_only' | 'data_rewind';
	migration_run_id: string;
	installation_id: string;
	installation_generation: number;
	package_revision_ref: string;
	grant_revision: number;
	schema_revision: number;
	surface_revision: number;
	post_update_writes_retained: boolean;
	grants_restored: false;
	rolled_back_at: string;
}

export interface AppDataRewindPreviewReceipt {
	migration_run_id: string;
	installation_id: string;
	backup_ref: string;
	update_plan_digest: string;
	preview: {
		preview_version: 1;
		source_record_count: number;
		destination_installation_id: string;
		destination_installation_generation: number;
		status: 'ready' | 'requires_review' | 'blocked';
		preview_digest: string;
		record_decisions: Array<Record<string, unknown>>;
		missing_attachments?: Array<Record<string, unknown>>;
		[key: string]: unknown;
	};
}

export interface AppDataRewindCommitReceipt {
	rollback: AppUpdateRollbackReceipt;
	import_receipt: {
		receipt_ref: string;
		preview_digest: string;
		destination_installation_id: string;
		destination_installation_generation: number;
		source_record_count: number;
		created_count: number;
		merged_count: number;
		skipped_count: number;
		[key: string]: unknown;
	};
	record_decisions: Array<Record<string, unknown>>;
}

export interface AppDataImportPreviewReceipt {
	request_id: string;
	archive_receipt: {
		envelope_version: 1;
		logical_payload_digest: string;
		envelope_header_digest: string;
		ciphertext_digest: string;
		byte_count: number;
		encrypted: boolean;
	};
	package_payload_present: boolean;
	foreign_authority_transferred: false;
	preview: {
		preview_version: 1;
		source_record_count: number;
		destination_installation_id: string;
		destination_installation_generation: number;
		status: 'ready' | 'requires_review' | 'blocked';
		preview_digest: string;
		record_decisions: Array<Record<string, unknown>>;
		missing_attachments?: Array<Record<string, unknown>>;
		[key: string]: unknown;
	};
}

export interface AppDataImportApprovalReceipt {
	request_id: string;
	approval_ref: string;
	preview_digest: string;
	expires_at: string;
}

export interface AppDataImportCommitReceipt {
	request_id: string;
	foreign_authority_transferred: false;
	receipt: {
		receipt_ref: string;
		approval_ref: string;
		preview_digest: string;
		destination_installation_id: string;
		destination_installation_generation: number;
		source_record_count: number;
		created_count: number;
		merged_count: number;
		skipped_count: number;
		[key: string]: unknown;
	};
}

interface AppCandidatePublicationIntent {
	version: 1;
	request_id: string;
	package_id: string;
	source_publisher_identity: string;
	package_content_digest: string;
}

const STATUSES = new Set<AppInstallationStatus>([
	'ready_for_review', 'enabled', 'disabled', 'update_pending', 'quarantined',
	'uninstalled_retained', 'purged'
]);

const PURGE_TARGETS = new Set<AppPurgeTarget>([
	'active_rows', 'append_only_record_revisions', 'database_wal_and_temp',
	'scalar_and_search_indexes', 'package_and_cache_bytes', 'retained_attachments',
	'export_archives', 'artifact_v2_references', 'memory_candidates_and_promotions',
	'analytics', 'debug_and_prompt_captures', 'evaluation_artifacts',
	'provider_side_continuations', 'routes_and_published_surfaces',
	'schedules_and_outbox', 'disclosure_sessions', 'directory_and_search_projections'
]);

const PURGE_CLASSIFICATIONS = new Set(['public', 'ordinary', 'personal', 'sensitive', 'secret']);
const PURGE_STATUSES = new Set([
	'deleted', 'cryptographically_erased', 'retained_shared', 'retained_by_policy',
	// A target the server could not settle. Omitting it did not make a failed
	// purge impossible — it made it unreadable: the receipt failed to parse and
	// the owner was told the outcome was "invalid" rather than that part of the
	// purge did not happen.
	'provider_retention_unknown', 'failed'
]);

const OPERATION_ROUTES: Record<AppLifecycleOperation, string> = {
	disable: 'disable',
	quarantine: 'quarantine',
	uninstall_retain: 'uninstall',
	revoke_grant: 'grant-revocations',
	begin_update: 'update-begin',
	abort_update: 'update-abort'
};

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? value as Record<string, unknown>
		: null;
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
	const actual = Object.keys(value).sort();
	const expected = [...keys].sort();
	return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function boundedString(value: unknown, maximum: number): value is string {
	return typeof value === 'string' && value.length > 0 && value.length <= maximum;
}

function safePositiveInteger(value: unknown): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value > 0;
}

function safeNonnegativeInteger(value: unknown): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function digest(value: unknown): value is string {
	return typeof value === 'string' && /^blake3:[0-9a-f]{64}$/.test(value);
}

function timestamp(value: unknown): value is string {
	return boundedString(value, 64) && Number.isFinite(Date.parse(value));
}

function responseError(body: unknown, fallback: string): Error {
	const item = record(body);
	return new Error(item && boundedString(item.message, 4096) ? item.message : fallback);
}

async function readBoundedJson(response: Response): Promise<unknown> {
	const declared = response.headers.get('content-length');
	if (declared !== null) {
		const bytes = Number(declared);
		if (!Number.isSafeInteger(bytes) || bytes < 0 || bytes > MAX_JSON_BYTES) {
			throw new Error('The app lifecycle response exceeded its bound.');
		}
	}
	if (!response.body) return null;
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	while (true) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_JSON_BYTES) {
			await reader.cancel().catch(() => undefined);
			throw new Error('The app lifecycle response exceeded its bound.');
		}
		chunks.push(value);
	}
	const encoded = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		encoded.set(chunk, offset);
		offset += chunk.byteLength;
	}
	const body = new TextDecoder('utf-8', { fatal: true }).decode(encoded);
	if (!body) return null;
	try {
		return JSON.parse(body);
	} catch {
		throw new Error('The app lifecycle response was not valid JSON.');
	}
}

export function appPackageExportAvailability(entry: AppDirectoryEntry): {
	available: boolean;
	reason?: string;
} {
	return ['enabled', 'disabled', 'quarantined', 'uninstalled_retained'].includes(entry.status)
		? { available: true }
		: {
			available: false,
			reason: entry.status === 'purged'
				? 'Purged package bytes are no longer available.'
				: 'Package export is unavailable while this install or update awaits review.'
		};
}

function parseLifecycleReceipt(
	value: unknown,
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'status'>,
	operation: AppLifecycleOperation
): AppLifecycleReceipt {
	const item = record(value);
	const allowedStatuses: Record<AppLifecycleOperation, readonly AppInstallationStatus[]> = {
		disable: ['disabled'],
		quarantine: ['quarantined'],
		uninstall_retain: ['uninstalled_retained'],
		revoke_grant: [entry.status],
		begin_update: ['update_pending'],
		abort_update: ['enabled', 'disabled']
	};
	const reviewed = operation !== 'revoke_grant';
	if (!item || !exactKeys(item, reviewed
		? ['installation_id', 'generation', 'status', 'review_identity']
		: ['installation_id', 'generation', 'status']) ||
		item.installation_id !== entry.installation_id ||
		item.generation !== entry.installation_generation + 1 ||
		!STATUSES.has(item.status as AppInstallationStatus) ||
		(reviewed && item.review_identity !== null) ||
		!allowedStatuses[operation].includes(item.status as AppInstallationStatus)) {
		throw new Error('The app lifecycle receipt did not match the requested installation generation.');
	}
	return {
		installation_id: entry.installation_id,
		generation: item.generation as number,
		status: item.status as AppInstallationStatus,
		review_identity: null
	};
}

function parseReenableReview(
	value: unknown,
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'status'>
): AppReenableReviewIdentity {
	const item = record(value);
	if (entry.status !== 'disabled' || !item || !exactKeys(item, [
		'installation_id', 'installation_generation', 'package_id', 'package_version',
		'package_content_digest', 'package_lock_digest', 'grant_revision',
		'grant_identity_digest', 'schema_revision', 'schema_identity_digest',
		'surface_revision', 'surface_identity_digest', 'global_policy_revision',
		'implementation_identity_digest', 'review_digest'
	]) || item.installation_id !== entry.installation_id ||
		item.installation_generation !== entry.installation_generation ||
		!boundedString(item.package_id, 192) || !boundedString(item.package_version, 128) ||
		!digest(item.package_content_digest) || !digest(item.package_lock_digest) ||
		!safePositiveInteger(item.grant_revision) || !digest(item.grant_identity_digest) ||
		!safePositiveInteger(item.schema_revision) || !digest(item.schema_identity_digest) ||
		!safePositiveInteger(item.surface_revision) || !digest(item.surface_identity_digest) ||
		!safePositiveInteger(item.global_policy_revision) ||
		!digest(item.implementation_identity_digest) || !digest(item.review_digest)) {
		throw new Error('The re-enable review did not match the current disabled installation.');
	}
	return item as unknown as AppReenableReviewIdentity;
}

export function appLifecycleControls(entry: AppDirectoryEntry): AppLifecycleControl[] {
	const status = entry.status;
	const controls: AppLifecycleControl[] = [];
	if (status === 'enabled') {
		controls.push(
			{ operation: 'disable', label: 'Disable', dangerous: false, available: true },
			{ operation: 'begin_update', label: 'Begin update', dangerous: false, available: true },
			{ operation: 'revoke_grant', label: 'Revoke grant', dangerous: true, available: true },
			{ operation: 'quarantine', label: 'Quarantine', dangerous: true, available: true },
			{ operation: 'uninstall_retain', label: 'Uninstall and retain data', dangerous: true, available: true }
		);
	} else if (status === 'disabled') {
		controls.push(
			{ operation: 'reenable', label: 'Review and re-enable', dangerous: false, available: true },
			{ operation: 'begin_update', label: 'Begin update', dangerous: false, available: true },
			{ operation: 'revoke_grant', label: 'Revoke grant', dangerous: true, available: true },
			{ operation: 'quarantine', label: 'Quarantine', dangerous: true, available: true },
			{ operation: 'uninstall_retain', label: 'Uninstall and retain data', dangerous: true, available: true }
		);
	} else if (status === 'update_pending') {
		controls.push(
			{ operation: 'commit_update', label: 'Commit reviewed update', dangerous: false, available: false, reason: 'Load and approve the exact update review above; there is no unreviewed commit route.' },
			{ operation: 'abort_update', label: 'Abort update', dangerous: false, available: true },
			{ operation: 'quarantine', label: 'Quarantine', dangerous: true, available: true }
		);
	} else if (status === 'ready_for_review') {
		controls.push({ operation: 'quarantine', label: 'Quarantine', dangerous: true, available: true });
	} else if (status === 'quarantined') {
		controls.push({ operation: 'uninstall_retain', label: 'Uninstall and retain data', dangerous: true, available: true });
	} else if (status === 'uninstalled_retained') {
		controls.push(
			{ operation: 'reenable', label: 'Reinstall', dangerous: false, available: false, reason: 'A new reviewed reinstall attempt must be published before approval.' },
			{ operation: 'purge', label: 'Review retained-data purge', dangerous: true, available: true }
		);
	}
	return controls;
}

export async function previewAppPurge(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'status'>,
	signal?: AbortSignal
): Promise<AppPurgePreview> {
	if (entry.status !== 'uninstalled_retained') {
		throw new Error('Only an uninstalled-retained app can be previewed for purge.');
	}
	const retained = recoverPurgeIntent(entry);
	if (retained) return retained.preview;
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(entry.installation_id)}/purge-preview`,
		{ method: 'POST', headers: scopedRequestHeaders({ Accept: 'application/json' }), signal }
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The purge preview could not be prepared.');
	return parsePurgePreview(body, entry);
}

export async function commitAppPurge(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'status'>,
	preview: AppPurgePreview,
	signal?: AbortSignal
): Promise<AppPurgeReceipt> {
	if (entry.status !== 'uninstalled_retained' ||
		preview.installation_id !== entry.installation_id ||
		preview.installation_generation !== entry.installation_generation) {
		throw new Error('The purge preview is no longer current for this retained installation.');
	}
	const intent = stagePurgeIntent(entry, preview);
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(entry.installation_id)}/purge`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: JSON.stringify({
				preview_ref: preview.preview_ref,
				preview_digest: preview.preview_digest,
				installation_generation: preview.installation_generation,
				observed_at: preview.observed_at,
				expires_at: preview.expires_at,
				idempotency_key: intent.idempotency_key
			}),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) {
		const errorBody = record(body);
		if (errorBody && [
			'app_purge_commit_invalid', 'app_purge_preview_stale',
			'app_purge_requires_retained_uninstall', 'app_purge_identity_conflict'
		].includes(String(errorBody.error))) {
			// These owner responses are emitted only after the durable exact-replay
			// lookup found no committed receipt, so discarding the local intent is
			// safe. Transport failures and ambiguous 5xx responses retain it.
			clearPurgeIntent(intent);
		}
		throw responseError(body, 'The retained data could not be purged.');
	}
	const receipt = parsePurgeReceipt(body, entry, preview);
	clearPurgeIntent(intent);
	return receipt;
}

function parsePurgePreview(
	value: unknown,
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation'>
): AppPurgePreview {
	const item = record(value);
	const selection = item ? record(item.selection) : null;
	if (!item || !selection || !exactKeys(item, [
		'protocol_version', 'preview_ref', 'scope_binding_ref', 'installation_id',
		'installation_generation', 'selection', 'inventory_entries',
		'inventory_snapshot_digest', 'preview_digest', 'observed_at', 'expires_at'
	]) || !exactKeys(selection, ['target']) || item.protocol_version !== 2 ||
		item.installation_id !== entry.installation_id ||
		item.installation_generation !== entry.installation_generation ||
		selection.target !== 'whole_installation' || !boundedString(item.preview_ref, 192) ||
		!boundedString(item.scope_binding_ref, 192) || !digest(item.inventory_snapshot_digest) ||
		!digest(item.preview_digest) || !timestamp(item.observed_at) || !timestamp(item.expires_at) ||
		Date.parse(item.expires_at) <= Date.parse(item.observed_at) || !Array.isArray(item.inventory_entries) ||
		item.inventory_entries.length !== PURGE_TARGETS.size) {
		throw new Error('The purge preview did not match the retained installation.');
	}
	const inventoryEntries = item.inventory_entries.map(parsePurgeInventoryEntry);
	if (new Set(inventoryEntries.map((inventory) => inventory.target)).size !== PURGE_TARGETS.size ||
		inventoryEntries.some((inventory) => !PURGE_TARGETS.has(inventory.target))) {
		throw new Error('The purge preview did not enumerate every storage owner exactly once.');
	}
	return { ...item, selection: { target: 'whole_installation' }, inventory_entries: inventoryEntries } as AppPurgePreview;
}

function parsePurgeInventoryEntry(value: unknown): AppPurgeInventoryEntry {
	const item = record(value);
	if (!item || !exactKeys(item, [
		'target', 'item_count', 'byte_count', 'maximum_classification', 'inventory_digest'
	]) || !PURGE_TARGETS.has(item.target as AppPurgeTarget) ||
		!safeNonnegativeInteger(item.item_count) || !safeNonnegativeInteger(item.byte_count) ||
		!PURGE_CLASSIFICATIONS.has(String(item.maximum_classification)) || !digest(item.inventory_digest)) {
		throw new Error('The purge preview contained invalid storage accounting.');
	}
	return item as unknown as AppPurgeInventoryEntry;
}

function parsePurgeReceipt(
	value: unknown,
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation'>,
	preview: AppPurgePreview
): AppPurgeReceipt {
	const item = record(value);
	if (!item || !exactKeys(item, [
		'protocol_version', 'receipt_ref', 'approval_ref', 'scope_binding_ref', 'installation_id',
		'installation_generation', 'selection_kind', 'selection_digest', 'preview_digest',
		'outcomes', 'completion', 'committed_at'
	]) || item.protocol_version !== 2 || item.installation_id !== entry.installation_id ||
		item.installation_generation !== entry.installation_generation ||
		item.selection_kind !== 'whole_installation' || item.preview_digest !== preview.preview_digest ||
		item.scope_binding_ref !== preview.scope_binding_ref || !boundedString(item.receipt_ref, 192) ||
		!boundedString(item.approval_ref, 192) || !digest(item.selection_digest) ||
		!timestamp(item.committed_at) || !Array.isArray(item.outcomes) ||
		item.outcomes.length !== PURGE_TARGETS.size ||
		!['fully_erased', 'completed_with_disclosed_retention', 'incomplete'].includes(String(item.completion))) {
		throw new Error('The purge receipt did not match the reviewed preview.');
	}
	const outcomes = item.outcomes.map((value) => parsePurgeOutcome(value));
	if (new Set(outcomes.map((outcome) => outcome.target)).size !== PURGE_TARGETS.size) {
		throw new Error('The purge receipt did not settle every storage owner exactly once.');
	}
	for (const inventory of preview.inventory_entries) {
		const outcome = outcomes.find((candidate) => candidate.target === inventory.target);
		if (!outcome || outcome.affected_items !== inventory.item_count ||
			outcome.affected_bytes !== inventory.byte_count) {
			throw new Error('The purge receipt accounting did not match the reviewed preview.');
		}
	}
	// An unsettled purge is reported as itself. This has to run BEFORE the
	// assertions below: a failed terminal target would otherwise be reported as
	// "retained an installation-owned authority surface", which describes a
	// deliberate retention decision rather than work that did not happen.
	const failedOutcomes = outcomes.filter((outcome) => outcome.status === 'failed');
	if (failedOutcomes.length > 0 || item.completion === 'incomplete') {
		const named = failedOutcomes.map((outcome) => outcome.target).join(', ');
		throw new Error(
			`The purge did not complete: ${failedOutcomes.length} target(s) failed`
			+ `${named ? ` (${named})` : ''}. The installation was not fully removed.`
		);
	}
	const terminalDeletionTargets = new Set<AppPurgeTarget>([
		'active_rows', 'append_only_record_revisions', 'scalar_and_search_indexes',
		'routes_and_published_surfaces', 'schedules_and_outbox', 'disclosure_sessions',
		'directory_and_search_projections'
	]);
	if (outcomes.some((outcome) => terminalDeletionTargets.has(outcome.target) &&
		!['deleted', 'cryptographically_erased'].includes(outcome.status))) {
		throw new Error('The purge receipt retained an installation-owned authority surface.');
	}
	const disclosedRetention = outcomes.some((outcome) => [
		'retained_shared', 'retained_by_policy', 'provider_retention_unknown'
	].includes(outcome.status));
	if ((item.completion === 'fully_erased') === disclosedRetention) {
		throw new Error('The purge completion did not match its disclosed retention outcomes.');
	}
	return { ...item, outcomes } as AppPurgeReceipt;
}

function parsePurgeOutcome(value: unknown): AppPurgeReceipt['outcomes'][number] {
	const item = record(value);
	if (!item) throw new Error('The purge receipt contained an invalid outcome.');
	const keys = ['target', 'status', 'affected_items', 'affected_bytes', 'outcome_digest'];
	if ('policy_or_ownership_ref' in item) keys.push('policy_or_ownership_ref');
	// The server attaches `failure_ref` (and only that) to a failed target,
	// exactly as it attaches `policy_or_ownership_ref` to a retained one.
	if ('failure_ref' in item) keys.push('failure_ref');
	// Each condition names itself. A single collapsed boolean made a receipt the
	// client could not read indistinguishable from one the server got wrong, and
	// the owner saw only "invalid outcome" for either.
	const faults: string[] = [];
	if (!exactKeys(item, keys)) faults.push(`unexpected key(s): [${Object.keys(item).join(', ')}] vs allowed [${keys.join(', ')}]`);
	if (!PURGE_TARGETS.has(item.target as AppPurgeTarget)) faults.push(`unknown target "${String(item.target)}"`);
	if (!PURGE_STATUSES.has(String(item.status))) faults.push(`unknown status "${String(item.status)}"`);
	if (!safeNonnegativeInteger(item.affected_items)) faults.push(`affected_items not a safe non-negative integer: ${JSON.stringify(item.affected_items)}`);
	if (!safeNonnegativeInteger(item.affected_bytes)) faults.push(`affected_bytes not a safe non-negative integer: ${JSON.stringify(item.affected_bytes)}`);
	if (!digest(item.outcome_digest)) faults.push(`outcome_digest not a digest: ${JSON.stringify(item.outcome_digest)}`);
	if ('policy_or_ownership_ref' in item && !boundedString(item.policy_or_ownership_ref, 192)) faults.push('policy_or_ownership_ref is not a bounded string');
	if ('failure_ref' in item && !boundedString(item.failure_ref, 192)) faults.push('failure_ref is not a bounded string');
	if (['retained_shared', 'retained_by_policy'].includes(String(item.status)) && !('policy_or_ownership_ref' in item)) faults.push(`status "${String(item.status)}" without policy_or_ownership_ref`);
	// Mirror `validate_semantics` in magician-apps/src/apps/retention.rs exactly.
	// It enforces three rules and no more: retained statuses must name their
	// retention evidence, `failed` must name its failure evidence, and the three
	// settled statuses must NOT carry failure evidence.
	//
	// This client used to add a fourth rule of its own — that a settled status
	// must not carry `policy_or_ownership_ref` — which the server has never
	// promised. A `deleted` target legitimately names the owner or policy its
	// rows sat under; that is disclosure, not a contradiction. The invented rule
	// rejected valid receipts for successful purges.
	if (String(item.status) === 'failed' && !('failure_ref' in item)) faults.push('failed without failure_ref');
	if (['deleted', 'cryptographically_erased', 'provider_retention_unknown'].includes(String(item.status)) && 'failure_ref' in item) faults.push(`settled status "${String(item.status)}" carries failure_ref`);
	if (faults.length > 0) {
		throw new Error(`The purge receipt contained an invalid outcome. [target=${String(item.target)} status=${String(item.status)}] ${faults.join('; ')}`);
	}
	return item as unknown as AppPurgeReceipt['outcomes'][number];
}

interface PersistedPurgeIntent {
	schema: 'magician.apps.purge-intent.v1';
	storage_key: string;
	installation_id: string;
	installation_generation: number;
	preview_ref: string;
	preview_digest: string;
	idempotency_key: string;
	preview: AppPurgePreview;
}

function stagePurgeIntent(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation'>,
	preview: AppPurgePreview
): PersistedPurgeIntent {
	const storage = safeLocalStorage();
	if (!storage) throw new Error('Durable browser storage is required before a destructive purge can start.');
	const key = scopedUiStorageKey('magician.apps.purge-intent.v1', entry.installation_id);
	const existing = parsePurgeIntent(storage.getItem(key));
	if (existing) {
		const retainedPreview = parsePurgePreview(existing.preview, entry);
		if (existing.storage_key !== key || existing.installation_id !== entry.installation_id ||
			existing.installation_generation !== preview.installation_generation ||
			existing.preview_ref !== preview.preview_ref || existing.preview_digest !== preview.preview_digest ||
			JSON.stringify(retainedPreview) !== JSON.stringify(preview)) {
			throw new Error('A different unresolved purge attempt already exists for this installation.');
		}
		return existing;
	}
	const bytes = new Uint8Array(32);
	crypto.getRandomValues(bytes);
	const intent: PersistedPurgeIntent = {
		schema: 'magician.apps.purge-intent.v1',
		storage_key: key,
		installation_id: entry.installation_id,
		installation_generation: entry.installation_generation,
		preview_ref: preview.preview_ref,
		preview_digest: preview.preview_digest,
		idempotency_key: `blake3:${Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')}`,
		preview
	};
	const encoded = JSON.stringify(intent);
	storage.setItem(key, encoded);
	if (storage.getItem(key) !== encoded) throw new Error('The purge recovery identity could not be persisted.');
	return intent;
}

function parsePurgeIntent(raw: string | null): PersistedPurgeIntent | null {
	if (!raw || raw.length > 32 * 1024) return null;
	try {
		const item = record(JSON.parse(raw));
		if (!item || !exactKeys(item, [
			'schema', 'storage_key', 'installation_id', 'installation_generation', 'preview_ref',
			'preview_digest', 'idempotency_key', 'preview'
		]) || item.schema !== 'magician.apps.purge-intent.v1' ||
			!boundedString(item.storage_key, 1024) || !boundedString(item.installation_id, 128) ||
			!safePositiveInteger(item.installation_generation) ||
			!boundedString(item.preview_ref, 192) || !digest(item.preview_digest) ||
			!digest(item.idempotency_key) || !record(item.preview)) return null;
		return item as unknown as PersistedPurgeIntent;
	} catch {
		return null;
	}
}

function recoverPurgeIntent(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation'>
): PersistedPurgeIntent | null {
	const storage = safeLocalStorage();
	if (!storage) return null;
	const key = scopedUiStorageKey('magician.apps.purge-intent.v1', entry.installation_id);
	const intent = parsePurgeIntent(storage.getItem(key));
	if (!intent) return null;
	const preview = parsePurgePreview(intent.preview, entry);
	if (intent.storage_key !== key || intent.installation_id !== entry.installation_id ||
		intent.installation_generation !== entry.installation_generation ||
		intent.preview_ref !== preview.preview_ref || intent.preview_digest !== preview.preview_digest) {
		throw new Error('The retained purge recovery identity did not match this installation.');
	}
	return { ...intent, preview };
}

function clearPurgeIntent(intent: PersistedPurgeIntent): void {
	const storage = safeLocalStorage();
	if (!storage) return;
	const current = parsePurgeIntent(storage.getItem(intent.storage_key));
	if (current?.idempotency_key === intent.idempotency_key &&
		current.preview_digest === intent.preview_digest) storage.removeItem(intent.storage_key);
}

function safeLocalStorage(): Storage | null {
	try { return globalThis.localStorage ?? null; } catch { return null; }
}

type DurableLifecycleOperation = Exclude<AppLifecycleOperation, 'revoke_grant'> | 'reenable';
interface PersistedLifecycleIntent {
	version: 1;
	installation_id: string;
	expected_generation: number;
	operation: DurableLifecycleOperation;
	request_id: string;
	review_digest?: string;
}

function lifecycleIntentKey(installationId: string, operation: DurableLifecycleOperation): string {
	return scopedUiStorageKey('magician.apps.lifecycle-intent.v1', installationId, operation);
}

function parseLifecycleIntent(value: string | null): PersistedLifecycleIntent | null {
	if (!value) return null;
	try {
		const item = record(JSON.parse(value));
		if (!item || !(exactKeys(item, ['version', 'installation_id', 'expected_generation', 'operation', 'request_id']) ||
			exactKeys(item, ['version', 'installation_id', 'expected_generation', 'operation', 'request_id', 'review_digest'])) || item.version !== 1 ||
			!boundedString(item.installation_id, 128) || !safePositiveInteger(item.expected_generation) ||
			!['disable', 'quarantine', 'uninstall_retain', 'begin_update', 'abort_update', 'reenable'].includes(String(item.operation)) ||
			!boundedString(item.request_id, 192) ||
			(item.review_digest !== undefined && !digest(item.review_digest))) return null;
		return item as unknown as PersistedLifecycleIntent;
	} catch { return null; }
}

function lifecycleIntentBody(intent: PersistedLifecycleIntent): string {
	return JSON.stringify({
		expected_generation: intent.expected_generation,
		request_id: intent.request_id,
		...(intent.review_digest ? { review_digest: intent.review_digest } : {})
	});
}

function stageLifecycleIntent(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation'>,
	operation: DurableLifecycleOperation,
	reviewDigest?: string
): { intent: PersistedLifecycleIntent; body: string; storage_key: string } {
	const storage = safeLocalStorage();
	if (!storage) throw new Error('Durable browser storage is required before a lifecycle control can be sent.');
	const storage_key = lifecycleIntentKey(entry.installation_id, operation);
	const retained = parseLifecycleIntent(storage.getItem(storage_key));
	const intent = retained && retained.installation_id === entry.installation_id &&
		retained.expected_generation === entry.installation_generation &&
		retained.operation === operation && retained.review_digest === reviewDigest
		? retained
		: {
			version: 1 as const,
			installation_id: entry.installation_id,
			expected_generation: entry.installation_generation,
			operation,
			request_id: `lifecycle-request:${globalThis.crypto.randomUUID()}`,
			...(reviewDigest ? { review_digest: reviewDigest } : {})
		};
	storage.setItem(storage_key, JSON.stringify(intent));
	const body = lifecycleIntentBody(intent);
	return { intent, body, storage_key };
}

function clearLifecycleIntent(storageKey: string, intent: PersistedLifecycleIntent): void {
	const storage = safeLocalStorage();
	if (!storage) return;
	if (parseLifecycleIntent(storage.getItem(storageKey))?.request_id === intent.request_id) {
		storage.removeItem(storageKey);
	}
}

const DURABLE_LIFECYCLE_OPERATIONS: readonly DurableLifecycleOperation[] = [
	'disable', 'quarantine', 'uninstall_retain', 'begin_update', 'abort_update', 'reenable'
];

function parseRecoveredLifecycleReceipt(
	value: unknown,
	intent: PersistedLifecycleIntent
): AppLifecycleReceipt {
	const item = record(value);
	const expectedStatuses: Record<DurableLifecycleOperation, readonly AppInstallationStatus[]> = {
		disable: ['disabled'], quarantine: ['quarantined'],
		uninstall_retain: ['uninstalled_retained'], begin_update: ['update_pending'],
		abort_update: ['enabled', 'disabled'], reenable: ['enabled']
	};
	if (!item || !exactKeys(item, ['installation_id', 'generation', 'status', 'review_identity']) ||
		item.installation_id !== intent.installation_id ||
		item.generation !== intent.expected_generation + 1 ||
		!expectedStatuses[intent.operation].includes(item.status as AppInstallationStatus)) {
		throw new Error('The recovered lifecycle receipt did not match its retained generation.');
	}
	if (intent.operation === 'reenable') {
		const review = parseReenableReview(item.review_identity, {
			installation_id: intent.installation_id,
			installation_generation: intent.expected_generation,
			status: 'disabled'
		});
		if (review.review_digest !== intent.review_digest) {
			throw new Error('The recovered re-enable receipt did not match its retained review.');
		}
		return {
			installation_id: intent.installation_id,
			generation: item.generation as number,
			status: 'enabled',
			review_identity: review
		};
	}
	if (item.review_identity !== null) {
		throw new Error('An ordinary lifecycle recovery returned unexpected review authority.');
	}
	return {
		installation_id: intent.installation_id,
		generation: item.generation as number,
		status: item.status as AppInstallationStatus,
		review_identity: null
	};
}

export async function recoverRetainedAppLifecycleIntents(signal?: AbortSignal): Promise<{
	receipts: AppLifecycleReceipt[];
	errors: string[];
}> {
	const storage = safeLocalStorage();
	if (!storage) return { receipts: [], errors: [] };
	const prefix = scopedUiStorageKey('magician.apps.lifecycle-intent.v1');
	const keys: string[] = [];
	const storageEntries = Math.min(storage.length, 256);
	for (let index = 0; index < storageEntries && keys.length < 32; index += 1) {
		const key = storage.key(index);
		if (key?.startsWith(prefix)) keys.push(key);
	}
	keys.sort();
	const receipts: AppLifecycleReceipt[] = [];
	const errors: string[] = [];
	for (const key of keys) {
		if (signal?.aborted) break;
		const intent = parseLifecycleIntent(storage.getItem(key));
		if (!intent || !DURABLE_LIFECYCLE_OPERATIONS.includes(intent.operation) ||
			key !== lifecycleIntentKey(intent.installation_id, intent.operation)) continue;
		const route = intent.operation === 'reenable' ? 'reenable' : OPERATION_ROUTES[intent.operation];
		try {
			const response = await fetch(
				`/api/magician/v2/apps/installations/${encodeURIComponent(intent.installation_id)}/${route}`,
				{
					method: 'POST',
					headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
					body: lifecycleIntentBody(intent),
					signal
				}
			);
			const body = await readBoundedJson(response);
			if (!response.ok) throw responseError(body, 'A retained lifecycle operation could not be recovered.');
			const receipt = parseRecoveredLifecycleReceipt(body, intent);
			clearLifecycleIntent(key, intent);
			receipts.push(receipt);
		} catch (cause) {
			if (signal?.aborted) break;
			errors.push(cause instanceof Error ? cause.message : 'A retained lifecycle operation could not be recovered.');
		}
	}
	return { receipts, errors };
}

export async function runAppLifecycleOperation(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'status'>,
	operation: AppLifecycleOperation,
	signal?: AbortSignal
): Promise<AppLifecycleReceipt> {
	const installationId = entry.installation_id;
	const durableOperation = operation === 'revoke_grant' ? null : operation;
	const staged = durableOperation ? stageLifecycleIntent(entry, durableOperation) : null;
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/${OPERATION_ROUTES[operation]}`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				...(staged ? { 'Content-Type': 'application/json' } : {})
			}),
			...(staged ? { body: staged.body } : {}),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The app lifecycle operation failed.');
	const receipt = parseLifecycleReceipt(body, entry, operation);
	if (staged) clearLifecycleIntent(staged.storage_key, staged.intent);
	return receipt;
}

export async function fetchAppReenableReview(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'status'>,
	signal?: AbortSignal
): Promise<AppReenableReviewIdentity> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(entry.installation_id)}/reenable-review`,
		{ headers: scopedRequestHeaders({ Accept: 'application/json' }), signal }
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The current re-enable review could not be loaded.');
	return parseReenableReview(body, entry);
}

export async function commitAppReenable(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation' | 'status'>,
	review: AppReenableReviewIdentity,
	signal?: AbortSignal
): Promise<AppLifecycleReceipt> {
	if (review.installation_id !== entry.installation_id ||
		review.installation_generation !== entry.installation_generation || entry.status !== 'disabled') {
		throw new Error('The re-enable review is stale for this installation.');
	}
	const staged = stageLifecycleIntent(entry, 'reenable', review.review_digest);
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(entry.installation_id)}/reenable`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: staged.body,
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The reviewed re-enable could not be committed.');
	const item = record(body);
	if (!item || !exactKeys(item, ['installation_id', 'generation', 'status', 'review_identity']) ||
		item.installation_id !== entry.installation_id || item.generation !== entry.installation_generation + 1 ||
		item.status !== 'enabled') throw new Error('The re-enable receipt did not match the reviewed generation.');
	const echoed = parseReenableReview(item.review_identity, entry);
	if (echoed.review_digest !== review.review_digest) {
		throw new Error('The re-enable receipt did not echo the exact displayed review.');
	}
	clearLifecycleIntent(staged.storage_key, staged.intent);
	return { installation_id: entry.installation_id, generation: item.generation as number,
		status: 'enabled', review_identity: echoed };
}

export async function importAppPackage(
	archive: Blob,
	signal?: AbortSignal
): Promise<AppPackageImportReceipt> {
	if (archive.size <= 0 || archive.size > MAX_PACKAGE_BYTES) {
		throw new Error('The app package must be between 1 byte and 72 MiB.');
	}
	const response = await fetch('/api/magician/v2/apps/packages/import', {
		method: 'POST',
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': PACKAGE_MEDIA_TYPE }),
		body: archive,
		signal
	});
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The app package could not be imported.');
	const item = record(body);
	const requirements = item ? record(item.requirements) : null;
	if (!item || !exactKeys(item, [
		'state', 'stage_outcome', 'package_id', 'source_publisher_identity',
		'semantic_version', 'package_content_digest', 'requirements',
		'local_identity_resolution_required', 'foreign_authority_transferred'
	]) || !requirements || !exactKeys(requirements, [
		'reverify_complete_bundle_digest', 'run_local_conformance', 'run_local_permission_review',
		'rebuild_verify_and_sandbox_executable_content_if_present', 'foreign_grants_transfer',
		'portable_evidence_is_advisory_only'
	]) || item.state !== 'staged_for_local_conformance' ||
		!['created', 'already_present'].includes(String(item.stage_outcome)) ||
		!boundedString(item.package_id, 192) || !boundedString(item.source_publisher_identity, 192) ||
		!boundedString(item.semantic_version, 128) || !boundedString(item.package_content_digest, 96) ||
		requirements.reverify_complete_bundle_digest !== true ||
		requirements.run_local_conformance !== true || requirements.run_local_permission_review !== true ||
		requirements.rebuild_verify_and_sandbox_executable_content_if_present !== true ||
		requirements.foreign_grants_transfer !== false ||
		requirements.portable_evidence_is_advisory_only !== true ||
		item.local_identity_resolution_required !== true || item.foreign_authority_transferred !== false
	) throw new Error('The app package import receipt was invalid.');
	return item as unknown as AppPackageImportReceipt;
}

function candidatePublicationIntentKey(packageDigest: string): string {
	return scopedUiStorageKey('magician.apps.candidate-publication-intent.v1', packageDigest);
}

export async function publishAppCandidate(
	archive: Blob,
	staged: AppPackageImportReceipt,
	requestId: string,
	target?: AppCandidatePublicationTarget,
	signal?: AbortSignal
): Promise<AppCandidatePublicationReceipt> {
	if (archive.size <= 0 || archive.size > MAX_PACKAGE_BYTES || !boundedString(requestId, 192)) {
		throw new Error('The candidate publication request was invalid.');
	}
	if (target && (!boundedString(target.installation_id, 128) ||
		!['update', 'reinstall'].includes(target.attempt_kind))) {
		throw new Error('The candidate publication target was invalid.');
	}
	const intent: AppCandidatePublicationIntent = {
		version: 1,
		request_id: requestId,
		package_id: staged.package_id,
		source_publisher_identity: staged.source_publisher_identity,
		package_content_digest: staged.package_content_digest
	};
	const storage = safeLocalStorage();
	const key = candidatePublicationIntentKey(staged.package_content_digest);
	storage?.setItem(key, JSON.stringify(intent));
	const response = await fetch('/api/magician/v2/apps/packages/candidates', {
		method: 'POST',
		headers: scopedRequestHeaders({
			Accept: 'application/json',
			'Content-Type': PACKAGE_MEDIA_TYPE,
			'X-Magician-App-Request-Id': requestId,
			'X-Magician-App-Package-Id': staged.package_id,
			'X-Magician-App-Source-Publisher': staged.source_publisher_identity,
			'X-Magician-App-Content-Digest': staged.package_content_digest,
			...(target ? {
				'X-Magician-App-Target-Installation': target.installation_id,
				'X-Magician-App-Attempt-Kind': target.attempt_kind
			} : {})
		}),
		body: archive,
		signal
	});
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The review candidate could not be published.');
	const item = record(body);
	if (!item || !exactKeys(item, [
		'request_id', 'state', 'stage_outcome', 'publication_outcome', 'package_revision_ref',
		'attempt_id', 'installation_id', 'package_content_digest', 'dependency_lock_digest',
		'local_publisher_identity', 'source_publisher_identity', 'activation_authority_granted'
	]) || item.request_id !== requestId ||
		!['ready_for_review', 'update_pending', 'uninstalled_retained'].includes(String(item.state)) ||
		!['created', 'already_present'].includes(String(item.stage_outcome)) ||
		!['created', 'already_present'].includes(String(item.publication_outcome)) ||
		item.package_content_digest !== staged.package_content_digest ||
		item.source_publisher_identity !== staged.source_publisher_identity ||
		!boundedString(item.package_revision_ref, 192) || !boundedString(item.attempt_id, 192) ||
		!boundedString(item.installation_id, 128) || !digest(item.dependency_lock_digest) ||
		!boundedString(item.local_publisher_identity, 192) || item.activation_authority_granted !== false
	) throw new Error('The candidate publication receipt did not match the staged package request.');
	storage?.removeItem(key);
	return item as unknown as AppCandidatePublicationReceipt;
}

const UPDATE_STATES: AppUpdateCoordinatorState[] = [
	'dry_run_passed', 'backup_recorded', 'ready_to_switch', 'switched',
	'rewind_review_pending', 'aborted', 'rolled_back'
];

function parseUpdatePlan(value: unknown): AppUpdatePlanReceipt | null {
	const item = record(value);
	if (!item || !exactKeys(item, [
		'migration_run_id', 'installation_id', 'attempt_id', 'attempt_kind',
		'source_fence_digest', 'destination_package_revision_ref',
		'destination_schema_revision', 'destination_dataset_generation', 'permission_diff',
		'schema_diff_digest', 'surface_diff_digest', 'data_diff_digest', 'migration_operations',
		'migration_plan_digest', 'update_plan_digest', 'destructive', 'backup_required',
		'dry_run_examined', 'dry_run_representable', 'state'
	])) return null;
	if (!boundedString(item.migration_run_id, 192) || !boundedString(item.installation_id, 128) ||
		!boundedString(item.attempt_id, 192) ||
		!['update', 'reinstall'].includes(String(item.attempt_kind)) ||
		!digest(item.source_fence_digest) || !boundedString(item.destination_package_revision_ref, 192) ||
		!safePositiveInteger(item.destination_schema_revision) ||
		!safeNonnegativeInteger(item.destination_dataset_generation) ||
		!record(item.permission_diff) || !digest(item.schema_diff_digest) ||
		!digest(item.surface_diff_digest) || !digest(item.data_diff_digest) ||
		!Array.isArray(item.migration_operations) || item.migration_operations.length > 256 ||
		item.migration_operations.some((operation) => !record(operation)) ||
		(item.migration_plan_digest !== null && !digest(item.migration_plan_digest)) ||
		// A code-only update preserves the initial dataset generation (zero).
		// A record migration must advance it to a positive generation.
		(item.destination_dataset_generation === 0 &&
			(item.migration_plan_digest !== null || item.migration_operations.length !== 0)) ||
		!digest(item.update_plan_digest) || typeof item.destructive !== 'boolean' ||
		typeof item.backup_required !== 'boolean' || !safeNonnegativeInteger(item.dry_run_examined) ||
		!safeNonnegativeInteger(item.dry_run_representable) ||
		!UPDATE_STATES.includes(item.state as AppUpdateCoordinatorState)) return null;
	return item as unknown as AppUpdatePlanReceipt;
}

function parseUpdateRollback(value: unknown): AppUpdateRollbackReceipt | null {
	const item = record(value);
	if (!item || !exactKeys(item, [
		'rollback_ref', 'request_id', 'kind', 'migration_run_id', 'installation_id',
		'installation_generation', 'package_revision_ref', 'grant_revision', 'schema_revision',
		'surface_revision', 'post_update_writes_retained', 'grants_restored', 'rolled_back_at'
	]) || !boundedString(item.rollback_ref, 192) || !boundedString(item.request_id, 192) ||
		!['code_only', 'data_rewind'].includes(String(item.kind)) ||
		!boundedString(item.migration_run_id, 192) || !boundedString(item.installation_id, 128) ||
		!safePositiveInteger(item.installation_generation) ||
		!boundedString(item.package_revision_ref, 192) || !safePositiveInteger(item.grant_revision) ||
		!safePositiveInteger(item.schema_revision) || !safePositiveInteger(item.surface_revision) ||
		typeof item.post_update_writes_retained !== 'boolean' || item.grants_restored !== false ||
		!timestamp(item.rolled_back_at)) return null;
	return item as unknown as AppUpdateRollbackReceipt;
}

export async function prepareAppUpdatePlan(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation'>,
	attemptId: string,
	migrationOperations: Array<Record<string, unknown>> = [],
	signal?: AbortSignal
): Promise<AppUpdatePlanReceipt> {
	if (!boundedString(entry.installation_id, 128) ||
		!safePositiveInteger(entry.installation_generation) || !boundedString(attemptId, 192) ||
		migrationOperations.length > 256 || migrationOperations.some((operation) => !record(operation))) {
		throw new Error('The update-plan request was invalid.');
	}
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(entry.installation_id)}/update-plans`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: JSON.stringify({
				attempt_id: attemptId,
				expected_parked_generation: entry.installation_generation,
				migration_operations: migrationOperations
			}),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The exact update migration plan could not be prepared.');
	const receipt = parseUpdatePlan(body);
	if (!receipt || receipt.installation_id !== entry.installation_id || receipt.attempt_id !== attemptId) {
		throw new Error('The update-plan receipt did not match the reviewed installation.');
	}
	return receipt;
}

export async function backupAppUpdate(
	plan: AppUpdatePlanReceipt,
	passphrase: string,
	signal?: AbortSignal
): Promise<AppUpdatePlanReceipt> {
	if (!boundedString(plan.migration_run_id, 192) || !validArchivePassphrase(passphrase)) {
		throw new Error('The encrypted update-backup request was invalid.');
	}
	const response = await fetch(
		`/api/magician/v2/apps/updates/${encodeURIComponent(plan.migration_run_id)}/backup`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				'X-Magician-App-Archive-Passphrase': passphrase
			}),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The encrypted pre-update backup could not be recorded.');
	const receipt = parseUpdatePlan(body);
	if (!receipt || receipt.migration_run_id !== plan.migration_run_id ||
		receipt.update_plan_digest !== plan.update_plan_digest || receipt.state !== 'ready_to_switch') {
		throw new Error('The update-backup receipt did not match the reviewed plan.');
	}
	return receipt;
}

export async function rollbackCodeAppUpdate(
	entry: Pick<AppDirectoryEntry, 'installation_id' | 'installation_generation'>,
	plan: AppUpdatePlanReceipt,
	signal?: AbortSignal
): Promise<AppUpdateRollbackReceipt> {
	const requestId = `update-code-rollback:${crypto.randomUUID()}`;
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(entry.installation_id)}/rollbacks/code`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: JSON.stringify({
				migration_run_id: plan.migration_run_id,
				expected_installation_generation: entry.installation_generation,
				request_id: requestId
			}),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The code-only update could not be rolled back.');
	const receipt = parseUpdateRollback(body);
	if (!receipt || receipt.request_id !== requestId || receipt.kind !== 'code_only' ||
		receipt.installation_id !== entry.installation_id ||
		receipt.migration_run_id !== plan.migration_run_id) {
		throw new Error('The code rollback receipt did not match the exact switched update.');
	}
	return receipt;
}

export async function previewAppDataRewind(
	plan: AppUpdatePlanReceipt,
	passphrase: string,
	signal?: AbortSignal
): Promise<AppDataRewindPreviewReceipt> {
	if (!validArchivePassphrase(passphrase)) throw new Error('The backup passphrase was invalid.');
	const response = await fetch(
		`/api/magician/v2/apps/updates/${encodeURIComponent(plan.migration_run_id)}/rewind-preview`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				'X-Magician-App-Archive-Passphrase': passphrase
			}),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The data-rewind preview could not be prepared.');
	const item = record(body);
	const preview = item ? record(item.preview) : null;
	if (!item || !exactKeys(item, [
		'migration_run_id', 'installation_id', 'backup_ref', 'update_plan_digest', 'preview'
	]) || item.migration_run_id !== plan.migration_run_id ||
		item.installation_id !== plan.installation_id || item.update_plan_digest !== plan.update_plan_digest ||
		!boundedString(item.backup_ref, 192) || !preview || !digest(preview.preview_digest) ||
		!safeNonnegativeInteger(preview.source_record_count) ||
		!['ready', 'requires_review', 'blocked'].includes(String(preview.status)) ||
		!Array.isArray(preview.record_decisions) || preview.record_decisions.length > 10_000 ||
		preview.record_decisions.some((decision) => !record(decision))) {
		throw new Error('The data-rewind preview did not match the exact switched update.');
	}
	return item as unknown as AppDataRewindPreviewReceipt;
}

export async function commitAppDataRewind(
	previewReceipt: AppDataRewindPreviewReceipt,
	passphrase: string,
	signal?: AbortSignal
): Promise<AppDataRewindCommitReceipt> {
	if (!validArchivePassphrase(passphrase)) throw new Error('The backup passphrase was invalid.');
	const requestId = `update-data-rewind:${crypto.randomUUID()}`;
	const response = await fetch(
		`/api/magician/v2/apps/updates/${encodeURIComponent(previewReceipt.migration_run_id)}/rewind-commit`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				'Content-Type': 'application/json',
				'X-Magician-App-Archive-Passphrase': passphrase
			}),
			body: JSON.stringify({
				migration_run_id: previewReceipt.migration_run_id,
				preview_digest: previewReceipt.preview.preview_digest,
				request_id: requestId,
				explicit_rewind_confirmed: true
			}),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The reviewed data rewind could not be committed.');
	const item = record(body);
	const rollback = item ? parseUpdateRollback(item.rollback) : null;
	const importReceipt = item ? record(item.import_receipt) : null;
	if (!item || !exactKeys(item, ['rollback', 'import_receipt', 'record_decisions']) ||
		!rollback || rollback.request_id !== requestId || rollback.kind !== 'data_rewind' ||
		rollback.migration_run_id !== previewReceipt.migration_run_id || !importReceipt ||
		importReceipt.preview_digest !== previewReceipt.preview.preview_digest ||
		!Array.isArray(item.record_decisions) || item.record_decisions.length > 10_000 ||
		item.record_decisions.some((decision) => !record(decision))) {
		throw new Error('The data-rewind receipt did not match the reviewed preview.');
	}
	return item as unknown as AppDataRewindCommitReceipt;
}

export async function exportAppPackage(
	installationId: string,
	signal?: AbortSignal
): Promise<Blob> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/package-export`,
		{ headers: scopedRequestHeaders({ Accept: PACKAGE_MEDIA_TYPE }), signal }
	);
	if (!response.ok) {
		let body: unknown = null;
		try { body = await readBoundedJson(response); } catch { /* keep public fallback */ }
		throw responseError(body, 'The app package could not be exported.');
	}
	const contentType = response.headers.get('content-type')?.split(';', 1)[0]?.trim();
	const declared = Number(response.headers.get('content-length'));
	if (contentType !== PACKAGE_MEDIA_TYPE || !Number.isSafeInteger(declared) ||
		declared <= 0 || declared > MAX_PACKAGE_BYTES || !response.body) {
		throw new Error('The app package export response was invalid.');
	}
	const reader = response.body.getReader();
	const chunks: ArrayBuffer[] = [];
	let total = 0;
	while (true) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_PACKAGE_BYTES || total > declared) {
			await reader.cancel().catch(() => undefined);
			throw new Error('The app package export exceeded its declared bound.');
		}
		const copy = new Uint8Array(value.byteLength);
		copy.set(value);
		chunks.push(copy.buffer);
	}
	if (total !== declared) throw new Error('The app package export was truncated.');
	return new Blob(chunks, { type: PACKAGE_MEDIA_TYPE });
}

async function readBoundedBlobResponse(
	response: Response,
	maximumBytes: number,
	expectedMediaTypes: readonly string[]
): Promise<Blob> {
	const contentType = response.headers.get('content-type')?.split(';', 1)[0]?.trim();
	const declared = Number(response.headers.get('content-length'));
	if (!contentType || !expectedMediaTypes.includes(contentType) || !Number.isSafeInteger(declared) ||
		declared <= 0 || declared > maximumBytes || !response.body) {
		throw new Error('The app archive response was invalid.');
	}
	const reader = response.body.getReader();
	const chunks: ArrayBuffer[] = [];
	let total = 0;
	while (true) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > maximumBytes || total > declared) {
			await reader.cancel().catch(() => undefined);
			throw new Error('The app archive exceeded its declared bound.');
		}
		const copy = new Uint8Array(value.byteLength);
		copy.set(value);
		chunks.push(copy.buffer);
	}
	if (total !== declared) throw new Error('The app archive response was truncated.');
	return new Blob(chunks, { type: contentType });
}

function validArchivePassphrase(value: string): boolean {
	return value.length >= 12 && value.length <= 1024 && /^[ -~]+$/.test(value);
}

export async function exportAppDataArchive(
	installationId: string,
	kind: 'data' | 'combined',
	requestId: string,
	passphrase: string,
	signal?: AbortSignal
): Promise<Blob> {
	if (!boundedString(installationId, 128) || !boundedString(requestId, 192) ||
		!validArchivePassphrase(passphrase)) throw new Error('The encrypted app export request was invalid.');
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/portable-exports`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: ENCRYPTED_ARCHIVE_MEDIA_TYPE,
				'Content-Type': 'application/json',
				'X-Magician-App-Archive-Passphrase': passphrase
			}),
			body: JSON.stringify({
				request_id: requestId,
				kind,
				protection: 'default',
				warned_plaintext_confirmed: false
			}),
			signal
		}
	);
	if (!response.ok) {
		let body: unknown = null;
		try { body = await readBoundedJson(response); } catch { /* public fallback */ }
		throw responseError(body, 'The app data archive could not be exported.');
	}
	if (response.headers.get('x-magician-app-request-id') !== requestId ||
		!digest(response.headers.get('x-magician-app-logical-digest')) ||
		!digest(response.headers.get('x-magician-app-envelope-digest')) ||
		!digest(response.headers.get('x-magician-app-ciphertext-digest'))) {
		throw new Error('The app data export receipt did not match the request.');
	}
	return readBoundedBlobResponse(response, MAX_PORTABLE_ARCHIVE_BYTES, [ENCRYPTED_ARCHIVE_MEDIA_TYPE]);
}

function parseDataImportPreview(value: unknown, requestId: string, installationId: string): AppDataImportPreviewReceipt {
	const item = record(value);
	const receipt = item ? record(item.archive_receipt) : null;
	const preview = item ? record(item.preview) : null;
	if (!item || !exactKeys(item, [
		'request_id', 'archive_receipt', 'package_payload_present',
		'foreign_authority_transferred', 'preview'
	]) || item.request_id !== requestId || item.foreign_authority_transferred !== false ||
		typeof item.package_payload_present !== 'boolean' || !receipt || !exactKeys(receipt, [
			'envelope_version', 'logical_payload_digest', 'envelope_header_digest',
			'ciphertext_digest', 'byte_count', 'encrypted'
		]) || receipt.envelope_version !== 1 || !digest(receipt.logical_payload_digest) ||
		!digest(receipt.envelope_header_digest) || !digest(receipt.ciphertext_digest) ||
		!safePositiveInteger(receipt.byte_count) || typeof receipt.encrypted !== 'boolean' ||
		!preview || preview.preview_version !== 1 || preview.destination_installation_id !== installationId ||
		!safePositiveInteger(preview.destination_installation_generation) ||
		!safePositiveInteger(preview.source_record_count) || !digest(preview.preview_digest) ||
		!['ready', 'requires_review', 'blocked'].includes(String(preview.status)) ||
		!Array.isArray(preview.record_decisions) || preview.record_decisions.length !== preview.source_record_count ||
		(preview.missing_attachments !== undefined && !Array.isArray(preview.missing_attachments))
	) throw new Error('The data-import preview receipt did not match the requested destination.');
	return item as unknown as AppDataImportPreviewReceipt;
}

export async function previewAppDataImport(
	installationId: string,
	archive: Blob,
	requestId: string,
	passphrase: string | null,
	signal?: AbortSignal
): Promise<AppDataImportPreviewReceipt> {
	if (archive.size <= 0 || archive.size > MAX_PORTABLE_ARCHIVE_BYTES ||
		!boundedString(requestId, 192) || (passphrase !== null && !validArchivePassphrase(passphrase))) {
		throw new Error('The app data-import preview request was invalid.');
	}
	const encrypted = passphrase !== null;
	const intentKey = scopedUiStorageKey('magician.apps.data-import-preview-intent.v1', installationId, requestId);
	const storage = safeLocalStorage();
	storage?.setItem(intentKey, JSON.stringify({ version: 1, installation_id: installationId, request_id: requestId }));
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/data-imports/preview`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				'Content-Type': encrypted ? ENCRYPTED_ARCHIVE_MEDIA_TYPE : PLAINTEXT_ARCHIVE_MEDIA_TYPE,
				'X-Magician-App-Request-Id': requestId,
				...(passphrase === null ? {} : { 'X-Magician-App-Archive-Passphrase': passphrase })
			}),
			body: archive,
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The app data archive could not be previewed.');
	const preview = parseDataImportPreview(body, requestId, installationId);
	storage?.removeItem(intentKey);
	return preview;
}

export async function approveAppDataImport(
	installationId: string,
	previewDigest: string,
	requestId: string,
	signal?: AbortSignal
): Promise<AppDataImportApprovalReceipt> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/data-imports/approve`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: JSON.stringify({ request_id: requestId, preview_digest: previewDigest }),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The app data import could not be approved.');
	const item = record(body);
	if (!item || !exactKeys(item, ['request_id', 'approval_ref', 'preview_digest', 'expires_at']) ||
		item.request_id !== requestId || item.preview_digest !== previewDigest ||
		!boundedString(item.approval_ref, 192) || !timestamp(item.expires_at)) {
		throw new Error('The app data-import approval did not match the reviewed preview.');
	}
	return item as unknown as AppDataImportApprovalReceipt;
}

export async function commitAppDataImport(
	installationId: string,
	previewDigest: string,
	approvalRef: string,
	requestId: string,
	signal?: AbortSignal
): Promise<AppDataImportCommitReceipt> {
	const intentKey = scopedUiStorageKey('magician.apps.data-import-commit-intent.v1', installationId, previewDigest);
	const storage = safeLocalStorage();
	storage?.setItem(intentKey, JSON.stringify({ version: 1, installation_id: installationId,
		preview_digest: previewDigest, approval_ref: approvalRef, request_id: requestId }));
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/data-imports/commit`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: JSON.stringify({ request_id: requestId, preview_digest: previewDigest, approval_ref: approvalRef }),
			signal
		}
	);
	const body = await readBoundedJson(response);
	if (!response.ok) throw responseError(body, 'The reviewed app data import could not be committed.');
	const item = record(body);
	const receipt = item ? record(item.receipt) : null;
	if (!item || !exactKeys(item, ['request_id', 'foreign_authority_transferred', 'receipt']) ||
		item.request_id !== requestId || item.foreign_authority_transferred !== false || !receipt ||
		receipt.approval_ref !== approvalRef || receipt.preview_digest !== previewDigest ||
		receipt.destination_installation_id !== installationId ||
		!safePositiveInteger(receipt.destination_installation_generation) ||
		!safePositiveInteger(receipt.source_record_count) || !safeNonnegativeInteger(receipt.created_count) ||
		!safeNonnegativeInteger(receipt.merged_count) || !safeNonnegativeInteger(receipt.skipped_count) ||
		Number(receipt.created_count) + Number(receipt.merged_count) + Number(receipt.skipped_count) !== receipt.source_record_count
	) throw new Error('The app data-import commit receipt did not match the reviewed preview.');
	storage?.removeItem(intentKey);
	return item as unknown as AppDataImportCommitReceipt;
}

export async function recoverRetainedAppDataImportCommits(
	signal?: AbortSignal
): Promise<{ receipts: AppDataImportCommitReceipt[]; errors: string[] }> {
	const storage = safeLocalStorage();
	if (!storage) return { receipts: [], errors: [] };
	const prefix = scopedUiStorageKey('magician.apps.data-import-commit-intent.v1');
	const keys = Array.from({ length: storage.length }, (_, index) => storage.key(index))
		.filter((key): key is string => Boolean(key?.startsWith(prefix)))
		.slice(0, 16);
	const receipts: AppDataImportCommitReceipt[] = [];
	const errors: string[] = [];
	for (const key of keys) {
		if (signal?.aborted) break;
		let value: unknown;
		try { value = JSON.parse(storage.getItem(key) ?? 'null'); } catch { value = null; }
		const intent = record(value);
		if (!intent || !exactKeys(intent, [
			'version', 'installation_id', 'preview_digest', 'approval_ref', 'request_id'
		]) || intent.version !== 1 || !boundedString(intent.installation_id, 128) ||
		!digest(intent.preview_digest) || !boundedString(intent.approval_ref, 192) ||
		!boundedString(intent.request_id, 192)) {
			storage.removeItem(key);
			continue;
		}
		try {
			receipts.push(await commitAppDataImport(
				intent.installation_id,
				intent.preview_digest,
				intent.approval_ref,
				intent.request_id,
				signal
			));
		} catch (cause) {
			if (signal?.aborted) break;
			errors.push(cause instanceof Error ? cause.message : 'A retained data-import commit could not be recovered.');
		}
	}
	return { receipts, errors };
}
