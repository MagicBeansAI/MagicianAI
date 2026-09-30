import { get } from 'svelte/store';

import {
	hitlRequestFromCanonicalEvent,
	hitlRequestFromFeedItem,
	isHitlSourceInputCompatible,
	normalizeHitlSource
} from '$lib/hitl/adapters';
import {
	respondToHitl,
	type RespondToHitlOptions
} from '$lib/hitl/respondToHitl';
import type {
	HitlIdentifiers,
	HitlOpenTarget,
	HitlRequest,
	HitlResolveOutcome
} from '$lib/hitl/types';
import type { FeedItem } from '$lib/feed/types';
import { attentionStore } from '$lib/stores/attentionStore';
import {
	dropPendingHitl,
	type HitlPendingEntry
} from '$lib/stores/pendingHitlStore';
import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type { HitlOpenTarget } from '$lib/hitl/types';

export interface HitlOpenScopeSnapshot {
	principal: string;
	workspace: string;
	key: string;
}

export type HitlPromptOpenResult =
	| { status: 'resolved'; request: HitlRequest }
	| { status: 'cancelled'; request: HitlRequest }
	| { status: 'stale'; request: HitlRequest }
	| { status: 'error'; request?: HitlRequest; error: string };

export interface OpenHitlPromptDependencies {
	getScope?: () => HitlOpenScopeSnapshot;
	respond?: (
		request: HitlRequest,
		headers?: HeadersInit,
		options?: RespondToHitlOptions
	) => Promise<HitlResolveOutcome>;
	dropPending?: (id: string) => void;
	dropAttention?: (id: string) => void;
}

function currentScope(): HitlOpenScopeSnapshot {
	const scope = get(scopeIdentityStore);
	return {
		principal: scope.principal,
		workspace: scope.workspace,
		key: `${scope.principal}:${scope.workspace}`
	};
}

function scopeHeaders(scope: HitlOpenScopeSnapshot): Headers {
	void scope;
	return scopedRequestHeaders();
}

function normalizedIdentifier(value: string | null | undefined): string | undefined {
	const normalized = value?.trim();
	return normalized || undefined;
}

function targetId(target: {
	id?: string | null;
	identifiers?: Partial<HitlIdentifiers> | null;
}): string | null {
	return (
		normalizedIdentifier(target.id) ??
		normalizedIdentifier(target.identifiers?.correlation_id) ??
		normalizedIdentifier(target.identifiers?.pause_state_id) ??
		normalizedIdentifier(target.identifiers?.approval_id) ??
		normalizedIdentifier(target.identifiers?.request_id) ??
		null
	);
}

