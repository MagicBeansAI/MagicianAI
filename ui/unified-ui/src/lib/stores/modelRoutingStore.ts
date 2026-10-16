/**
 * Model-routing store — per-operation profile choice (2026-08-31) and the
 * per-operation parent-engine rule (2026-09-14).
 *
 * The panel exposes config mappings, the engines driving flows now, explicit
 * one-operation-at-a-time overrides, and a Parent/Pinned choice per
 * operation. Reverting either is a DELETE.
 */
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export interface RoutingProfile {
	name: string;
	provider: string;
	model: string;
	class: 'local' | 'api' | 'harness';
	installed: boolean;
	/** false for adaptive composites: listed for display, not settable. */
	selectable?: boolean;
}

export interface RoutingOperation {
	operation: string;
	group: string;
	description: string;
	configured_selector:
		| string
		| {
				default: string;
				when_has_images?: string;
				when_cloud?: string;
				description?: string;
				group?: string;
				engine?: EngineFollow;
		  };
	default_profile: string;
	configured_profile: string;
	effective_profile: string;
	/** `parent`: inside a flow the operation would ride the flow's engine. */
	routing_source: 'config' | 'parent' | 'override';
	overridden: boolean;
	stale_override?: boolean;
	/** The effective Parent/Pinned setting: the owner's pin, else config. */
	engine: EngineFollow;
	engine_source: 'config' | 'override';
	/** `engine` is `parent` and the operation's default is not local. */
	follows_parent: boolean;
	/** A local (Ollama) default never follows, whatever `engine` says. */
	local_floor?: boolean;
	/** The profile the operation would ride under each driving engine. */
	parent_profiles: DrivingEngineProfiles;
}

/** Whether an operation follows the engine that started its flow. */
export type EngineFollow = 'parent' | 'pinned';

/** The engines that start flows now; `null` is the native loop. */
export interface DrivingEngines {
	chat: string | null;
	run: string | null;
}

export interface DrivingEngineProfiles {
	chat: string | null;
	run: string | null;
}

export interface RoutingOverview {
	affinity?: string | null;
	affinity_profile?: string | null;
	affinity_scope?: 'flow' | 'process' | null;
	driving_engines?: DrivingEngines;
	locality?: 'local' | 'cloud';
	operations: RoutingOperation[];
	profiles: RoutingProfile[];
	overrides: Record<string, string>;
	engine_pins?: Record<string, EngineFollow>;
	rule: string;
}

export async function fetchRoutingOverview(): Promise<RoutingOverview> {
	const response = await fetch('/api/magician/v2/llm/routing', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	if (!response.ok) throw new Error(`Routing overview failed (${response.status})`);
	const overview = (await response.json()) as RoutingOverview;
	// Keep the UI usable while a freshly built frontend is briefly paired with
	// an older backend during a rolling desktop/service restart. New servers
	// supply authoritative descriptions and conditional selectors; these
	// fallbacks are display-only and never affect a routing mutation.
	return {
		...overview,
		driving_engines: overview.driving_engines ?? { chat: null, run: null },
		operations: (overview.operations ?? []).map((operation) => {
			// An older backend reported the process-wide engine as
			// `via_affinity`; the closest current reading is a parent.
			const legacy = operation as RoutingOperation & { via_affinity?: boolean };
			return {
				...operation,
				group: operation.group ?? 'Other',
				description:
					operation.description ??
					`Runs the ${operation.operation.replaceAll('_', ' ')} LLM operation.`,
				configured_selector: operation.configured_selector ?? operation.default_profile,
				configured_profile: operation.configured_profile ?? operation.default_profile,
				routing_source:
					operation.routing_source ??
					(operation.overridden ? 'override' : legacy.via_affinity ? 'parent' : 'config'),
				engine: operation.engine ?? 'parent',
				engine_source: operation.engine_source ?? 'config',
				follows_parent: operation.follows_parent ?? false,
				parent_profiles: operation.parent_profiles ?? { chat: null, run: null }
			};
		})
	};
}

export async function setOperationProfile(operation: string, profile: string): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/llm/routing/${encodeURIComponent(operation)}`,
		{
			method: 'PUT',
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify({ profile })
		}
	);
	if (!response.ok) {
		const body = (await response.json().catch(() => ({}))) as { message?: string };
		throw new Error(body.message ?? `Switch failed (${response.status})`);
	}
}

export async function clearOperationProfile(operation: string): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/llm/routing/${encodeURIComponent(operation)}`,
		{
			method: 'DELETE',
			headers: scopedRequestHeaders({})
		}
	);
	if (!response.ok && response.status !== 404) {
		throw new Error(`Revert failed (${response.status})`);
	}
}

export async function setOperationEngine(operation: string, engine: EngineFollow): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/llm/routing/${encodeURIComponent(operation)}/engine`,
		{
			method: 'PUT',
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify({ engine })
		}
	);
	if (!response.ok) {
		const body = (await response.json().catch(() => ({}))) as { message?: string };
		throw new Error(body.message ?? `Engine pin failed (${response.status})`);
	}
}

export async function clearOperationEngine(operation: string): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/llm/routing/${encodeURIComponent(operation)}/engine`,
		{
			method: 'DELETE',
			headers: scopedRequestHeaders({})
		}
	);
	if (!response.ok && response.status !== 404) {
		throw new Error(`Engine unpin failed (${response.status})`);
	}
}
