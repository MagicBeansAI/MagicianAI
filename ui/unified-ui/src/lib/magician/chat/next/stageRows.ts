// Pure mappers from a host's progress rows to `TurnStage` rows. Shared by
// ChatTurnProgress and RequestActivityCard so both chips read the same way.

export type StageStatus = 'running' | 'done' | 'failed' | 'waiting';

export interface StageRow {
	key: string;
	label: string;
	status: StageStatus;
	tool?: string;
	target?: string;
}

/** Presentation-only metadata for a tool row: the tool name and a short target. */
export interface StageToolMeta {
	tool: string;
	target?: string;
}

/** Short target for a tool call's args: URL → host (no `www.`), path → basename. */
export function toolTarget(args: unknown): string | undefined {
	if (!args || typeof args !== 'object') return undefined;
	const a = args as Record<string, unknown>;
	const url = [a.url, a.href].find((v): v is string => typeof v === 'string' && v.length > 0);
	if (url) {
		try {
			return new URL(url).hostname.replace(/^www\./i, '') || undefined;
		} catch {
			return undefined;
		}
	}
	const path = [a.path, a.file_path, a.filename, a.file].find(
		(v): v is string => typeof v === 'string' && v.length > 0
	);
	if (path) return path.split(/[\\/]/).filter(Boolean).pop();
	return undefined;
}

/** Longest summary of a call's arguments a row shows. */
const ARG_SUMMARY_MAX = 80;
/** Longest tool label (name + summary, or a server `detail`) a row shows. */
const TOOL_LABEL_MAX = 100;
/** Argument keys whose value best says what a remote call does, in order. */
const SUMMARY_KEYS = ['sql', 'query', 'term', 'q', 'search', 'question', 'prompt'];
const CALL_TOOL_SUFFIX = '__call_tool';

/** One line, at most `max` chars, ellipsis when cut. */
export function clipText(text: string, max: number): string {
	const line = text.replace(/\s+/g, ' ').trim();
	return line.length > max ? `${line.slice(0, Math.max(0, max - 1)).trimEnd()}…` : line;
}

/** Short summary of an MCP `arguments_json` (a JSON string or an object):
 *  the first sql/query/term-like field, else the compact JSON, clipped. */
export function summarizeCallArgs(argumentsJson: unknown): string | undefined {
	let value: unknown = argumentsJson;
	if (typeof value === 'string') {
		const raw = value.trim();
		if (!raw) return undefined;
		try {
			value = JSON.parse(raw);
		} catch {
			return clipText(raw, ARG_SUMMARY_MAX);
		}
	}
	if (value === null || value === undefined) return undefined;
	if (typeof value === 'object' && !Array.isArray(value)) {
		const obj = value as Record<string, unknown>;
		for (const key of SUMMARY_KEYS) {
			const field = obj[key];
			if (typeof field === 'string' && field.trim()) return clipText(field, ARG_SUMMARY_MAX);
		}
		if (Object.keys(obj).length === 0) return undefined;
	}
	const compact = typeof value === 'string' ? value : JSON.stringify(value);
	return compact ? clipText(compact, ARG_SUMMARY_MAX) : undefined;
}

/** How a tool row names its call. `name` follows "Calling " / precedes
 *  " returned"; `target` is the short summary beside it. */
export interface ToolCallDisplay {
	/** The raw tool name (glyph / tint key). */
	tool: string;
	name: string;
	target?: string;
	/** `detail` = server summary; `mcp` = derived from a `<pack>__call_tool` call. */
	source: 'detail' | 'mcp' | 'plain';
}

/**
 * Presentation of one tool call. A server `detail` (e.g.
 * "metabase-mcp · execute_sql — SELECT …") wins as the name. A
 * `<pack>__call_tool` call reads "<pack> · <remote tool>" with its
 * `arguments_json` summarised as the target. Anything else keeps the tool
 * name and `toolTarget` of its args. Never longer than ~100 chars.
 */
