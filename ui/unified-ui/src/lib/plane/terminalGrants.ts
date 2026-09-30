// Terminal grants for the Magician plane (plane plan Task 10).
//
// A `plt_` grant is a scoped, expiring credential a terminal harness binds
// to Magician's MCP door. The token value exists exactly once — at mint —
// and the store keeps only its hash; the list payload never carries values.

import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export interface TerminalGrantRow {
	id: string;
	label: string;
	workspace: string;
	agent_identity: string;
	harness_engine: string;
	allowed_tools: string[];
	max_usd: number | null;
	max_wall_clock_secs: number | null;
	max_concurrent_runs: number;
	created_at: string;
	expires_at: string;
}

export interface MintedGrant {
	grant: TerminalGrantRow;
	/** Shown once, never retrievable again. */
	token: string;
	dropped_tools: string[];
}

/**
 * Engines the door can actually launch. Driven by the same roster the
 * delegated-run door validates against, so the dropdown can never offer a
 name that would silently fall back to Magician's own loop. `magician` is
 * the deliberate forced pin; further harnesses appear here as their
 * `HarnessEngine` implementations land.
 */
export const LAUNCHABLE_ENGINES: readonly string[] = [
	'pi',
	'claude_code',
	'codex',
	'codex_app_server',
	'grok',
	'agy',
	'magician'
];

export type NativeToolPosture = 'stripped' | 'denylisted' | 'sandboxed' | 'live';

export interface EngineAvailability {
	name: string;
	installed: boolean;
	/** How this spawn treats native (non-plane) tools. Magician is stripped. */
	native_tool_posture?: NativeToolPosture;
	/** Models this engine can be pinned to; `default` lets the CLI choose. */
	models?: string[];
}

export type DecisionMode = 'all_engines' | 'magician_only' | 'off';

function parseDecisionMode(value: unknown): DecisionMode | undefined {
	return value === 'all_engines' || value === 'magician_only' || value === 'off' ? value : undefined;
}

