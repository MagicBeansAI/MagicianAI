import type { Task } from '$lib/stores/taskStore';
import type { VibeDevProject } from '$lib/stores/vibeDevProjectStore';
import { parseNumericDuckDbValue } from '$lib/magician/components/generative/chartUtil';

const VIBEDEV_THREAD_ID = 'vibedev';
const VIBEDEV_PROJECT_PREFIX = 'VibeDev project:';
const LLM_LIFETIME_PARTITION_HINT = '/* magician:all_llm_partitions */';

export interface VibeDevProjectAttribution {
	projectId: string;
	chatSessionId: string | null;
	taskIds: string[];
}

export interface VibeDevProjectCost {
	projectId: string;
	calls: number;
	costUsd: number;
	inputTokens: number;
	outputTokens: number;
	reasoningTokens: number;
	cacheReadTokens: number;
	cacheCreationTokens: number;
	topProvider: string | null;
	topModel: string | null;
	topModelCalls: number;
	topModelCostUsd: number;
}

export interface VibeDevRunCost {
	taskId: string;
	calls: number;
	costUsd: number;
	inputTokens: number;
	outputTokens: number;
	reasoningTokens: number;
	cacheReadTokens: number;
	cacheCreationTokens: number;
	topProvider: string | null;
	topModel: string | null;
	topModelCalls: number;
	topModelCostUsd: number;
}

export interface VibeDevRunAttribution {
	runId: string;
	taskIds: string[];
}

export interface VibeDevTotalCost {
	projects: number;
	calls: number;
	costUsd: number;
	inputTokens: number;
	outputTokens: number;
	reasoningTokens: number;
	cacheReadTokens: number;
	cacheCreationTokens: number;
}

export function isVibeDevTask(task: Task): boolean {
	if (task.uiThreadId === VIBEDEV_THREAD_ID) return true;
	return (task.tags ?? []).some((tag) => tag.name.toLowerCase() === VIBEDEV_THREAD_ID);
}

export function projectIdFromTaskDescription(description: string | undefined): string | null {
	if (!description) return null;
	for (const line of description.split('\n')) {
		const trimmed = line.trim();
		if (!trimmed.startsWith(VIBEDEV_PROJECT_PREFIX)) continue;
		const value = trimmed.slice(VIBEDEV_PROJECT_PREFIX.length).trim();
		return value.length > 0 ? value : null;
	}
	return null;
}

export function buildVibeDevProjectAttributions(
	projects: VibeDevProject[],
	tasks: Task[]
): VibeDevProjectAttribution[] {
	const byProject = new Map<
		string,
		{
			projectId: string;
			chatSessionId: string | null;
			taskIds: Set<string>;
		}
	>();
	for (const project of projects) {
		byProject.set(project.project_id, {
			projectId: project.project_id,
			chatSessionId: project.chat_session_id || null,
			taskIds: new Set([
				...(project.run_task_ids ?? []),
				...(project.active_root_task_id ? [project.active_root_task_id] : [])
			])
		});
	}
	for (const task of tasks) {
		if (!isVibeDevTask(task)) continue;
		const projectId = projectIdFromTaskDescription(task.description);
		if (!projectId) continue;
		const attribution = byProject.get(projectId);
		if (attribution) attribution.taskIds.add(task.id);
	}
	return Array.from(byProject.values()).map((entry) => ({
		projectId: entry.projectId,
		chatSessionId: entry.chatSessionId,
		taskIds: Array.from(entry.taskIds).filter(Boolean).sort()
	}));
}

export function emptyProjectCost(projectId: string): VibeDevProjectCost {
	return {
		projectId,
		calls: 0,
		costUsd: 0,
		inputTokens: 0,
		outputTokens: 0,
		reasoningTokens: 0,
		cacheReadTokens: 0,
		cacheCreationTokens: 0,
		topProvider: null,
		topModel: null,
		topModelCalls: 0,
		topModelCostUsd: 0
	};
}