export function describeToolCall(tool: string, args?: unknown, detail?: unknown): ToolCallDisplay {
	if (typeof detail === 'string' && detail.trim()) {
		return { tool, name: clipText(detail, TOOL_LABEL_MAX), source: 'detail' };
	}
	const a = args && typeof args === 'object' ? (args as Record<string, unknown>) : undefined;
	if (tool.endsWith(CALL_TOOL_SUFFIX) && a) {
		const pack = tool.slice(0, -CALL_TOOL_SUFFIX.length) || 'mcp';
		const remote = typeof a.name === 'string' && a.name.trim() ? a.name.trim() : 'call_tool';
		const target = summarizeCallArgs(a.arguments_json ?? a.arguments);
		return {
			tool,
			name: clipText(`${pack} · ${remote}`, TOOL_LABEL_MAX),
			...(target ? { target } : {}),
			source: 'mcp'
		};
	}
	const target = toolTarget(args);
	return { tool, name: tool, ...(target ? { target } : {}), source: 'plain' };
}

/** The tool name inside a tool row's label ("Calling X", "X returned",
 *  "X failed"), so a later event without args keeps the name the row has. */
export function toolRowName(label: string | null | undefined): string | undefined {
	if (!label) return undefined;
	const match = /^(?:\[[^\]]*\] )?(?:Calling (.+)|(.+) (?:returned|failed))$/.exec(label);
	return match ? (match[1] ?? match[2]) : undefined;
}

/**
 * Map a delegated/agentic run's TYPED action events onto the `tool.call.*`
 * vocabulary both chat hosts render: `AgenticActionStarted` → a running
 * `tool.call.started` row, `AgenticActionExecuted` → `tool.call.finished` /
 * `tool.call.failed`. The server's `call_id` (shared by a start and its
 * finish) keys the row so the finish settles the same row; an executed event
 * without one (legacy) falls back to `step_id-iteration-timestamp`. Returns
 * null for any other event.
 */
export function agenticActionAsToolEvent(
	outerType: string,
	payload: Record<string, unknown>
): { eventType: string; payload: Record<string, unknown> } | null {
	const started = outerType === 'AgenticActionStarted';
	if (!started && outerType !== 'AgenticActionExecuted') return null;
	const callId =
		typeof payload.call_id === 'string' && payload.call_id
			? payload.call_id
			: `${(payload.step_id as string) || 'step'}-${(payload.iteration as number) ?? 0}-${(payload.timestamp as number) ?? 0}`;
	const eventType = started
		? 'tool.call.started'
		: payload.success === false
			? 'tool.call.failed'
			: 'tool.call.finished';
	return {
		eventType,
		payload: {
			...payload,
			// The specific tool (`target`, e.g. "shell") over the action
			// CATEGORY (`action_type`, e.g. "pack").
			tool_name: payload.target || payload.action_type,
			call_id: callId,
			...(started ? {} : { duration_ms: payload.latency_ms })
		}
	};
}

/** Any status outside TurnStage's vocabulary (e.g. `idle`) reads as settled. */
export function toStageStatus(status: string): StageStatus {
	return status === 'running' || status === 'failed' || status === 'waiting' ? status : 'done';
}

/**
 * Map host rows to TurnStage rows, attaching `tool` / `target` from
 * `metaByKey`. `fallbackTarget` supplies a target for rows without one.
 */
export function toStageRows<R extends { key: string; label: string; status: string }>(
	rows: readonly R[],
	metaByKey: ReadonlyMap<string, StageToolMeta>,
	fallbackTarget?: (row: R) => string | undefined
): StageRow[] {
	return rows.map((row) => {
		const meta = metaByKey.get(row.key);
		const target = meta?.target ?? fallbackTarget?.(row);
		return {
			key: row.key,
			label: row.label,
			status: toStageStatus(row.status),
			...(meta ? { tool: meta.tool } : {}),
			...(target ? { target } : {})
		};
	});
}

/** How many of the latest rows the stage shows before folding the rest into `+N`. */
export const STAGE_LATEST_LIMIT = 5;

/**
 * The latest `limit` rows (rows arrive latest-last) and how many earlier rows
 * were left out. A run with hundreds of steps keeps the card one line tall;
 * Inspect shows every step live.
 */
export function latestRows<T>(
	rows: readonly T[],
	limit: number = STAGE_LATEST_LIMIT
): { shown: T[]; hidden: number } {
	const hidden = Math.max(0, rows.length - Math.max(0, limit));
	return { shown: rows.slice(hidden), hidden };
}
