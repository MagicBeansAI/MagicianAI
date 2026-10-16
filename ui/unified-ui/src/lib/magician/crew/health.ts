import { timedFetch } from '$lib/shared/fetch';
import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';

const CREW_HEALTH_ENDPOINT = '/api/magician/v2/agents/health';

export type AgentHealthState = 'working' | 'needs_attention' | 'paused' | 'offline' | 'idle';
export type AgentHealthBand = 'good' | 'fair' | 'poor';
export type AgentHealthConfidence = 'high' | 'medium' | 'low';
export type AgentHealthTrend =
	| 'improving'
	| 'stable'
	| 'declining'
	| 'insufficient_history';

export interface AgentHealthCoverage {
	ratio: number;
	level: AgentHealthConfidence;
	present: string[];
	missing: string[];
}

export interface AgentHealthInputs {
	runtime_status: string;
	state: AgentHealthState;
	observation_window_started_at_ms: number;
	observation_window_ended_at_ms: number;
	calls_7d: number;
	spend_usd_7d: number;
	cost_observed_calls?: number;
	success_rate_7d: number | null;
	last_llm_call_at_ms: number | null;
	last_task_activity_at_ms: number | null;
	last_activity_at_ms: number | null;
	llm_analytics_available: boolean;
	task_activity_available: boolean;
}

export interface AgentHealthContributions {
	baseline: number;
	quality: number;
	recency: number;
	state: number;
}

export interface AgentOverallHealth {
	score: number;
	band: AgentHealthBand;
	observed_at_ms: number;
	formula_version: number;
	coverage: AgentHealthCoverage;
	inputs: AgentHealthInputs;
	contributions: AgentHealthContributions;
}

export interface AgentHealthSnapshot {
	date: string;
	observed_at_ms: number;
	score: number;
	band: AgentHealthBand;
	formula_version: number;
	coverage: AgentHealthCoverage;
	inputs: AgentHealthInputs;
	contributions: AgentHealthContributions;
}

export interface AgentHealthRolling7d {
	window_started_at_ms: number;
	window_ended_at_ms: number;
	calls: number;
	spend_usd: number;
	cost_observed_calls?: number;
	success_rate: number | null;
	last_call_at_ms: number | null;
	score_average: number | null;
	score_delta: number | null;
	trend: AgentHealthTrend;
	sample_days: number;
}

export interface AgentHealthProjection {
	agent_id: string;
	overall: AgentOverallHealth;
	rolling_7d: AgentHealthRolling7d;
	history: AgentHealthSnapshot[];
}

export interface CrewHealthAvailability {
	status: 'available' | 'partial';
	llm_analytics: boolean;
	durable_history: boolean;
	limitations: string[];
}

export interface CrewHealthResponse {
	schema_version: string;
	formula_version: number;
	generated_at: string;
	scope: { principal: string; workspace: string };
	availability: CrewHealthAvailability;
	agents: AgentHealthProjection[];
}

export interface CrewAgentHealthResponse {
	schema_version: string;
	formula_version: number;
	generated_at: string;
	scope: { principal: string; workspace: string };
	availability: CrewHealthAvailability;
	agent: AgentHealthProjection;
}

interface CachedHealth {
	at: number;
	value: CrewHealthResponse | null;
}

const healthCache = new Map<string, CachedHealth>();
const healthInflight = new Map<string, Promise<CrewHealthResponse | null>>();
const MAX_CACHED_SCOPES = 16;

function currentScopeKey(): string {
	const scope = getCurrentScopeIdentity();
	return `${scope.principal}:${scope.workspace}`;
}

export async function fetchCrewHealth(): Promise<CrewHealthResponse | null> {
	try {
		const response = await timedFetch(CREW_HEALTH_ENDPOINT);
		if (!response.ok) return null;
		const payload = (await response.json()) as CrewHealthResponse;
		return payload?.schema_version === 'crew_health.v1' && Array.isArray(payload.agents)
			? payload
			: null;
	} catch {
		return null;
	}
}

export function fetchCrewHealthCached(maxAgeMs = 60_000): Promise<CrewHealthResponse | null> {
	const scopeKey = currentScopeKey();
	const cached = healthCache.get(scopeKey);
	if (cached && Date.now() - cached.at < maxAgeMs) return Promise.resolve(cached.value);
	const inflight = healthInflight.get(scopeKey);
	if (inflight) return inflight;

	const request = fetchCrewHealth()
		.then((value) => {
			const responseScopeKey = value
				? `${value.scope.principal}:${value.scope.workspace}`
				: null;
			const scopedValue = responseScopeKey === scopeKey ? value : null;
			if (healthCache.size >= MAX_CACHED_SCOPES && !healthCache.has(scopeKey)) {
				const oldestScope = healthCache.keys().next().value;
				if (oldestScope) healthCache.delete(oldestScope);
			}
			healthCache.delete(scopeKey);
			healthCache.set(scopeKey, { at: Date.now(), value: scopedValue });
			return currentScopeKey() === scopeKey ? scopedValue : null;
		})
		.finally(() => {
			healthInflight.delete(scopeKey);
		});
	healthInflight.set(scopeKey, request);
	return request;
}

export async function fetchAgentHealth(agentId: string): Promise<CrewAgentHealthResponse | null> {
	const normalized = agentId.trim();
	if (!normalized) return null;
	try {
		const response = await timedFetch(
			`/api/magician/v2/agents/${encodeURIComponent(normalized)}/health`
		);
		if (!response.ok) return null;
		const payload = (await response.json()) as CrewAgentHealthResponse;
		return payload?.schema_version === 'crew_health.v1' && payload.agent?.agent_id === normalized
			? payload
			: null;
	} catch {
		return null;
	}
}

export function healthByAgent(
	response: CrewHealthResponse | null
): Map<string, AgentHealthProjection> | null {
	if (!response) return null;
	return new Map(response.agents.map((agent) => [agent.agent_id, agent]));
}

export function healthBand(score: number): AgentHealthBand {
	return score >= 70 ? 'good' : score >= 40 ? 'fair' : 'poor';
}
