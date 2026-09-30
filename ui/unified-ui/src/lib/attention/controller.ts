import { goto } from '$app/navigation';
import { get, writable, type Readable } from 'svelte/store';

import { postHitlResponse } from '$lib/hitl/adapters';
import {
	respondToHitl,
	type RespondToHitlOptions
} from '$lib/hitl/respondToHitl';
import type {
	HitlDiffApprovalFile,
	HitlInputSchema,
	HitlInputType,
	HitlRequest,
	HitlResolveOutcome,
	HitlResponseValue
} from '$lib/hitl/types';
import { timedFetch } from '$lib/shared/fetch';
import { attentionStore } from '$lib/stores/attentionStore';
import { dropPendingHitl } from '$lib/stores/pendingHitlStore';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import {
	attentionRowAliases,
	type AttentionDisplayRow,
	type SkillEvolutionGateAction,
	type SkillEvolutionRollbackDecision
} from './model';

export interface AttentionScopeSnapshot {
	principal: string;
	workspace: string;
	key: string;
}

export interface AttentionActivationResult {
	status: 'resolved' | 'cancelled' | 'dismissed' | 'opened' | 'stale' | 'noop' | 'error';
	error?: string;
	request?: HitlRequest;
}

export interface AttentionControllerDependencies {
	getScope?: () => AttentionScopeSnapshot;
	respond?: (
		request: HitlRequest,
		headers?: HeadersInit,
		options?: RespondToHitlOptions
	) => Promise<HitlResolveOutcome>;
	postResponse?: (
		request: HitlRequest,
		value: HitlResponseValue,
		headers?: HeadersInit
	) => Promise<HitlResolveOutcome>;
	hydrateRequest?: (
		row: AttentionDisplayRow,
		scope: AttentionScopeSnapshot
	) => Promise<HitlRequest | null>;
	dropPending?: (id: string) => void;
	dropAttention?: (id: string) => void;
	dismissFailed?: (id: string, feedItemId?: string) => void;
	navigate?: (href: string) => void | Promise<void>;
	refreshAttention?: () => Promise<void>;
	fetch?: typeof timedFetch;
	now?: () => Date;
	onHydrationChange?: (key: string, active: boolean) => void;
}

export interface AttentionItemControllerState {
	hydratingKey: string | null;
	skillEvolutionActionKey: string | null;
	rollbackActionKey: string | null;
	approvingAllDiffs: boolean;
	error: string | null;
	notice: string | null;
}

export interface AttentionItemController {
	state: Readable<AttentionItemControllerState>;
	activate(row: AttentionDisplayRow): Promise<AttentionActivationResult>;
	approveAllDiffApprovals(rows: AttentionDisplayRow[]): Promise<void>;
	runSkillEvolutionGate(
		row: AttentionDisplayRow,
		action?: SkillEvolutionGateAction
	): Promise<void>;
	runRollbackRecommendationDecision(
		row: AttentionDisplayRow,
		decision: SkillEvolutionRollbackDecision
	): Promise<void>;
	clearFeedback(): void;
}

function currentScope(): AttentionScopeSnapshot {
	const scope = get(scopeIdentityStore);
	return {
		principal: scope.principal,
		workspace: scope.workspace,
		key: `${scope.principal}:${scope.workspace}`
	};
}

export function attentionScopeHeaders(scope: AttentionScopeSnapshot): Record<string, string> {
	const headers: Record<string, string> = {};
	return headers;
}

function scopeIsCurrent(
	scope: AttentionScopeSnapshot,
	dependencies: AttentionControllerDependencies
): boolean {
	return (dependencies.getScope ?? currentScope)().key === scope.key;
}

function errorFromOutcome(outcome: HitlResolveOutcome): string | null {
	if (outcome.ok) return null;
	if ('cancelled' in outcome) return null;
	return outcome.message || `HTTP ${outcome.status}`;
}

