import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

const MAX_RESPONSE_BYTES = 800 * 1024;
const MAX_ITEMS = 32;
const MEMORY_STATES = new Set(['proposed', 'accepted', 'rejected', 'stale', 'tombstoned']);
const RETRIEVAL_STATES = new Set(['accepted', 'stale', 'tombstoned']);
const MEMORY_REASONS = new Set(['awaiting_owner_review', 'owner_accepted', 'owner_rejected', 'owner_revoked', 'source_invalidated', 'compacted_legacy']);
const INVALIDATION_REASONS = new Set(['source_updated', 'source_deleted', 'source_restored', 'source_forgotten', 'policy_changed', 'grant_revoked', 'installation_disabled', 'installation_quarantined', 'installation_uninstalled_retained', 'installation_purged', 'contribution_expired']);

export interface AppContributionStateItem {
	destination: 'memory' | 'retrieval';
	proposalId: string;
	proposalDigest: string;
	installationId: string;
	sourceEventRef: string;
	sourceEventRevision: number;
	state: 'proposed' | 'accepted' | 'rejected' | 'stale' | 'tombstoned';
	reason: string;
	stateChangedAtMs: number;
	expiresAtMs?: number;
	retainedUntilMs?: number;
	claimOrSummary?: string;
	detailsCompacted: boolean;
	targetAgentId?: string;
	targetGoalId?: string;
}

export interface AppContributionStateSnapshot {
	memoryGeneration: number;
	retrievalGeneration?: number;
	retrievalAvailable: boolean;
	items: AppContributionStateItem[];
}

function object(value: unknown): Record<string, unknown> {
	if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Contribution state is not an object.');
	return value as Record<string, unknown>;
}

function exact(value: Record<string, unknown>, allowed: readonly string[], label: string): void {
	const keys = Object.keys(value);
	if (keys.some((key) => !allowed.includes(key))) throw new Error(`${label} contains an unknown field.`);
}

function text(value: unknown, label: string, maximum = 512): string {
	if (typeof value !== 'string' || value.length === 0 || new TextEncoder().encode(value).length > maximum) {
		throw new Error(`${label} is invalid.`);
	}
	return value;
}

function natural(value: unknown, label: string): number {
	if (!Number.isSafeInteger(value) || Number(value) < 0) throw new Error(`${label} is invalid.`);
	return Number(value);
}

function optionalNatural(value: unknown, label: string): number | undefined {
	return value === undefined ? undefined : natural(value, label);
}

function optionalText(value: unknown, label: string, maximum = 64 * 1024): string | undefined {
	return value === undefined ? undefined : text(value, label, maximum);
}

function digest(value: unknown, label: string): string {
	const parsed = text(value, label, 71);
	if (!/^blake3:[0-9a-f]{64}$/.test(parsed)) throw new Error(`${label} is invalid.`);
	return parsed;
}

async function boundedJson(response: Response): Promise<unknown> {
	const declared = Number(response.headers.get('content-length') ?? '0');
	if (Number.isFinite(declared) && declared > MAX_RESPONSE_BYTES) throw new Error('Contribution state response is oversized.');
	const reader = response.body?.getReader();
	if (!reader) throw new Error('Contribution state response has no body.');
	const chunks: Uint8Array[] = [];
	let length = 0;
	while (true) {
		const { value, done } = await reader.read();
		if (done) break;
		if (!value) continue;
		length += value.byteLength;
		if (length > MAX_RESPONSE_BYTES) {
			await reader.cancel();
			throw new Error('Contribution state response is oversized.');
		}
		chunks.push(value);
	}
	const bytes = new Uint8Array(length);
	let offset = 0;
	for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
	return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
}

