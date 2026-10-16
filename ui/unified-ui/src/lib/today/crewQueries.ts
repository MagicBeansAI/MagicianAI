/**
 * "State of the Crew" (Operations carousel slide 3): per-agent activity over
 * the last 24 hours (rolling window, not the local calendar day).
 *
 * Sources:
 * - agents: the agent store (`GET /api/magician/v2/agents`);
 * - model use: one grouped query on `/api/magician/v2/analytics/llm_calls/query`
 *   (`buildCrewLlmSql`), parsed by column name;
 * - tasks: `GET /api/magician/v3/tasks?limit=100&sort=updated_at&order=desc`,
 *   paged by `cursor` while the last row is still inside the window (max 5 pages).
 *
 * The builders and joins are pure; only `fetchCrewActivity` touches the network.
 * Nothing here invents a number: missing data stays 0 / "—".
 */
import { timedFetch } from '$lib/shared/fetch';
import type { AgentSummary } from '$lib/stores/agentStore';
import { toNumber, type PulseQueryResponse } from '$lib/today/pulseQueries';

export const CREW_WINDOW_MS = 86_400_000;
export const CREW_TASK_PAGE_LIMIT = 100;
export const CREW_TASK_MAX_PAGES = 5;

const LLM_CALLS_ENDPOINT = '/api/magician/v2/analytics/llm_calls/query';
const TASKS_ENDPOINT = '/api/magician/v3/tasks';

export interface CrewLlmRow {
	agentId: string;
	calls: number;
	costUsd: number;
	okCalls: number;
	avgLatencyMs: number | null;
}

export interface CrewTaskRow {
	agentId: string | null;
	status: string;
	updatedAtMs: number;
}

export type CrewAgentState = 'active' | 'idle' | 'off';

export interface CrewRow {
	agentId: string;
	name: string;
	state: CrewAgentState;
	costUsd: number;
	calls: number;
	okCalls: number;
	done: number;
	failed: number;
	activeTasks: number;
	/** done / (done + failed) as a whole percent; null when both are 0. */
	successPct: number | null;
	/** ok_calls / calls as a whole percent; null without calls. */
	reliabilityPct: number | null;
}

export interface CrewTotals {
	activeNow: number;
	totalAgents: number;
	costUsd: number;
	tasksDone: number;
	reliabilityPct: number | null;
}

/** Epoch-ms literal only — never interpolate caller strings. */
export function buildCrewLlmSql(sinceMs: number): string {
	const since = Math.floor(Number.isFinite(sinceMs) ? sinceMs : 0);
	return `SELECT agent_id, COUNT(*) AS calls, COALESCE(SUM(cost_usd), 0) AS cost_usd,
       SUM(CASE WHEN success THEN 1 ELSE 0 END) AS ok_calls,
       AVG(latency_ms) AS avg_latency_ms
FROM llm_calls
WHERE timestamp_ms >= ${since}
  AND NULLIF(TRIM(agent_id), '') IS NOT NULL
  AND (COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate')
GROUP BY agent_id ORDER BY cost_usd DESC`;
}

export function parseCrewLlm(res: PulseQueryResponse): CrewLlmRow[] {
	const col = (name: string) => res.columns.indexOf(name);
	const iAgent = col('agent_id');
	if (iAgent < 0) return [];
	const iCalls = col('calls');
	const iCost = col('cost_usd');
	const iOk = col('ok_calls');
	const iLatency = col('avg_latency_ms');
	const out: CrewLlmRow[] = [];
	for (const row of res.rows) {
		if (!Array.isArray(row)) continue;
		const agentId = typeof row[iAgent] === 'string' ? (row[iAgent] as string).trim() : '';
		if (!agentId) continue;
		const latency = iLatency >= 0 ? row[iLatency] : null;
		out.push({
			agentId,
			calls: iCalls >= 0 ? toNumber(row[iCalls]) : 0,
			costUsd: iCost >= 0 ? toNumber(row[iCost]) : 0,
			okCalls: iOk >= 0 ? toNumber(row[iOk]) : 0,
			avgLatencyMs: latency === null || latency === undefined ? null : toNumber(latency)
		});
	}
	return out;
}

