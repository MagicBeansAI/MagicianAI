/**
 * The activity card's turn usage chip (calls · tokens · cost), and how the
 * server's whole-turn totals replace the summed rows.
 *
 * The per-turn events endpoint returns only the newest ~1 MiB of rows, so
 * summing what came back undercounts a long turn. Newer servers send
 * `usage` computed over the whole file (see `chatTurnEventsStore`'s
 * `TurnUsageBaseline`). When it is present the card shows the server
 * totals plus only the rows that arrived AFTER that read (`uncovered`) —
 * the rows that came back with the read are already inside the server
 * figures, so adding them again would double count. Older servers send no
 * `usage` and the card keeps summing rows.
 *
 * Both sides follow one contract: only the turn's OWN calls, one per call id
 * (`summarizeOwnCalls`); spawned / delegated task calls come from the task
 * usage store.
 */
import { cachedPercentOf, totalUsage } from '$lib/llm/tokenUsage';

export interface TurnUsageSummary {
	inputTokens: number;
	cachedTokens: number;
	cacheCreationTokens: number | null;
	outputTokens: number;
	callCount: number;
	/** False when task costs are known but token counts were not reported. */
	tokensReported?: boolean;
	/** `null` when no call reported cache figures; the chip then omits it. */
	hitPct: number | null;
	/** Summed `cost_usd` (chat calls) / `cost` (task-run calls); `null` when none priced. */
	costUsd: number | null;
	/** Tasks this turn started whose usage is included. */
	spawnedTasks?: number;
}

export interface ServerTurnUsage {
	calls: number;
	inputTokens: number;
	outputTokens: number;
	costUsd: number | null;
	/** Absent / `null` when the server did not report cache figures. */
	cacheReadTokens?: number | null;
	cacheCreationTokens?: number | null;
}

/**
 * One LLM-call row of the turn's event file, reduced to what the chip needs.
 *
 * The turn file holds the turn's OWN calls and also the calls of a task the
 * turn spawned or delegated to (rows whose `execution_id` is that task's
 * execution). Those task calls reach the chip from the task usage store, so
 * counting them here too double counted them. And the turn's own chat call
 * lands as TWO rows (`llm.succeeded` with `llm_call_id` / `cost_usd`, and
 * `LLMResponseReceived` with `correlation.llm_call_id` / `cost`), so cost
 * summed per row doubled it.
 */
export interface TurnCallRecord {
	callId?: string;
	executionId?: string | null;
	input: number | null;
	output: number | null;
	cacheRead: number | null;
	cacheCreation: number | null;
	costUsd: number | null;
}

/** Own = no execution id, or the turn's own id (inline chat stamps it as the execution id). */
export function isOwnTurnCall(record: TurnCallRecord, chatTurnId: string): boolean {
	return !record.executionId || record.executionId === chatTurnId;
}

/** Call ids of the turn's own calls among `records`. */
export function ownCallIds(records: TurnCallRecord[], chatTurnId: string): Set<string> {
	const ids = new Set<string>();
	for (const record of records) {
		if (record.callId && isOwnTurnCall(record, chatTurnId)) ids.add(record.callId);
	}
	return ids;
}

/**
 * The turn's own calls: one per distinct call id (tokens AND cost), rows
 * without an id counted individually. `skipCallIds` drops calls already
 * counted elsewhere (the server totals' covered rows).
 */
export function summarizeOwnCalls(
	records: TurnCallRecord[],
	chatTurnId: string,
	skipCallIds: ReadonlySet<string> = new Set()
): TurnUsageSummary | null {
	const own = records.filter(
		(record) =>
			isOwnTurnCall(record, chatTurnId) && !(record.callId && skipCallIds.has(record.callId))
	);
	const totals = totalUsage(own);
	const costById = new Map<string, number>();
	let anonCost: number | null = null;
	for (const record of own) {
		if (record.costUsd === null) continue;
		if (record.callId) {
			if (!costById.has(record.callId)) costById.set(record.callId, record.costUsd);
		} else {
			anonCost = (anonCost ?? 0) + record.costUsd;
		}
	}
	let costUsd = anonCost;
	for (const cost of costById.values()) costUsd = (costUsd ?? 0) + cost;
	if (totals === null && costUsd === null) return null;
	// Plain numbers for the chip (see the card's comment on `?? 0`).
	return {
		inputTokens: totals?.input ?? 0,
		cachedTokens: totals?.cacheRead ?? 0,
		cacheCreationTokens: totals?.cacheCreation ?? null,
		outputTokens: totals?.output ?? 0,
		callCount: totals?.calls ?? own.length,
		hitPct: totals?.cachedPercent ?? null,
		costUsd
	};
}

/**
 * `rows` sums every row in the store (it still supplies the cache figures,
 * which the server totals do not carry); `uncovered` sums only rows the
 * server read did not include. Without a server baseline, `rows` as-is.
 */
export function withServerTurnUsage(
	rows: TurnUsageSummary | null,
	uncovered: TurnUsageSummary | null,
	server: ServerTurnUsage | null | undefined
): TurnUsageSummary | null {
	if (!server) return rows;
	if (server.calls === 0 && !uncovered) return rows;
	const extraCost = uncovered?.costUsd ?? null;
	return {
		inputTokens: server.inputTokens + (uncovered?.inputTokens ?? 0),
		outputTokens: server.outputTokens + (uncovered?.outputTokens ?? 0),
		callCount: server.calls + (uncovered?.callCount ?? 0),
		costUsd:
			server.costUsd === null && extraCost === null
				? null
				: (server.costUsd ?? 0) + (extraCost ?? 0),
		...cacheFigures(rows, uncovered, server)
	};
}

/** Cache figures: the server's when it reported them (plus later rows), else the rows'. */
function cacheFigures(
	rows: TurnUsageSummary | null,
	uncovered: TurnUsageSummary | null,
	server: ServerTurnUsage
): Pick<TurnUsageSummary, 'cachedTokens' | 'cacheCreationTokens' | 'hitPct'> {
	const serverRead = server.cacheReadTokens ?? null;
	const serverCreation = server.cacheCreationTokens ?? null;
	if (serverRead === null && serverCreation === null) {
		return {
			cachedTokens: rows?.cachedTokens ?? 0,
			cacheCreationTokens: rows?.cacheCreationTokens ?? null,
			hitPct: rows?.hitPct ?? null
		};
	}
	const cachedTokens = (serverRead ?? 0) + (uncovered?.cachedTokens ?? 0);
	const inputTokens = server.inputTokens + (uncovered?.inputTokens ?? 0);
	return {
		cachedTokens,
		cacheCreationTokens:
			serverCreation === null && (uncovered?.cacheCreationTokens ?? null) === null
				? null
				: (serverCreation ?? 0) + (uncovered?.cacheCreationTokens ?? 0),
		hitPct: serverRead === null ? null : cachedPercentOf(cachedTokens, inputTokens)
	};
}
