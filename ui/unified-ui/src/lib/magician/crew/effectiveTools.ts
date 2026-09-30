import { timedFetch } from '$lib/shared/fetch';

export type EffectiveToolSurface =
	| 'chat'
	| 'realtime_voice'
	| 'tutor'
	| 'app_copilot'
	| 'thinking_map';

export interface EffectiveToolSurfaceOption {
	surface: EffectiveToolSurface;
	feature_mode: 'none' | 'tutor' | 'app_copilot' | 'brainstorm' | 'vibedev';
	label: string;
}

export interface EffectiveToolGrant {
	name: string;
	description?: string;
	provider_visible: boolean;
	dispatchable: boolean;
	requires_approval: boolean;
}

export interface EffectiveStructuralToolGrant extends EffectiveToolGrant {
	allowed_targets: string[];
}

export interface EffectiveInternalToolGrant {
	name: string;
	dispatchable: boolean;
}

export interface EffectiveToolPolicyPreview {
	schema_version: 'effective_tool_policy_preview.v1';
	generated_at: string;
	snapshot_id: string;
	agent_id: string;
	definition_version: number;
	definition_digest: string;
	trust_level: string;
	selected_surface: EffectiveToolSurface;
	feature_mode: EffectiveToolSurfaceOption['feature_mode'];
	source_kind: string;
	available_surfaces: EffectiveToolSurfaceOption[];
	direct: EffectiveToolGrant[];
	runtime: EffectiveToolGrant[];
	structural: EffectiveStructuralToolGrant[];
	deferred: EffectiveToolGrant[];
	internal: EffectiveInternalToolGrant[];
	delegation_targets: string[];
	handover_targets: string[];
	denied_tool_names: string[];
	approval_rule_count: number;
	provider_tool_count: number;
	dispatch_tool_count: number;
}

export interface RuntimeContextCachePreview {
	schema_version: 'runtime_context_cache_preview.v1';
	generated_at: string;
	agent_id: string;
	definition_version: number;
	scope: { principal: string; workspace: string };
	registry_revision: string;
	built_at_ms: number;
	pack_count: number;
	tool_index_count: number;
	cache: {
		entry_count: number;
		hits: number;
		misses: number;
		builds: number;
		invalidations: number;
		revision_failures: number;
		coalesced_waiters: number;
	};
	surface_plan_status: 'active';
	surface_plan_cache: {
		entry_count: number;
		hits: number;
		misses: number;
		inserts: number;
		evictions: number;
		invalidations: number;
		static_prompt_entry_count: number;
		static_prompt_hits: number;
		static_prompt_misses: number;
		static_prompt_inserts: number;
		static_prompt_evictions: number;
	};
	working_set_store: {
		entry_count: number;
		reads: number;
		creates: number;
		mutations: number;
		removals: number;
		evictions: number;
	};
}

interface EffectiveToolsApiError {
	code?: string;
	error?: string;
	message?: string;
	details?: {
		available_surfaces?: EffectiveToolSurfaceOption[];
		reason?: string;
	};
}

export function effectiveToolCount(preview: EffectiveToolPolicyPreview): number {
	return (
		preview.direct.length
		+ preview.runtime.length
		+ preview.structural.length
		+ preview.deferred.length
		+ preview.internal.length
	);
}

export function shortPolicyId(snapshotId: string): string {
	const normalized = snapshotId.trim();
	return normalized.length > 12 ? normalized.slice(0, 12) : normalized;
}

export async function fetchEffectiveToolPolicy(
	agentId: string,
	surface?: EffectiveToolSurface
): Promise<EffectiveToolPolicyPreview> {
	const normalized = agentId.trim();
	if (!normalized) throw new Error('Crew member id is required');
	const query = surface ? `?surface=${encodeURIComponent(surface)}` : '';
	const response = await timedFetch(
		`/api/magician/v2/agents/${encodeURIComponent(normalized)}/effective-tools${query}`
	);
	if (!response.ok) {
		let payload: EffectiveToolsApiError | null = null;
		try {
			payload = (await response.json()) as EffectiveToolsApiError;
		} catch {
			// Preserve the status fallback when an intermediary returns non-JSON.
		}
		const message = payload?.message || payload?.error || payload?.details?.reason;
		throw new Error(message || `Effective tools request failed (${response.status})`);
	}
	return (await response.json()) as EffectiveToolPolicyPreview;
}

async function runtimeContextCacheRequest(
	agentId: string,
	refresh: boolean
): Promise<RuntimeContextCachePreview> {
	const normalized = agentId.trim();
	if (!normalized) throw new Error('Crew member id is required');
	const suffix = refresh ? '/refresh' : '';
	const response = await timedFetch(
		`/api/magician/v2/agents/${encodeURIComponent(normalized)}/runtime-context-cache${suffix}`,
		refresh ? { method: 'POST' } : undefined
	);
	if (!response.ok) {
		let payload: EffectiveToolsApiError | null = null;
		try {
			payload = (await response.json()) as EffectiveToolsApiError;
		} catch {
			// Preserve the status fallback when an intermediary returns non-JSON.
		}
		const message = payload?.message || payload?.error || payload?.details?.reason;
		throw new Error(message || `Runtime context cache request failed (${response.status})`);
	}
	return (await response.json()) as RuntimeContextCachePreview;
}

export function fetchRuntimeContextCache(agentId: string): Promise<RuntimeContextCachePreview> {
	return runtimeContextCacheRequest(agentId, false);
}

export function refreshRuntimeContextCache(agentId: string): Promise<RuntimeContextCachePreview> {
	return runtimeContextCacheRequest(agentId, true);
}