/** Epoch ms from an ISO string or a number (seconds when below 1e12). */
export function timestampToMs(value: unknown): number {
	if (typeof value === 'number' && Number.isFinite(value)) return value < 1e12 ? value * 1000 : value;
	if (typeof value === 'string' && value.trim()) {
		const asNumber = Number(value);
		if (Number.isFinite(asNumber)) return timestampToMs(asNumber);
		const parsed = Date.parse(value);
		return Number.isFinite(parsed) ? parsed : Number.NaN;
	}
	return Number.NaN;
}

export function parseCrewTaskPage(payload: unknown): { rows: CrewTaskRow[]; nextCursor: string | null } {
	const data = (payload && typeof payload === 'object' ? payload : {}) as Record<string, unknown>;
	const rawRows = Array.isArray(data.tasks) ? data.tasks : [];
	const rows: CrewTaskRow[] = [];
	for (const raw of rawRows) {
		if (!raw || typeof raw !== 'object') continue;
		const r = raw as Record<string, unknown>;
		const agentId = typeof r.agent_id === 'string' && r.agent_id.trim() ? r.agent_id.trim() : null;
		rows.push({
			agentId,
			status: typeof r.status === 'string' ? r.status : '',
			updatedAtMs: timestampToMs(r.updated_at)
		});
	}
	const pagination = (data.pagination && typeof data.pagination === 'object' ? data.pagination : {}) as Record<
		string,
		unknown
	>;
	const cursor = pagination.next_cursor;
	return { rows, nextCursor: typeof cursor === 'string' && cursor ? cursor : null };
}

/** Walk on only while the page's last row is still inside the window. */
export function shouldFetchNextTaskPage(
	pageRows: readonly CrewTaskRow[],
	nextCursor: string | null,
	sinceMs: number,
	pagesFetched: number,
	maxPages = CREW_TASK_MAX_PAGES
): boolean {
	if (!nextCursor || pagesFetched >= maxPages || pageRows.length === 0) return false;
	const last = pageRows[pageRows.length - 1];
	return Number.isFinite(last.updatedAtMs) && last.updatedAtMs >= sinceMs;
}

export function agentCrewState(agent: Pick<AgentSummary, 'status' | 'disabled'>): CrewAgentState {
	if (agent.status === 'running' || agent.status === 'triggered') return 'active';
	if (agent.disabled || agent.status === 'disabled') return 'off';
	return 'idle';
}

const DONE = new Set(['completed', 'done']);
const ACTIVE = new Set(['running', 'paused', 'planning']);

function pct(numerator: number, denominator: number): number | null {
	return denominator > 0 ? Math.round((numerator / denominator) * 100) : null;
}

/**
 * Join agents + model use + tasks into rows. Lists agents with any calls or
 * tasks in the window plus any agent active now; active first, then cost,
 * then tasks done.
 */