function openTargetRecord(value: unknown): Record<string, unknown> | null {
	return value && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function openTargetSourceIdentifier(
	source: string,
	identifiers: Record<string, unknown>,
	inputSchema: Record<string, unknown>
): string | null {
	const identifier = (key: string) =>
		typeof identifiers[key] === 'string'
			? normalizedIdentifier(identifiers[key] as string) ?? null
			: null;
	const schemaIdentifier = (key: string) =>
		typeof inputSchema[key] === 'string'
			? normalizedIdentifier(inputSchema[key] as string) ?? null
			: null;
	switch (source) {
		case 'approval':
			return identifier('approval_id');
		case 'user_request':
			return identifier('request_id');
		case 'agentic':
		case 'escalation':
			return identifier('pause_state_id');
		case 'clarification':
		case 'plan_approval':
		case 'bot_auth':
		case 'service_health':
			return identifier('correlation_id');
		case 'diff_approval': {
			const proposalId = schemaIdentifier('proposal_id');
			const transactionId = schemaIdentifier('transaction_id');
			if (proposalId && transactionId && proposalId !== transactionId) return null;
			return proposalId ?? transactionId ?? identifier('correlation_id');
		}
		default:
			return null;
	}
}

function sourceIdentifier(request: HitlRequest): string | null {
	switch (request.source) {
		case 'approval':
			return normalizedIdentifier(request.identifiers.approval_id) ?? null;
		case 'user_request':
			return normalizedIdentifier(request.identifiers.request_id) ?? null;
		case 'agentic':
		case 'escalation':
			return normalizedIdentifier(request.identifiers.pause_state_id) ?? null;
		case 'clarification':
		case 'plan_approval':
		case 'bot_auth':
		case 'service_health':
			return normalizedIdentifier(request.identifiers.correlation_id) ?? null;
		case 'diff_approval': {
			const proposalId = normalizedIdentifier(request.schema.proposal_id);
			const transactionId = normalizedIdentifier(request.schema.transaction_id);
			if (proposalId && transactionId && proposalId !== transactionId) return null;
			return proposalId ?? transactionId ?? normalizedIdentifier(request.identifiers.correlation_id) ?? null;
		}
	}
}

function requestHasCompleteTargetContract(request: HitlRequest): boolean {
	const canonicalId = sourceIdentifier(request);
	if (!canonicalId || canonicalId !== normalizedIdentifier(request.id)) return false;
	if (!request.prompt.trim()) return false;
	if (!isHitlSourceInputCompatible(request.source, request.input_type, request.schema)) return false;
	if (!normalizedIdentifier(request.scope.principal) || !normalizedIdentifier(request.scope.workspace)) {
		return false;
	}
	const correlationId = normalizedIdentifier(request.identifiers.correlation_id);
	if (correlationId && correlationId !== canonicalId) return false;
	const pauseStateId = normalizedIdentifier(request.identifiers.pause_state_id);
	const approvalId = normalizedIdentifier(request.identifiers.approval_id);
	const requestId = normalizedIdentifier(request.identifiers.request_id);
	if (approvalId && (request.source !== 'approval' || approvalId !== canonicalId)) return false;
	if (requestId && (request.source !== 'user_request' || requestId !== canonicalId)) return false;
	if (pauseStateId) {
		if (request.source === 'agentic' || request.source === 'escalation') {
			if (pauseStateId !== canonicalId) return false;
		} else if (request.source === 'user_request') {
			if (pauseStateId !== canonicalId) return false;
		} else if (request.source !== 'diff_approval') {
			return false;
		}
	}
	if (
		(request.source === 'agentic' ||
			request.source === 'escalation' ||
			request.source === 'diff_approval') &&
		!normalizedIdentifier(request.scope.execution_id)
	) {
		return false;
	}
	if (
		(request.source === 'clarification' || request.source === 'plan_approval') &&
		!normalizedIdentifier(request.scope.workflow_id ?? request.scope.task_id)
	) {
		return false;
	}
	if (request.source === 'diff_approval' && request.input_type !== 'diff_approval') return false;
	if (request.input_type === 'diff_approval' && request.source !== 'diff_approval') return false;
	if (request.source === 'bot_auth') {
		const parts = canonicalId.split(':');
		if (
			parts.length !== 4 ||
			parts[0] !== 'bot_auth' ||
			parts[1] !== request.scope.principal ||
			parts[2] !== request.scope.workspace ||
			!parts[3]?.trim()
		) {
			return false;
		}
	}
	return true;
}

function hitlRequestFromOpenTargetValue(value: unknown): HitlRequest | null {
	const target = openTargetRecord(value);
	if (!target) return null;
	for (const field of ['input_schema', 'identifiers', 'scope'] as const) {
		const fieldValue = target[field];
		if (fieldValue !== undefined && fieldValue !== null && !openTargetRecord(fieldValue)) {
			return null;
		}
	}
	const identifiers = openTargetRecord(target.identifiers) ?? {};
	const inputSchema = openTargetRecord(target.input_schema) ?? {};
	const id = targetId({
		id: typeof target.id === 'string' ? target.id : '',
		identifiers: {
			correlation_id:
				typeof identifiers.correlation_id === 'string'
					? identifiers.correlation_id
					: undefined,
			pause_state_id:
				typeof identifiers.pause_state_id === 'string'
					? identifiers.pause_state_id
					: undefined,
			approval_id:
				typeof identifiers.approval_id === 'string'
					? identifiers.approval_id
					: undefined,
			request_id:
				typeof identifiers.request_id === 'string'
					? identifiers.request_id
					: undefined
		}
	});
	if (!id) return null;
	if (
		typeof target.source !== 'string' ||
		typeof target.input_type !== 'string' ||
		typeof target.prompt !== 'string'
	) {
		return null;
	}
	const source = normalizeHitlSource(target.source);
	if (!source) return null;
	const rawSourceIdentifier = openTargetSourceIdentifier(
		source,
		identifiers,
		inputSchema
	);
	if (!rawSourceIdentifier || rawSourceIdentifier !== id) return null;
	const rawCorrelationId =
		typeof identifiers.correlation_id === 'string'
			? normalizedIdentifier(identifiers.correlation_id)
			: undefined;
	if (rawCorrelationId && rawCorrelationId !== rawSourceIdentifier) return null;
	const canonical = hitlRequestFromCanonicalEvent({
		event_type: 'HitlRequested',
		data: {
			...target,
			correlation_id: id,
			pause_state_id: identifiers.pause_state_id,
			approval_id: identifiers.approval_id,
			request_id: identifiers.request_id,
			timestamp_ms: typeof target.at === 'number' ? target.at : undefined
		}
	});
	if (!canonical) return null;
	const request: HitlRequest = {
		id,
		source: canonical.source,
		input_type: canonical.input_type,
		schema: canonical.schema,
		prompt: canonical.prompt,
		hint: canonical.hint,
		scope: canonical.scope,
		identifiers: {
			pause_state_id: normalizedIdentifier(canonical.identifiers.pause_state_id),
			approval_id: normalizedIdentifier(canonical.identifiers.approval_id),
			correlation_id:
				normalizedIdentifier(
					typeof identifiers.correlation_id === 'string'
						? identifiers.correlation_id
						: undefined
				) ?? id,
			request_id: normalizedIdentifier(canonical.identifiers.request_id)
		},
		at: typeof target.at === 'number' && Number.isFinite(target.at) ? target.at : undefined
	};
	return requestHasCompleteTargetContract(request) ? request : null;
}

export function hitlRequestFromOpenTarget(target: HitlOpenTarget): HitlRequest | null {
	return hitlRequestFromOpenTargetValue(target);
}

/** Parse an untrusted API payload into the direct-open wire contract. */
export function hitlOpenTargetFromUnknown(value: unknown): HitlOpenTarget | null {
	const request = hitlRequestFromOpenTargetValue(value);
	return request ? hitlOpenTargetFromRequest(request) : null;
}

export function hitlOpenTargetFromRequest(request: HitlRequest): HitlOpenTarget {
	return {
		id: request.id,
		source: request.source,
		input_type: request.input_type,
		prompt: request.prompt,
		hint: request.hint,
		input_schema: request.schema,
		identifiers: request.identifiers,
		scope: request.scope,
		at: request.at
	};
}

export function hitlOpenTargetFromFeedItem(item: FeedItem): HitlOpenTarget | null {
	const metadata =
		item.metadata && typeof item.metadata === 'object' && !Array.isArray(item.metadata)
			? (item.metadata as Record<string, unknown>)
			: null;
	const embedded = metadata?.hitl_request;
	const directTarget = hitlOpenTargetFromUnknown(embedded);
	if (directTarget) return directTarget;
	const request = hitlRequestFromFeedItem(item);
	if (request) return hitlOpenTargetFromRequest(request);
	if (item.item_type === 'approval') {
		const approvalId =
			typeof metadata?.approval_id === 'string' && metadata.approval_id.trim()
				? metadata.approval_id.trim()
				: item.id.trim();
		if (!approvalId) return null;
		return {
			id: approvalId,
			source: 'approval',
			input_type: 'confirmation',
			prompt: item.title,
			hint: item.summary,
			input_schema: { confirm_label: 'Approve', deny_label: 'Reject' },
			identifiers: {
				approval_id: approvalId,
				correlation_id: approvalId
			},
			scope: {
				principal: item.principal,
				workspace: item.workspace,
				task_id: item.task_id ?? undefined,
				agent_id: item.agent_id ?? undefined,
				thread_id: item.ui_thread_id ?? undefined
			},
			at: item.updated_at
		};
	}
	return null;
}

export function hitlOpenTargetFromPendingEntry(entry: HitlPendingEntry): HitlOpenTarget | null {
	if (!entry.raw) return null;
	const request = hitlRequestFromCanonicalEvent(entry.raw);
	return request ? hitlOpenTargetFromRequest(request) : null;
}

function authoritativeRequestAliases(request: HitlRequest): string[] {
	const canonicalId = sourceIdentifier(request);
	return canonicalId && canonicalId === request.id ? [canonicalId] : [];
}

function reaskedRequest(request: HitlRequest, outcome: HitlResolveOutcome): HitlRequest {
	if (outcome.ok || !('reask' in outcome)) return request;
	return {
		...request,
		prompt: outcome.question ?? request.prompt,
		hint: outcome.hint ?? request.hint
	};
}

function reaskOptions(
	request: HitlRequest,
	outcome: HitlResolveOutcome
): RespondToHitlOptions | undefined {
	if (outcome.ok || !('reask' in outcome) || outcome.previousAnswer === undefined) {
		return undefined;
	}
	if (
		request.input_type === 'text' ||
		request.input_type === 'guidance' ||
		request.input_type === 'file_path' ||
		request.input_type === 'external_action'
	) {
		return { defaultValue: outcome.previousAnswer };
	}
	return undefined;
}

/**
 * How many times one exchange may be sent back for revision before this stops
 * re-opening the prompt and reports the last reason instead.
 *
 * **A backstop, not the bound.** What ordinarily ends this loop is that every
 * iteration awaits `respond`, which opens the modal: it cannot advance without a
 * fresh human answer, and a dismissal ends it. The retired `ExecutionPanel`
 * carried the same number for the same stated reason — "so a misbehaving backend
 * can't spin forever" — and it was equally redundant there, right up until a
 * caller supplied a `respond` that did not prompt. This module now has one:
 * `answerTaskAsk` substitutes the answer the reader typed in the task panel for
 * the first modal. Deleting the line that stops it substituting the *same* answer
 * on every re-ask turns this into an unbounded POST loop, which is exactly the
 * shape the number was written against.
 *
 * Five, because it is the number the panel used and no evidence has ever
 * distinguished it from another; what matters is that it exists and is finite.
 */
const REASK_LIMIT = 5;

/** Open and resolve a prompt without consulting the Attention feed. */
export async function openHitlPrompt(
	target: HitlOpenTarget,
	dependencies: OpenHitlPromptDependencies = {}
): Promise<HitlPromptOpenResult> {
	let request = hitlRequestFromOpenTarget(target);
	if (!request) {
		return { status: 'error', error: 'HITL target is missing a resolvable identifier.' };
	}
	const getScope = dependencies.getScope ?? currentScope;
	const scope = getScope();
	if (
		request.scope.principal !== scope.principal ||
		request.scope.workspace !== scope.workspace
	) {
		return {
			status: 'error',
			request,
			error: `This request belongs to ${request.scope.principal}/${request.scope.workspace}; switch to that scope before responding.`
		};
	}
	const respond = dependencies.respond ?? respondToHitl;
	let options: RespondToHitlOptions | undefined;
	let reasks = 0;

	while (getScope().key === scope.key) {
		const outcome = await respond(request, scopeHeaders(scope), options);
		if (getScope().key !== scope.key) return { status: 'stale', request };
		if (outcome.ok) {
			const dropPending = dependencies.dropPending ?? dropPendingHitl;
			const dropAttention = dependencies.dropAttention ?? attentionStore.dropResolved;
			for (const alias of authoritativeRequestAliases(request)) {
				dropPending(alias);
				dropAttention(alias);
			}
			return { status: 'resolved', request };
		}
		if ('cancelled' in outcome) return { status: 'cancelled', request };
		if ('reask' in outcome && outcome.reask && reasks < REASK_LIMIT) {
			reasks += 1;
			options = reaskOptions(request, outcome);
			request = reaskedRequest(request, outcome);
			continue;
		}
		return {
			status: 'error',
			request,
			error: outcome.message || `HITL response failed (${outcome.status}).`
		};
	}

	return { status: 'stale', request };
}