export function emptyRunCost(taskId: string): VibeDevRunCost {
	return {
		taskId,
		calls: 0,
		costUsd: 0,
		inputTokens: 0,
		outputTokens: 0,
		reasoningTokens: 0,
		cacheReadTokens: 0,
		cacheCreationTokens: 0,
		topProvider: null,
		topModel: null,
		topModelCalls: 0,
		topModelCostUsd: 0
	};
}

export function totalTokens(cost: VibeDevProjectCost | VibeDevRunCost | VibeDevTotalCost): number {
	return cost.inputTokens + cost.outputTokens + cost.reasoningTokens;
}

export function formatUsd(value: number): string {
	if (!Number.isFinite(value) || value <= 0) return '$0';
	if (value < 0.01) return `$${value.toFixed(4)}`;
	if (value < 1) return `$${value.toFixed(3)}`;
	return `$${value.toFixed(2)}`;
}

export function formatCompactNumber(value: number): string {
	return new Intl.NumberFormat(undefined, {
		notation: 'compact',
		maximumFractionDigits: 1
	}).format(Number.isFinite(value) ? value : 0);
}

export function modelLabel(cost: VibeDevProjectCost | VibeDevRunCost): string {
	const model = cost.topModel?.trim();
	const provider = cost.topProvider?.trim();
	if (provider && model && provider !== 'unknown') return `${provider}/${model}`;
	if (model) return model;
	if (provider && provider !== 'unknown') return provider;
	return 'none yet';
}

export function costByProject(
	projects: VibeDevProject[],
	rows: Array<Record<string, unknown>>
): Map<string, VibeDevProjectCost> {
	const costs = new Map(projects.map((project) => [project.project_id, emptyProjectCost(project.project_id)]));
	for (const row of rows) {
		const projectId = String(row.project_id ?? '').trim();
		if (!projectId) continue;
		costs.set(projectId, parseProjectCostRow(row, projectId));
	}
	return costs;
}

export function costByRun(
	runIds: string[],
	rows: Array<Record<string, unknown>>
): Map<string, VibeDevRunCost> {
	const costs = new Map(runIds.map((runId) => [runId, emptyRunCost(runId)]));
	for (const row of rows) {
		const taskId = String(row.run_id ?? row.run_task_id ?? row.task_id ?? '').trim();
		if (!taskId) continue;
		costs.set(taskId, parseRunCostRow(row, taskId));
	}
	return costs;
}

export function parseVibeDevTotalCost(row: Record<string, unknown> | undefined): VibeDevTotalCost {
	return {
		projects: cellNumber(row?.projects),
		calls: cellNumber(row?.calls),
		costUsd: cellNumber(row?.cost_usd),
		inputTokens: cellNumber(row?.input_tokens),
		outputTokens: cellNumber(row?.output_tokens),
		reasoningTokens: cellNumber(row?.reasoning_tokens),
		cacheReadTokens: cellNumber(row?.cache_read_tokens),
		cacheCreationTokens: cellNumber(row?.cache_creation_tokens)
	};
}