function parseMemoryItem(value: unknown): AppContributionStateItem {
	const row = object(value);
	exact(row, ['proposal_id', 'proposal_digest', 'installation_id', 'workflow_id', 'action_id', 'contribution_port_id', 'source_event_ref', 'source_event_revision', 'state', 'reason', 'source_invalidation_reason', 'state_changed_at_ms', 'expires_at_ms', 'retained_until_ms', 'claim_or_summary', 'details_compacted', 'revoke_review'], 'Memory contribution state');
	const state = text(row.state, 'Memory contribution state', 32);
	if (!MEMORY_STATES.has(state)) throw new Error('Memory contribution state is unknown.');
	const reason = text(row.reason, 'Memory state reason', 64);
	if (!MEMORY_REASONS.has(reason)) throw new Error('Memory state reason is unknown.');
	const sourceReason = optionalText(row.source_invalidation_reason, 'Memory invalidation reason', 64);
	if (sourceReason !== undefined && !INVALIDATION_REASONS.has(sourceReason)) throw new Error('Memory invalidation reason is unknown.');
	if ((reason === 'source_invalidated') !== (sourceReason !== undefined)) throw new Error('Memory source reason correlation is invalid.');
	if ((state === 'proposed') !== (reason === 'awaiting_owner_review') ||
		(state === 'accepted') !== (reason === 'owner_accepted') ||
		(state === 'rejected') !== (reason === 'owner_rejected') ||
		(state === 'stale') !== (reason === 'source_invalidated' && ['source_updated', 'source_restored', 'policy_changed'].includes(sourceReason ?? ''))) {
		throw new Error('Memory state disposition is inconsistent.');
	}
	if (typeof row.details_compacted !== 'boolean') throw new Error('Memory contribution compaction flag is invalid.');
	const live = state === 'proposed' || state === 'accepted';
	if (row.details_compacted === live || live !== (row.claim_or_summary !== undefined) ||
		(state === 'accepted') !== (row.retained_until_ms !== undefined) ||
		(live && row.expires_at_ms === undefined)) {
		throw new Error('Memory contribution detail and retention shape is inconsistent.');
	}
	return {
		destination: 'memory', proposalId: text(row.proposal_id, 'Memory proposal id'),
		proposalDigest: digest(row.proposal_digest, 'Memory proposal digest'),
		installationId: text(row.installation_id, 'Memory installation id'),
		sourceEventRef: text(row.source_event_ref, 'Memory source reference'),
		sourceEventRevision: natural(row.source_event_revision, 'Memory source revision'),
		state: state as AppContributionStateItem['state'],
		reason: sourceReason ?? reason,
		stateChangedAtMs: natural(row.state_changed_at_ms, 'Memory state timestamp'),
		expiresAtMs: optionalNatural(row.expires_at_ms, 'Memory expiry'),
		retainedUntilMs: optionalNatural(row.retained_until_ms, 'Memory retention'),
		claimOrSummary: optionalText(row.claim_or_summary, 'Memory summary'),
		detailsCompacted: row.details_compacted
	};
}