export async function setDecisionMode(mode: DecisionMode): Promise<DecisionMode> {
	const response = await fetch('/api/magician/v2/plane/decision-mode', {
		method: 'PUT',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json', Accept: 'application/json' }),
		body: JSON.stringify({ mode }),
		redirect: 'error'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw new Error(errorMessage(body) ?? 'The Decision Engine setting could not be saved.');
	const saved = parseDecisionMode(body?.decision_mode);
	if (!saved) throw new Error('The server did not confirm the Decision Engine setting.');
	return saved;
}

export interface EngineRoster {
	/** Server-wide Decision Engine policy for managed chat and agentic execution. */
	decision_mode?: DecisionMode;
	engines: EngineAvailability[];
	/** The live process default — what runs think with when no grant pins one. */
	current: string;
	/** Who thinks a chat turn (`chat.harness_engine`). */
	chat_current: string;
	/** The run engine's model; `undefined` when the CLI picks (`default`). */
	run_model?: string;
	/** Optional Magician LLM profile used when Pi drives agentic runs. */
	run_pi_profile?: string | null;
	/** The chat mouth's model; `undefined` when the CLI picks (`default`). */
	chat_model?: string;
	/** Set on the Magician-only stand-in returned when the endpoint answered
	 *  with an error: it says nothing about the server's real engines. */
	unavailable?: boolean;
}

/** The roster with on-this-machine install status (PATH lookup server-side),
 * so the dropdown greys out engines the operator cannot run. Fail closed to
 * Magician-only when the endpoint is unreachable. */
export async function fetchEngineAvailability(
	signal?: AbortSignal
): Promise<EngineRoster> {
	const response = await fetch('/api/magician/v2/plane/engines', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store',
		redirect: 'error',
		signal
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) {
		return {
			engines: [{ name: 'magician', installed: true }],
			current: 'magician',
			chat_current: 'magician',
			unavailable: true
		};
	}
	const roster = body as {
		decision_mode?: unknown;
		engines?: unknown;
		current?: unknown;
		chat_current?: unknown;
		run_model?: unknown;
		run_pi_profile?: unknown;
		chat_model?: unknown;
	} | null;
	const engines = Array.isArray(roster?.engines)
		? (roster.engines as EngineAvailability[]).map((engine) => ({
				...engine,
				native_tool_posture: parseNativeToolPosture(engine.native_tool_posture)
			}))
		: [{ name: 'magician', installed: true, native_tool_posture: 'stripped' as const }];
	const current = typeof roster?.current === 'string' ? roster.current : 'magician';
	const chat_current =
		typeof roster?.chat_current === 'string' ? roster.chat_current : 'magician';
	const model_of = (value: unknown): string | undefined =>
		typeof value === 'string' && value ? value : undefined;
	return {
		decision_mode: parseDecisionMode(roster?.decision_mode),
		engines,
		current,
		chat_current,
		run_model: model_of(roster?.run_model),
		run_pi_profile: model_of(roster?.run_pi_profile) ?? null,
		chat_model: model_of(roster?.chat_model)
	};
}

/** Persist `chat.harness_engine` (+ `harness_model`) and install the snapshot. */
export async function setChatHarnessEngine(
	harness_engine: string,
	harness_model: string = 'default'
): Promise<{ chat_current: string; chat_model?: string }> {
	const response = await fetch('/api/magician/v2/plane/chat-engine', {
		method: 'PUT',
		headers: scopedRequestHeaders({
			'Content-Type': 'application/json',
			Accept: 'application/json'
		}),
		body: JSON.stringify({ harness_engine, harness_model }),
		redirect: 'error'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) {
		throw new Error(errorMessage(body) ?? 'The chat engine could not be updated.');
	}
	const chat_current =
		body && typeof body === 'object' && typeof (body as { chat_current?: unknown }).chat_current === 'string'
			? (body as { chat_current: string }).chat_current
			: harness_engine;
	const parsed = (body ?? {}) as { chat_model?: unknown };
	return {
		chat_current,
		chat_model: typeof parsed.chat_model === 'string' ? parsed.chat_model : harness_model
	};
}

export async function listTerminalGrants(signal?: AbortSignal): Promise<TerminalGrantRow[]> {
	const response = await fetch('/api/magician/v2/plane/grants', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store',
		redirect: 'error',
		signal
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) {
		throw new Error(errorMessage(body) ?? 'Terminal grants are unavailable.');
	}
	const grants = (body as { grants?: unknown } | null)?.grants;
	return Array.isArray(grants) ? (grants as TerminalGrantRow[]) : [];
}

export interface MintTerminalGrantInput {
	label: string;
	workspace: string;
	agent_identity: string;
	harness_engine: string;
	allowed_tools: string[];
	ttl_hours: number;
	max_usd?: number | null;
	max_wall_clock_secs?: number | null;
	max_concurrent_runs?: number | null;
}

export async function mintTerminalGrant(
	input: MintTerminalGrantInput
): Promise<MintedGrant> {
	const response = await fetch('/api/magician/v2/plane/grants', {
		method: 'POST',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json', Accept: 'application/json' }),
		body: JSON.stringify(input),
		redirect: 'error'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) {
		throw new Error(errorMessage(body) ?? 'The grant could not be minted.');
	}
	return body as MintedGrant;
}

export async function revokeTerminalGrant(id: string): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/plane/grants/${encodeURIComponent(id)}`,
		{
			method: 'DELETE',
			headers: scopedRequestHeaders({ Accept: 'application/json' }),
			redirect: 'error'
		}
	);
	if (!response.ok && response.status !== 404) {
		const body = await response.json().catch(() => null);
		throw new Error(errorMessage(body) ?? 'The grant could not be revoked.');
	}
}

function parseNativeToolPosture(value: unknown): NativeToolPosture | undefined {
	if (
		value === 'stripped' ||
		value === 'denylisted' ||
		value === 'sandboxed' ||
		value === 'live'
	) {
		return value;
	}
	return undefined;
}

function errorMessage(body: unknown): string | null {
	if (body && typeof body === 'object') {
		const { message, error } = body as { message?: unknown; error?: unknown };
		if (typeof message === 'string' && message.length > 0) return message;
		if (typeof error === 'string' && error.length > 0) return error;
	}
	return null;
}

/** Persist `execution.harness_engine` (+ `harness_model`) and install the
 * snapshot. The run engine is also the affinity driver: the system's
 * non-local calls follow whatever drives the loop. */
export async function setRunHarnessEngine(
	harness_engine: string,
	harness_model: string = 'default',
	pi_profile?: string | null
): Promise<{ engine: string; harness_model: string; pi_profile: string | null }> {
	const response = await fetch('/api/magician/v2/plane/engine', {
		method: 'PUT',
		headers: scopedRequestHeaders({
			'Content-Type': 'application/json',
			Accept: 'application/json'
		}),
		body: JSON.stringify({
			harness_engine,
			harness_model,
			...(pi_profile !== undefined ? { pi_profile } : {})
		}),
		redirect: 'error'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) {
		throw new Error(errorMessage(body) ?? 'The run engine could not be updated.');
	}
	const parsed = (body ?? {}) as { engine?: unknown; harness_model?: unknown; pi_profile?: unknown };
	return {
		engine: typeof parsed.engine === 'string' ? parsed.engine : harness_engine,
		harness_model:
			typeof parsed.harness_model === 'string' ? parsed.harness_model : harness_model,
		pi_profile: typeof parsed.pi_profile === 'string' ? parsed.pi_profile : null
	};
}

export interface ChatProfileChoice {
	name: string;
	provider: string;
	model: string;
	is_default?: boolean;
	is_adaptive?: boolean;
}

export async function fetchChatProfileChoices(): Promise<ChatProfileChoice[]> {
	const response = await fetch('/api/magician/v2/chat/profiles', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store',
		redirect: 'error'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw new Error(errorMessage(body) ?? 'Profiles are unavailable.');
	return Array.isArray((body as { profiles?: unknown } | null)?.profiles)
		? (body as { profiles: ChatProfileChoice[] }).profiles
		: [];
}