export function buildCrewRows(input: {
	agents: readonly Pick<AgentSummary, 'agent_id' | 'name' | 'status' | 'disabled'>[];
	llm: readonly CrewLlmRow[];
	tasks: readonly CrewTaskRow[];
	sinceMs: number;
}): CrewRow[] {
	const rows = new Map<string, CrewRow>();
	const ensure = (agentId: string): CrewRow => {
		let row = rows.get(agentId);
		if (!row) {
			row = {
				agentId,
				name: agentId,
				state: 'idle',
				costUsd: 0,
				calls: 0,
				okCalls: 0,
				done: 0,
				failed: 0,
				activeTasks: 0,
				successPct: null,
				reliabilityPct: null
			};
			rows.set(agentId, row);
		}
		return row;
	};

	for (const llm of input.llm) {
		const row = ensure(llm.agentId);
		row.calls += llm.calls;
		row.costUsd += llm.costUsd;
		row.okCalls += llm.okCalls;
	}
	for (const task of input.tasks) {
		if (!task.agentId || !Number.isFinite(task.updatedAtMs) || task.updatedAtMs < input.sinceMs) continue;
		const row = ensure(task.agentId);
		if (DONE.has(task.status)) row.done += 1;
		else if (task.status === 'failed') row.failed += 1;
		else if (ACTIVE.has(task.status)) row.activeTasks += 1;
	}
	for (const agent of input.agents) {
		const state = agentCrewState(agent);
		const existing = rows.get(agent.agent_id);
		if (!existing && state !== 'active') continue;
		const row = existing ?? ensure(agent.agent_id);
		row.name = agent.name?.trim() || agent.agent_id;
		row.state = state;
	}

	const out = [...rows.values()];
	for (const row of out) {
		if (row.activeTasks > 0 && row.state !== 'off') row.state = 'active';
		row.successPct = pct(row.done, row.done + row.failed);
		row.reliabilityPct = pct(row.okCalls, row.calls);
	}
	const rank = (row: CrewRow) => (row.state === 'active' ? 0 : 1);
	return out.sort(
		(a, b) =>
			rank(a) - rank(b) ||
			b.costUsd - a.costUsd ||
			b.done + b.failed - (a.done + a.failed) ||
			a.name.localeCompare(b.name)
	);
}

export function crewTotals(rows: readonly CrewRow[], totalAgents: number): CrewTotals {
	let calls = 0;
	let ok = 0;
	let cost = 0;
	let done = 0;
	for (const row of rows) {
		calls += row.calls;
		ok += row.okCalls;
		cost += row.costUsd;
		done += row.done;
	}
	return {
		activeNow: rows.filter((r) => r.state === 'active').length,
		totalAgents,
		costUsd: cost,
		tasksDone: done,
		reliabilityPct: pct(ok, calls)
	};
}

export type PctTone = 'success' | 'warning' | 'danger' | 'muted';

export function pctTone(value: number | null): PctTone {
	if (value === null) return 'muted';
	if (value >= 95) return 'success';
	if (value >= 80) return 'warning';
	return 'danger';
}

export function formatPct(value: number | null): string {
	return value === null ? '—' : `${value}%`;
}

type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

/** Model-use query + task page walk. Throws when either source fails. */
export async function fetchCrewActivity(
	sinceMs: number,
	fetchImpl: FetchLike = timedFetch
): Promise<{ llm: CrewLlmRow[]; tasks: CrewTaskRow[] }> {
	const llmPromise = (async () => {
		const response = await fetchImpl(LLM_CALLS_ENDPOINT, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ sql: buildCrewLlmSql(sinceMs) })
		});
		if (!response.ok) throw new Error(`Crew model-use query failed (${response.status})`);
		const payload = (await response.json()) as Partial<PulseQueryResponse>;
		return parseCrewLlm({
			columns: Array.isArray(payload.columns) ? payload.columns : [],
			rows: Array.isArray(payload.rows) ? payload.rows : []
		});
	})();

	const tasksPromise = (async () => {
		const tasks: CrewTaskRow[] = [];
		let cursor: string | null = null;
		for (let page = 1; page <= CREW_TASK_MAX_PAGES; page += 1) {
			const params = new URLSearchParams({
				limit: String(CREW_TASK_PAGE_LIMIT),
				sort: 'updated_at',
				order: 'desc'
			});
			if (cursor) params.set('cursor', cursor);
			const response = await fetchImpl(`${TASKS_ENDPOINT}?${params.toString()}`);
			if (!response.ok) throw new Error(`Crew task read failed (${response.status})`);
			const { rows, nextCursor } = parseCrewTaskPage(await response.json());
			tasks.push(...rows.filter((row) => Number.isFinite(row.updatedAtMs) && row.updatedAtMs >= sinceMs));
			if (!shouldFetchNextTaskPage(rows, nextCursor, sinceMs, page)) break;
			cursor = nextCursor;
		}
		return tasks;
	})();

	const [llm, tasks] = await Promise.all([llmPromise, tasksPromise]);
	return { llm, tasks };
}