function dropResolvedAliases(
	row: AttentionDisplayRow,
	request: HitlRequest,
	dependencies: AttentionControllerDependencies
): void {
	const dropPending = dependencies.dropPending ?? dropPendingHitl;
	const dropAttention = dependencies.dropAttention ?? attentionStore.dropResolved;
	for (const alias of attentionRowAliases(row, request)) {
		dropPending(alias);
		dropAttention(alias);
	}
}

function reaskedRequest(request: HitlRequest, outcome: HitlResolveOutcome): HitlRequest {
	if (outcome.ok || !('reask' in outcome)) return request;
	return {
		...request,
		prompt: outcome.question ?? request.prompt,
		hint: outcome.hint ?? request.hint
	};
}

function reaskPromptOptions(
	request: HitlRequest,
	outcome: HitlResolveOutcome
): RespondToHitlOptions | undefined {
	if (outcome.ok || !('reask' in outcome) || outcome.previousAnswer === undefined) {
		return undefined;
	}
	switch (request.input_type) {
		case 'text':
		case 'guidance':
		case 'file_path':
		case 'external_action':
			return { defaultValue: outcome.previousAnswer };
		default:
			return undefined;
	}
}

/**
 * Activate any canonical Attention row directly. HITL rows use the singleton
 * prompt service through respondToHitl; terminal and review rows use their
 * native actions.
 */
export async function activateAttentionRow(
	row: AttentionDisplayRow,
	dependencies: AttentionControllerDependencies = {}
): Promise<AttentionActivationResult> {
	const scope = (dependencies.getScope ?? currentScope)();
	if (row.failed) {
		(dependencies.dismissFailed ?? attentionStore.dismissFailed)(row.key, row.feed_item_id);
		return { status: 'dismissed' };
	}
	if (row.review_href) {
		await (dependencies.navigate ?? goto)(row.review_href);
		return { status: 'opened' };
	}

	let request = row.request;
	if (!request) {
		if (!row.execution_id) {
			return { status: 'error', error: 'No execution_id on this attention row.' };
		}
		dependencies.onHydrationChange?.(row.key, true);
		try {
			request = await (dependencies.hydrateRequest ?? hydrateAttentionRequest)(row, scope, dependencies);
		} catch (error) {
			if (!scopeIsCurrent(scope, dependencies)) return { status: 'stale' };
			return {
				status: 'error',
				error: error instanceof Error ? error.message : 'Failed to load pause state.'
			};
		} finally {
			dependencies.onHydrationChange?.(row.key, false);
		}
		if (!scopeIsCurrent(scope, dependencies)) return { status: 'stale' };
		if (!request) {
			return { status: 'error', error: 'No active pause state for this execution.' };
		}
	}

	const respond = dependencies.respond ?? respondToHitl;
	let promptOptions: RespondToHitlOptions | undefined;
	while (scopeIsCurrent(scope, dependencies)) {
		const outcome = await respond(request, attentionScopeHeaders(scope), promptOptions);
		if (!scopeIsCurrent(scope, dependencies)) return { status: 'stale', request };
		if (outcome.ok) {
			dropResolvedAliases(row, request, dependencies);
			return { status: 'resolved', request };
		}
		if ('cancelled' in outcome) return { status: 'cancelled', request };
		if ('reask' in outcome && outcome.reask) {
			promptOptions = reaskPromptOptions(request, outcome);
			request = reaskedRequest(request, outcome);
			continue;
		}
		return { status: 'error', error: errorFromOutcome(outcome) ?? 'Response failed.', request };
	}
	return { status: 'stale', request };
}