export function buildVibeDevProjectRollupSql(
	attributions: VibeDevProjectAttribution[],
	days: number,
	dimensionSql: string[] = []
): string | null {
	const rows = attributionRowsSql(attributions);
	if (!rows) return null;
	const filters = scopedFilters(days, dimensionSql);
	return (
		`WITH project_keys(project_id, key_type, key_value) AS (${rows}), ` +
		`base AS (` +
		`SELECT row_number() OVER () AS call_row_id, * FROM llm_calls WHERE ${filters}` +
		`), scoped AS (` +
		`SELECT DISTINCT pk.project_id, b.call_row_id, b.provider, b.model, b.cost_usd, ` +
		`b.input_tokens, b.output_tokens, b.reasoning_tokens, b.cache_read_tokens, b.cache_creation_tokens ` +
		`FROM base b JOIN project_keys pk ON ` +
		`(pk.key_type = 'chat_session_id' AND b.chat_session_id = pk.key_value) OR ` +
		`(pk.key_type = 'task_id' AND b.task_id = pk.key_value)` +
		`), rollup AS (` +
		`SELECT project_id, COUNT(*) AS calls, COALESCE(SUM(cost_usd), 0) AS cost_usd, ` +
		`COALESCE(SUM(input_tokens), 0) AS input_tokens, COALESCE(SUM(output_tokens), 0) AS output_tokens, ` +
		`COALESCE(SUM(reasoning_tokens), 0) AS reasoning_tokens, ` +
		`COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens, ` +
		`COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens ` +
		`FROM scoped GROUP BY project_id` +
		`), model_counts AS (` +
		`SELECT project_id, COALESCE(NULLIF(provider, ''), 'unknown') AS top_provider, ` +
		`COALESCE(NULLIF(model, ''), 'unknown') AS top_model, COUNT(*) AS top_model_calls, ` +
		`COALESCE(SUM(cost_usd), 0) AS top_model_cost_usd ` +
		`FROM scoped GROUP BY 1, 2, 3` +
		`), top_models AS (` +
		`SELECT *, ROW_NUMBER() OVER (PARTITION BY project_id ORDER BY top_model_calls DESC, top_model_cost_usd DESC, top_model) AS rn ` +
		`FROM model_counts` +
		`) ` +
		`SELECT r.project_id, r.calls, r.cost_usd, r.input_tokens, r.output_tokens, r.reasoning_tokens, ` +
		`r.cache_read_tokens, r.cache_creation_tokens, tm.top_provider, tm.top_model, ` +
		`tm.top_model_calls, tm.top_model_cost_usd ` +
		`FROM rollup r LEFT JOIN top_models tm ON tm.project_id = r.project_id AND tm.rn = 1 ` +
		`ORDER BY r.cost_usd DESC, r.calls DESC, r.project_id`
	);
}

export function buildVibeDevRunRollupSql(
	attributions: VibeDevRunAttribution[],
	days: number,
	dimensionSql: string[] = []
): string | null {
	const rows = runAttributionRowsSql(attributions);
	if (!rows) return null;
	const filters = scopedFilters(days, dimensionSql);
	return (
		`WITH run_keys(run_id, task_id) AS (${rows}), ` +
		`base AS (` +
		`SELECT row_number() OVER () AS call_row_id, * FROM llm_calls WHERE ${filters}` +
		`), scoped AS (` +
		`SELECT DISTINCT rk.run_id, b.call_row_id, b.provider, b.model, b.cost_usd, ` +
		`b.input_tokens, b.output_tokens, b.reasoning_tokens, b.cache_read_tokens, b.cache_creation_tokens ` +
		`FROM base b JOIN run_keys rk ON b.task_id = rk.task_id` +
		`), rollup AS (` +
		`SELECT run_id, COUNT(*) AS calls, COALESCE(SUM(cost_usd), 0) AS cost_usd, ` +
		`COALESCE(SUM(input_tokens), 0) AS input_tokens, COALESCE(SUM(output_tokens), 0) AS output_tokens, ` +
		`COALESCE(SUM(reasoning_tokens), 0) AS reasoning_tokens, ` +
		`COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens, ` +
		`COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens ` +
		`FROM scoped GROUP BY run_id` +
		`), model_counts AS (` +
		`SELECT run_id, COALESCE(NULLIF(provider, ''), 'unknown') AS top_provider, ` +
		`COALESCE(NULLIF(model, ''), 'unknown') AS top_model, COUNT(*) AS top_model_calls, ` +
		`COALESCE(SUM(cost_usd), 0) AS top_model_cost_usd ` +
		`FROM scoped GROUP BY 1, 2, 3` +
		`), top_models AS (` +
		`SELECT *, ROW_NUMBER() OVER (PARTITION BY run_id ORDER BY top_model_calls DESC, top_model_cost_usd DESC, top_model) AS rn ` +
		`FROM model_counts` +
		`) ` +
		`SELECT r.run_id, r.calls, r.cost_usd, r.input_tokens, r.output_tokens, r.reasoning_tokens, ` +
		`r.cache_read_tokens, r.cache_creation_tokens, tm.top_provider, tm.top_model, ` +
		`tm.top_model_calls, tm.top_model_cost_usd ` +
		`FROM rollup r LEFT JOIN top_models tm ON tm.run_id = r.run_id AND tm.rn = 1 ` +
		`ORDER BY r.cost_usd DESC, r.calls DESC, r.run_id`
	);
}