function parseRetrievalItem(value: unknown): AppContributionStateItem {
	const row = object(value);
	exact(row, ['proposal_id', 'proposal_digest', 'installation_id', 'source_event_ref', 'source_event_revision', 'state', 'source_invalidation_reason', 'state_changed_at_ms', 'expires_at_ms', 'target_agent_id', 'target_goal_id', 'details_compacted'], 'Retrieval contribution state');
	const state = text(row.state, 'Retrieval contribution state', 32);
	if (!RETRIEVAL_STATES.has(state)) throw new Error('Retrieval contribution state is unknown.');
	const sourceReason = optionalText(row.source_invalidation_reason, 'Retrieval invalidation reason', 64);
	if (sourceReason !== undefined && !INVALIDATION_REASONS.has(sourceReason)) throw new Error('Retrieval invalidation reason is unknown.');
	if (state === 'accepted' && sourceReason !== undefined) throw new Error('Live retrieval state cannot carry a terminal reason.');
	if (typeof row.details_compacted !== 'boolean') throw new Error('Retrieval contribution compaction flag is invalid.');
	if ((state === 'accepted') === row.details_compacted ||
		(state === 'accepted' && row.expires_at_ms === undefined)) {
		throw new Error('Retrieval contribution detail shape is inconsistent.');
	}
	return {
		destination: 'retrieval', proposalId: text(row.proposal_id, 'Retrieval proposal id'),
		proposalDigest: digest(row.proposal_digest, 'Retrieval proposal digest'),
		installationId: text(row.installation_id, 'Retrieval installation id'),
		sourceEventRef: text(row.source_event_ref, 'Retrieval source reference'),
		sourceEventRevision: natural(row.source_event_revision, 'Retrieval source revision'),
		state: state as AppContributionStateItem['state'],
		reason: sourceReason ?? 'reviewed_projection',
		stateChangedAtMs: natural(row.state_changed_at_ms, 'Retrieval state timestamp'),
		expiresAtMs: optionalNatural(row.expires_at_ms, 'Retrieval expiry'),
		detailsCompacted: row.details_compacted,
		targetAgentId: text(row.target_agent_id, 'Retrieval target agent'),
		targetGoalId: optionalText(row.target_goal_id, 'Retrieval target goal')
	};
}

export function parseAppContributionState(value: unknown): AppContributionStateSnapshot {
	const root = object(value);
	exact(root, ['memory', 'retrieval', 'retrieval_available'], 'Contribution state response');
	if (typeof root.retrieval_available !== 'boolean') throw new Error('Retrieval availability is invalid.');
	const memory = object(root.memory);
	exact(memory, ['destination_generation', 'destination_receipt_digest', 'items'], 'Memory state snapshot');
	if (!Array.isArray(memory.items) || memory.items.length > MAX_ITEMS) throw new Error('Memory state page is invalid.');
	if (memory.destination_receipt_digest !== undefined) digest(memory.destination_receipt_digest, 'Memory receipt digest');
	const memoryGeneration = natural(memory.destination_generation, 'Memory generation');
	if ((memoryGeneration === 0) !== (memory.destination_receipt_digest === undefined)) throw new Error('Memory destination head is inconsistent.');
	let retrievalGeneration: number | undefined;
	let retrievalItems: AppContributionStateItem[] = [];
	if (root.retrieval !== undefined) {
		const retrieval = object(root.retrieval);
		exact(retrieval, ['destination_generation', 'destination_receipt_digest', 'items'], 'Retrieval state snapshot');
		if (!Array.isArray(retrieval.items) || retrieval.items.length > MAX_ITEMS) throw new Error('Retrieval state page is invalid.');
		if (retrieval.destination_receipt_digest !== undefined) digest(retrieval.destination_receipt_digest, 'Retrieval receipt digest');
		retrievalGeneration = natural(retrieval.destination_generation, 'Retrieval generation');
		if ((retrievalGeneration === 0) !== (retrieval.destination_receipt_digest === undefined)) throw new Error('Retrieval destination head is inconsistent.');
		retrievalItems = retrieval.items.map(parseRetrievalItem);
	}
	if (root.retrieval_available !== (root.retrieval !== undefined)) throw new Error('Retrieval availability was substituted.');
	const items = [...memory.items.map(parseMemoryItem), ...retrievalItems]
		.sort((left, right) => right.stateChangedAtMs - left.stateChangedAtMs || left.proposalDigest.localeCompare(right.proposalDigest));
	return { memoryGeneration, retrievalGeneration, retrievalAvailable: root.retrieval_available, items };
}

export async function fetchAppContributionState(signal?: AbortSignal): Promise<AppContributionStateSnapshot> {
	const response = await fetch('/api/magician/v2/apps/memory-contributions/state?limit=32', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }), cache: 'no-store', redirect: 'error', signal
	});
	const body = await boundedJson(response);
	if (!response.ok) {
		const value = object(body);
		throw new Error(typeof value.message === 'string' ? value.message : 'Contribution state is unavailable.');
	}
	return parseAppContributionState(body);
}