/** Hydrate a thin feed row from the persisted execution pause state. */
export async function hydrateAttentionRequest(
	row: AttentionDisplayRow,
	scope: AttentionScopeSnapshot,
	dependencies: Pick<AttentionControllerDependencies, 'fetch'> = {}
): Promise<HitlRequest | null> {
	if (!row.execution_id) return null;
	const params = new URLSearchParams();
	const url = `/api/magician/v2/executions/${encodeURIComponent(row.execution_id)}/pause-state${
		params.size > 0 ? `?${params.toString()}` : ''
	}`;
	const response = await (dependencies.fetch ?? timedFetch)(url, {
		headers: attentionScopeHeaders(scope)
	});
	if (!response.ok) throw new Error(`HTTP ${response.status}`);
	const payload = (await response.json()) as Record<string, unknown>;
	const active = (payload.active_pause_state as Record<string, unknown> | null) ?? null;
	if (!active) return null;
	const pauseState = (active.pause_state as Record<string, unknown> | null) ?? null;
	const inputTypeObject = (active.input_type as Record<string, unknown> | null) ?? null;
	const inputType =
		typeof inputTypeObject?.type === 'string'
			? (inputTypeObject.type as HitlInputType)
			: null;
	if (!inputType) return null;
	const pauseStateId =
		typeof active.pause_state_id === 'string'
			? active.pause_state_id
			: typeof pauseState?.storage_key === 'string'
				? pauseState.storage_key
				: row.pause_state_id ?? '';
	const schema: HitlInputSchema = {};
	if (inputType === 'diff_approval' && inputTypeObject) {
		if (typeof inputTypeObject.transaction_id === 'string') {
			schema.transaction_id = inputTypeObject.transaction_id;
		}
		if (typeof inputTypeObject.proposal_id === 'string') {
			schema.proposal_id = inputTypeObject.proposal_id;
		}
		if (typeof inputTypeObject.approval_source === 'string') {
			schema.approval_source = inputTypeObject.approval_source;
		}
		if (typeof inputTypeObject.rationale === 'string') {
			schema.rationale = inputTypeObject.rationale;
		}
		if (Array.isArray(inputTypeObject.files)) {
			schema.files = inputTypeObject.files
				.map((entry) => {
					if (!entry || typeof entry !== 'object') return null;
					const file = entry as Record<string, unknown>;
					if (typeof file.path !== 'string') return null;
					return {
						path: file.path,
						status: typeof file.status === 'string' ? file.status : 'M',
						additions: typeof file.additions === 'number' ? file.additions : 0,
						deletions: typeof file.deletions === 'number' ? file.deletions : 0,
						unified_diff: typeof file.unified_diff === 'string' ? file.unified_diff : ''
					};
				})
				.filter((entry): entry is HitlDiffApprovalFile => entry !== null);
		}
	}
	return {
		id: pauseStateId,
		source: row.source,
		input_type: inputType,
		schema,
		prompt: typeof active.question === 'string' ? active.question : row.prompt,
		hint: typeof active.hint === 'string' ? active.hint : row.hint,
		scope: row.scope,
		identifiers: {
			pause_state_id: pauseStateId,
			correlation_id:
				schema.proposal_id ?? schema.transaction_id ?? row.correlation_id
		},
		at: row.at,
		raw: active
	};
}

async function readApiError(response: Response): Promise<string> {
	const text = await response.text().catch(() => '');
	if (!text) return `HTTP ${response.status}`;
	try {
		const parsed = JSON.parse(text) as { error?: string; message?: string };
		return parsed.error || parsed.message || text;
	} catch {
		return text;
	}
}