export function buildVibeDevTotalRollupSql(
	attributions: VibeDevProjectAttribution[],
	days: number,
	dimensionSql: string[] = []
): string | null {
	const rows = attributionRowsSql(attributions);
	if (!rows) return null;
	const filters = scopedFilters(days, dimensionSql);
	return (
		`WITH project_keys(project_id, key_type, key_value) AS (${rows}), ` +
		`base AS (` +
		`SELECT row_number() OVER () AS call_row_id, * FROM llm_calls WHERE ${filters}` +
		`), scoped AS (` +
		`SELECT DISTINCT pk.project_id, b.call_row_id, b.cost_usd, b.input_tokens, b.output_tokens, ` +
		`b.reasoning_tokens, b.cache_read_tokens, b.cache_creation_tokens ` +
		`FROM base b JOIN project_keys pk ON ` +
		`(pk.key_type = 'chat_session_id' AND b.chat_session_id = pk.key_value) OR ` +
		`(pk.key_type = 'task_id' AND b.task_id = pk.key_value)` +
		`) ` +
		`SELECT COUNT(DISTINCT project_id) AS projects, COUNT(*) AS calls, COALESCE(SUM(cost_usd), 0) AS cost_usd, ` +
		`COALESCE(SUM(input_tokens), 0) AS input_tokens, COALESCE(SUM(output_tokens), 0) AS output_tokens, ` +
		`COALESCE(SUM(reasoning_tokens), 0) AS reasoning_tokens, ` +
		`COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens, ` +
		`COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens ` +
		`FROM scoped`
	);
}

export function buildVibeDevModelBreakdownSql(
	attributions: VibeDevProjectAttribution[],
	days: number,
	dimensionSql: string[] = [],
	limit = 10
): string | null {
	const rows = attributionRowsSql(attributions);
	if (!rows) return null;
	const filters = scopedFilters(days, dimensionSql);
	return (
		`WITH project_keys(project_id, key_type, key_value) AS (${rows}), ` +
		`base AS (` +
		`SELECT row_number() OVER () AS call_row_id, * FROM llm_calls WHERE ${filters}` +
		`), scoped AS (` +
		`SELECT DISTINCT b.call_row_id, b.provider, b.model, b.cost_usd, b.input_tokens, b.output_tokens, b.reasoning_tokens ` +
		`FROM base b JOIN project_keys pk ON ` +
		`(pk.key_type = 'chat_session_id' AND b.chat_session_id = pk.key_value) OR ` +
		`(pk.key_type = 'task_id' AND b.task_id = pk.key_value)` +
		`) ` +
		`SELECT COALESCE(NULLIF(provider, ''), 'unknown') AS provider, ` +
		`COALESCE(NULLIF(model, ''), 'unknown') AS model, COUNT(*) AS calls, ` +
		`COALESCE(SUM(cost_usd), 0) AS cost_usd, COALESCE(SUM(input_tokens), 0) AS input_tokens, ` +
		`COALESCE(SUM(output_tokens), 0) AS output_tokens, COALESCE(SUM(reasoning_tokens), 0) AS reasoning_tokens ` +
		`FROM scoped GROUP BY 1, 2 ORDER BY cost_usd DESC, calls DESC LIMIT ${Math.max(1, Math.min(limit, 50))}`
	);
}

function parseProjectCostRow(row: Record<string, unknown>, fallbackProjectId: string): VibeDevProjectCost {
	return {
		projectId: String(row.project_id ?? fallbackProjectId),
		calls: cellNumber(row.calls),
		costUsd: cellNumber(row.cost_usd),
		inputTokens: cellNumber(row.input_tokens),
		outputTokens: cellNumber(row.output_tokens),
		reasoningTokens: cellNumber(row.reasoning_tokens),
		cacheReadTokens: cellNumber(row.cache_read_tokens),
		cacheCreationTokens: cellNumber(row.cache_creation_tokens),
		topProvider: stringCell(row.top_provider),
		topModel: stringCell(row.top_model),
		topModelCalls: cellNumber(row.top_model_calls),
		topModelCostUsd: cellNumber(row.top_model_cost_usd)
	};
}

function parseRunCostRow(row: Record<string, unknown>, fallbackTaskId: string): VibeDevRunCost {
	return {
		taskId: String(row.run_id ?? row.run_task_id ?? row.task_id ?? fallbackTaskId),
		calls: cellNumber(row.calls),
		costUsd: cellNumber(row.cost_usd),
		inputTokens: cellNumber(row.input_tokens),
		outputTokens: cellNumber(row.output_tokens),
		reasoningTokens: cellNumber(row.reasoning_tokens),
		cacheReadTokens: cellNumber(row.cache_read_tokens),
		cacheCreationTokens: cellNumber(row.cache_creation_tokens),
		topProvider: stringCell(row.top_provider),
		topModel: stringCell(row.top_model),
		topModelCalls: cellNumber(row.top_model_calls),
		topModelCostUsd: cellNumber(row.top_model_cost_usd)
	};
}

// Emit the attribution key set as a `SELECT … UNION ALL SELECT …` chain rather
// than a `VALUES (…)` list. Both are read-only literal row sets, but the
// governed legacy-LLM read guard only permits SELECT-shaped query bodies, so a
// `VALUES` CTE trips validation and (being in the dashboard batch) takes down
// every widget. The enclosing CTE supplies the column names, so the literal
// SELECTs are positional and need no aliases.
function attributionRowsSql(attributions: VibeDevProjectAttribution[]): string | null {
	const rows: string[] = [];
	for (const attribution of attributions) {
		const projectId = attribution.projectId.trim();
		if (!projectId) continue;
		if (attribution.chatSessionId?.trim()) {
			rows.push(
				`SELECT '${escapeSqlValue(projectId)}', 'chat_session_id', '${escapeSqlValue(attribution.chatSessionId.trim())}'`
			);
		}
		for (const taskId of attribution.taskIds) {
			const trimmed = taskId.trim();
			if (!trimmed) continue;
			rows.push(`SELECT '${escapeSqlValue(projectId)}', 'task_id', '${escapeSqlValue(trimmed)}'`);
		}
	}
	return rows.length > 0 ? rows.join(' UNION ALL ') : null;
}

function runAttributionRowsSql(attributions: VibeDevRunAttribution[]): string | null {
	const rows: string[] = [];
	const seen = new Set<string>();
	for (const attribution of attributions) {
		const runId = attribution.runId.trim();
		if (!runId) continue;
		for (const taskId of attribution.taskIds) {
			const trimmed = taskId.trim();
			const key = `${runId}\n${trimmed}`;
			if (!trimmed || seen.has(key)) continue;
			seen.add(key);
			rows.push(`SELECT '${escapeSqlValue(runId)}', '${escapeSqlValue(trimmed)}'`);
		}
	}
	return rows.length > 0 ? rows.join(' UNION ALL ') : null;
}

function scopedFilters(days: number, dimensionSql: string[]): string {
	const safeDays = Math.floor(days);
	const parts =
		safeDays > 0
			? [
					`timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL ${safeDays} DAYS)`,
					...dimensionSql
				]
			: [`${LLM_LIFETIME_PARTITION_HINT} TRUE`, ...dimensionSql];
	return parts.length > 0 ? parts.join(' AND ') : 'TRUE';
}

function escapeSqlValue(value: string): string {
	return value.replace(/'/g, "''");
}

function cellNumber(value: unknown): number {
	const num = parseNumericDuckDbValue(value);
	return num === undefined || !Number.isFinite(num) ? 0 : num;
}

function stringCell(value: unknown): string | null {
	if (typeof value !== 'string') return null;
	const trimmed = value.trim();
	return trimmed.length > 0 ? trimmed : null;
}