export function createAttentionItemController(
	dependencies: AttentionControllerDependencies = {}
): AttentionItemController {
	const initialState: AttentionItemControllerState = {
		hydratingKey: null,
		skillEvolutionActionKey: null,
		rollbackActionKey: null,
		approvingAllDiffs: false,
		error: null,
		notice: null
	};
	const store = writable(initialState);
	const getScope = dependencies.getScope ?? currentScope;
	const now = dependencies.now ?? (() => new Date());
	const controllerDependencies: AttentionControllerDependencies = {
		...dependencies,
		getScope,
		onHydrationChange: (key, active) => {
			dependencies.onHydrationChange?.(key, active);
			store.update((state) => ({ ...state, hydratingKey: active ? key : null }));
		}
	};

	function clearFeedback(): void {
		store.update((state) => ({ ...state, error: null, notice: null }));
	}

	async function activate(row: AttentionDisplayRow): Promise<AttentionActivationResult> {
		clearFeedback();
		const result = await activateAttentionRow(row, controllerDependencies);
		if (result.status === 'error') {
			store.update((state) => ({ ...state, error: result.error ?? 'Attention action failed.' }));
		}
		return result;
	}

	async function postSkillEvolutionJson(
		path: string,
		body: Record<string, unknown>,
		scope: AttentionScopeSnapshot
	): Promise<void> {
		const response = await (dependencies.fetch ?? timedFetch)(path, {
			method: 'POST',
			headers: {
				...attentionScopeHeaders(scope),
				'Content-Type': 'application/json'
			},
			body: JSON.stringify(body)
		});
		if (!response.ok) throw new Error(await readApiError(response));
	}

	async function approveAllDiffApprovals(rows: AttentionDisplayRow[]): Promise<void> {
		const diffRows = rows.filter(
			(row) => !row.failed && row.request?.input_type === 'diff_approval'
		);
		if (get(store).approvingAllDiffs || diffRows.length === 0) return;
		const scope = getScope();
		store.update((state) => ({
			...state,
			approvingAllDiffs: true,
			error: null,
			notice: null
		}));
		let applied = 0;
		let failed = 0;
		try {
			for (const row of diffRows) {
				if (!scopeIsCurrent(scope, controllerDependencies) || !row.request) return;
				const outcome = await (dependencies.postResponse ?? postHitlResponse)(
					row.request,
					{ type: 'choice', selected_id: 'apply' },
					attentionScopeHeaders(scope)
				);
				if (!scopeIsCurrent(scope, controllerDependencies)) return;
				if (outcome.ok) {
					applied += 1;
					dropResolvedAliases(row, row.request, controllerDependencies);
				} else {
					failed += 1;
				}
			}
			if (!scopeIsCurrent(scope, controllerDependencies)) return;
			store.update((state) => ({
				...state,
				notice:
					failed === 0
						? `Applied ${applied} code change set${applied === 1 ? '' : 's'}.`
						: `Applied ${applied}; ${failed} failed.`
			}));
		} finally {
			store.update((state) => ({ ...state, approvingAllDiffs: false }));
		}
	}

	async function runSkillEvolutionGate(
		row: AttentionDisplayRow,
		action: SkillEvolutionGateAction = row.skillEvolution?.action ?? 'approve'
	): Promise<void> {
		const details = row.skillEvolution;
		if (!details || get(store).skillEvolutionActionKey !== null) return;
		const scope = getScope();
		const actionKey = `${row.key}:${action}`;
		store.update((state) => ({
			...state,
			skillEvolutionActionKey: actionKey,
			error: null,
			notice: null
		}));
		try {
			let notice: string;
			if (action === 'approve' || action === 'reject') {
				if (details.gate !== 'proposal_review') {
					throw new Error('This Skill Evolution row is not a proposal review gate.');
				}
				await postSkillEvolutionJson(
					`/api/magician/v2/learning/skill-evolution/proposals/${encodeURIComponent(details.candidateId)}/decision`,
					{
						status: action === 'approve' ? 'approved' : 'rejected',
						actor: 'operator',
						reason:
							action === 'approve'
								? 'Approved from the Attention Skill Evolution gate.'
								: 'Rejected from the Attention Skill Evolution gate.',
						payload: {
							source: 'skill_evolution_attention',
							attention_row_id: row.key,
							gate: details.gate,
							decided_at: now().toISOString()
						}
					},
					scope
				);
				notice = `${action === 'approve' ? 'Approved' : 'Rejected'} ${details.candidateId}.`;
			} else if (action === 'dry_run' || action === 'apply') {
				if (details.gate !== 'apply_gate') {
					throw new Error('This Skill Evolution row is not an apply gate.');
				}
				if (!details.implementationId) throw new Error('This apply gate is missing implementation_id.');
				if (!details.targetSurface) {
					throw new Error('This apply gate targets mixed or unsupported skill surfaces; review it in the dashboard.');
				}
				await postSkillEvolutionJson(
					`/api/magician/v2/learning/skill-evolution/implementations/${encodeURIComponent(details.candidateId)}/${encodeURIComponent(details.implementationId)}/apply`,
					{
						actor: 'operator',
						apply: action === 'apply',
						target_surface: details.targetSurface,
						summary:
							action === 'apply'
								? `Applied reviewed implementation to ${details.targetSurface} from Attention.`
								: `Prepared dry-run for reviewed implementation against ${details.targetSurface} from Attention.`,
						payload: {
							source: 'skill_evolution_attention',
							attention_row_id: row.key,
							gate: details.gate,
							proposal_id: details.proposalId,
							validation_id: details.validationId,
							implementation_id: details.implementationId,
							application_id: details.applicationId,
							requested_at: now().toISOString()
						}
					},
					scope
				);
				notice =
					action === 'apply'
						? `Applied ${details.candidateId}.`
						: `Prepared dry-run for ${details.candidateId}.`;
			} else {
				if (details.gate !== 'promotion_gate') {
					throw new Error('This Skill Evolution row is not a promotion gate.');
				}
				if (!details.validationId) throw new Error('This promotion gate is missing validation_id.');
				await postSkillEvolutionJson(
					`/api/magician/v2/learning/skill-evolution/proposals/${encodeURIComponent(details.candidateId)}/promotion`,
					{
						actor: 'operator',
						summary: 'Promoted reviewed Skill Evolution change from Attention.',
						validation_id: details.validationId,
						implementation_id: details.implementationId,
						application_id: details.applicationId,
						payload: {
							source: 'skill_evolution_attention',
							attention_row_id: row.key,
							gate: details.gate,
							proposal_id: details.proposalId,
							promoted_at: now().toISOString()
						}
					},
					scope
				);
				notice = `Promoted ${details.candidateId}.`;
			}
			if (!scopeIsCurrent(scope, controllerDependencies)) return;
			store.update((state) => ({ ...state, notice }));
			await (dependencies.refreshAttention ?? attentionStore.refresh)();
		} catch (error) {
			if (!scopeIsCurrent(scope, controllerDependencies)) return;
			store.update((state) => ({
				...state,
				error: error instanceof Error ? error.message : 'Skill Evolution action failed.'
			}));
		} finally {
			store.update((state) => ({ ...state, skillEvolutionActionKey: null }));
		}
	}

	async function runRollbackRecommendationDecision(
		row: AttentionDisplayRow,
		decision: SkillEvolutionRollbackDecision
	): Promise<void> {
		const details = row.rollbackRecommendation;
		if (!details || get(store).rollbackActionKey !== null) return;
		const scope = getScope();
		store.update((state) => ({
			...state,
			rollbackActionKey: `${row.key}:${decision}`,
			error: null,
			notice: null
		}));
		try {
			await postSkillEvolutionJson(
				`/api/magician/v2/learning/skill-evolution/rollback-recommendations/${encodeURIComponent(details.candidateId)}/${encodeURIComponent(details.recommendationId)}/decision`,
				{
					status: decision,
					actor: 'operator',
					summary:
						decision === 'dismissed'
							? 'Operator dismissed the rollback recommendation from Attention after review.'
							: 'Operator superseded the rollback recommendation from Attention after choosing a newer path.',
					payload: {
						source: 'skill_evolution_attention',
						attention_row_id: row.key,
						recommendation_id: details.recommendationId,
						application_id: details.applicationId,
						decided_at: now().toISOString()
					}
				},
				scope
			);
			if (!scopeIsCurrent(scope, controllerDependencies)) return;
			store.update((state) => ({
				...state,
				notice:
					decision === 'dismissed'
						? `Dismissed rollback ${details.recommendationId}.`
						: `Superseded rollback ${details.recommendationId}.`
			}));
			await (dependencies.refreshAttention ?? attentionStore.refresh)();
		} catch (error) {
			if (!scopeIsCurrent(scope, controllerDependencies)) return;
			store.update((state) => ({
				...state,
				error: error instanceof Error ? error.message : 'Rollback decision failed.'
			}));
		} finally {
			store.update((state) => ({ ...state, rollbackActionKey: null }));
		}
	}

	return {
		state: { subscribe: store.subscribe },
		activate,
		approveAllDiffApprovals,
		runSkillEvolutionGate,
		runRollbackRecommendationDecision,
		clearFeedback
	};
}
