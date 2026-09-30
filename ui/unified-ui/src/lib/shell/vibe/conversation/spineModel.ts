/**
 * VibeDev conversation-spine model.
 *
 * Pure, framework-free reducer that turns the live `coding.*` NDJSON event
 * stream (the F2 projection emitted by `run_coding_task::emit_pi_event`) into
 * an ordered list of typed `SpineCard`s — the conversation spine the cockpit
 * renders. This replaces the monolith's 12-row / 180-char scrape
 * (`+page.svelte` `ingestCodingLine`): NO row cap, NO detail trim — the view
 * windows the list, the data keeps everything.
 *
 * The event catalog this maps is documented in
 * `docs/components/magician/vibedev-cockpit-blueprint.md` §3 and grounded in
 * `docs/components/magician/pi-coding-engine-contract.md`. Tool results/args
 * arrive ALREADY secret-redacted from the backend; we never re-redact, we only
 * choose how much to render.
 */

export type SpineCardKind =
	| 'run_header'
	| 'reasoning'
	| 'message'
	| 'action'
	| 'diff'
	| 'test'
	| 'error'
	| 'plan'
	| 'completed';

export type SpineCardStatus = 'running' | 'waiting' | 'done' | 'failed' | 'info';

export interface SpineCard {
	/** Stable identity used as the keyed-each key and for upsert. */
	id: string;
	kind: SpineCardKind;
	/** The run's task id (used for client-side run-chain filtering). */
	taskId: string | null;
	/** Pi shadow-workspace id — groups a single run's events. */
	shadowId: string;
	/** Turn index within the run (0-based; bumped on `coding.turn.started`). */
	turn: number;
	/** First-seen intra-run sequence (stable sort within a run). */
	sequence: number;
	/** Monotonic insertion order — stable tiebreak for equal timestamps. */
	order: number;
	/** First-seen wall-clock ms (cards never jump once placed). Sort key. */
	ts: number;
	/** True earliest / latest event ms touching this card (min/max, so robust to
	 *  out-of-order desc backfill) — drives the expanded timestamp + latency. */
	startTs?: number;
	endTs?: number;
	status: SpineCardStatus;
	title: string;
	/** Accumulated text (message/reasoning) or one-line summary. */
	detail: string | null;
	toolName?: string | null;
	toolCallId?: string | null;
	/** Pre-redacted tool args (JSON-ish string), rendered collapsed. */
	args?: string | null;
	/** Pre-redacted tool result, rendered collapsed. */
	result?: string | null;
	isError?: boolean;
	exitStatus?: string | null;
	/** Diff cards: the HITL proposal id the authoritative diff hydrates from. */
	proposalId?: string | null;
	fileCount?: number | null;
	touchedFiles?: string[];
	profileLabel?: string | null;
	/** Run header: the user's prompt preview + repo. `promptFull` carries the
	 *  whole task prompt (capped server-side) so the spine can offer a "Show
	 *  more" expander past the 2-line preview. */
	promptPreview?: string | null;
	promptFull?: string | null;
	repoPath?: string | null;
	/** Internal (not rendered): streaming message/reasoning delta fragments keyed
	 *  by event `sequence`. Lets `detail` be rebuilt in sequence order regardless
	 *  of arrival order (desc backfill / out-of-order hydrate) and makes folding
	 *  idempotent (re-applying the same event can't duplicate text). */
	detailFrags?: Map<number, string>;
}

export interface RunMeta {
	costTotal: number | null;
	contextPercent: number | null;
	model: string | null;
	thinkingLevel: string | null;
	tokens: number | null;
	toolCalls: number | null;
	totalMessages: number | null;
	retry: { attempt: number; max: number; active: boolean; lastError?: string | null } | null;
	queue: { steering: number; followUp: number } | null;
	compacting: boolean;
	/** Latest stop_reason seen (turn/message finished). */
	stopReason: string | null;
	/** True once a terminal event (completed / hard failure) lands. */
	terminal: boolean;
	/** Active coding time this task has spent, and — only when a whole-task
	 *  ceiling is configured — how much is left. Present so a long run reads as
	 *  *working* rather than stuck.
	 *
	 *  `remainingSecs` is null when no ceiling is set, which is the default.
	 *  Reporting zero there would render an unbounded run as out of budget. */
	budget: { spentSecs: number; remainingSecs: number | null } | null;
	updatedAt: number;
}

export interface SpineState {
	cards: Map<string, SpineCard>;
	meta: Map<string, RunMeta>;
	/** Current turn index per shadow workspace. */
	turnByShadow: Map<string, number>;
	order: number;
}

export function emptySpineState(): SpineState {
	return {
		cards: new Map(),
		meta: new Map(),
		turnByShadow: new Map(),
		order: 0
	};
}

function emptyMeta(now: number): RunMeta {
	return {
		costTotal: null,
		contextPercent: null,
		model: null,
		thinkingLevel: null,
		tokens: null,
		toolCalls: null,
		totalMessages: null,
		retry: null,
		queue: null,
		compacting: false,
		stopReason: null,
		terminal: false,
		budget: null,
		updatedAt: now
	};
}

/** A coding event after unwrapping the AgentEvent envelope. */
export interface CodingEvent {
	eventType: string;
	payload: Record<string, unknown>;
	ts: number;
}

/**
 * Unwrap an NDJSON line (already JSON-parsed) into a coding event, or null if
 * it isn't a `coding.*` event. Mirrors the monolith's `unwrapCodingEvent` +
 * `extractEventTimestamp` (verbatim semantics) so both surfaces agree.
 */
export function unwrapCodingEvent(parsed: Record<string, unknown>): CodingEvent | null {
	const outerType = String(parsed.event_type ?? '');
	const data = parsed.data as Record<string, unknown> | undefined;
	let eventType = outerType;
	let payload: Record<string, unknown> = data ?? {};
	if (outerType === 'AgentEvent' && data) {
		const inner = data.event as Record<string, unknown> | undefined;
		if (!inner || typeof inner.event_type !== 'string') return null;
		eventType = inner.event_type;
		payload = (inner.payload as Record<string, unknown>) ?? {};
	}
	if (!eventType.startsWith('coding.')) return null;
	return { eventType, payload, ts: extractEventTimestamp(parsed, payload) };
}

function extractEventTimestamp(
	parsed: Record<string, unknown>,
	payload: Record<string, unknown>
): number {
	const candidates = [
		parsed.timestamp_ms,
		(parsed.data as Record<string, unknown> | undefined)?.timestamp_ms,
		payload.timestamp_ms
	];
	for (const candidate of candidates) {
		if (typeof candidate === 'number' && Number.isFinite(candidate)) return candidate;
	}
	return Date.now();
}

// ── small payload readers ───────────────────────────────────────────────────
function str(value: unknown): string | null {
	return typeof value === 'string' && value.length > 0 ? value : null;
}
function num(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}
function profileLabel(payload: Record<string, unknown>): string | null {
	const profile = payload.coding_profile as Record<string, unknown> | undefined;
	return str(profile?.label) ?? str(profile?.id);
}
function profileModel(payload: Record<string, unknown>): string | null {
	const profile = payload.coding_profile as Record<string, unknown> | undefined;
	return str(profile?.model);
}

/**
 * Tool-name → spine card kind. Edit/write tools still render as action cards;
 * the authoritative diff comes from `coding.approval_requested` (a proposal),
 * never from a truncated tool result. Test-runner tools get a red→green card.
 */
const TEST_TOOL_RE = /\b(test|vitest|jest|pytest|playwright|cargo[\s_-]*test|go[\s_-]*test|rspec)\b/i;

/**
 * Head of the shell command inside a tool's args JSON: the `command` string
 * field's first few whitespace tokens, with path-like tokens (containing `/`)
 * dropped. The test-tool / check-runner classifiers key off the program being
 * INVOKED — matching the full args JSON false-flags calls whose file ARGUMENTS
 * merely mention a keyword (`cat src/tests/x.ts` is not a test run; `\b`
 * matches at `/`, so path segments would hit `\btests?\b`). Dropping path
 * tokens keeps runner-by-path invocations (`./gradlew test`) classifiable by
 * their verb. Returns null when the args carry no parseable `command` string
 * (non-shell tools / truncated JSON) — callers fall back to full-args matching.
 */
function commandHead(args: string | null): string | null {
	if (!args) return null;
	let command: unknown;
	try {
		command = (JSON.parse(args) as Record<string, unknown> | null)?.command;
	} catch {
		return null;
	}
	if (typeof command !== 'string') return null;
	return command
		.trim()
		.split(/\s+/)
		.filter((token) => !token.includes('/'))
		.slice(0, 3)
		.join(' ');
}

function isTestTool(toolName: string | null, args: string | null): boolean {
	if (!toolName) return false;
	if (TEST_TOOL_RE.test(toolName)) return true;
	// `bash`/`shell` running a test command — sniff the command HEAD, not the
	// whole args JSON: a file argument like `cat src/tests/x.ts` must not
	// classify the call as a test run. No `command` field → full-args fallback.
	if (/^(bash|shell|run_command|execute)/i.test(toolName) && args) {
		const head = commandHead(args);
		return TEST_TOOL_RE.test(head ?? args);
	}
	return false;
}

function valueToText(value: unknown): string | null {
	if (value == null) return null;
	if (typeof value === 'string') return value;
	try {
		return JSON.stringify(value);
	} catch {
		return String(value);
	}
}

function metaFor(state: SpineState, shadowId: string, now: number): RunMeta {
	let meta = state.meta.get(shadowId);
	if (!meta) {
		meta = emptyMeta(now);
		state.meta.set(shadowId, meta);
	}
	meta.updatedAt = now;
	return meta;
}

function applyUsage(meta: RunMeta, usage: unknown): void {
	if (!usage || typeof usage !== 'object') return;
	const u = usage as Record<string, unknown>;
	// `turn.finished` / `message.finished` carry snake_case usage
	// (`total_tokens`); `coding.stats.tokens` is camelCase (`total`).
	const total = num(u.total_tokens) ?? num(u.total) ?? num(u.totalTokens);
	if (total !== null) meta.tokens = total;
}

/**
 * Accumulate a streaming message/reasoning `delta` into a card's `detail`, keyed
 * by event `sequence`. Rebuilds `detail` in sequence order so the text is correct
 * regardless of ARRIVAL order — the cross-scope backfill streams events
 * newest-first (desc) and refresh-hydrate folds them out of order, so appending
 * in arrival order scrambled the prose. Keying by sequence also makes folding
 * idempotent: re-applying the same event (e.g. backfill overlapping the live
 * tail) overwrites rather than duplicates.
 */
function accumulateDeltaBySequence(card: SpineCard, sequence: number, delta: string): void {
	const frags = (card.detailFrags ??= new Map<number, string>());
	frags.set(sequence, delta);
	if (frags.size === 1) {
		card.detail = delta;
		return;
	}
	card.detail = [...frags.entries()]
		.sort((a, b) => a[0] - b[0])
		.map((entry) => entry[1])
		.join('');
}

/**
 * Fold one coding event into the spine state. Returns true if anything changed.
 * Idempotent on duplicates (upsert by stable card id; message / reasoning deltas
 * accumulate by `sequence`, so order-independent + dup-safe).
 */
/** Fold the backend's budget telemetry onto the run so the header can show
 *  headroom while the run is still going. Silently ignores a payload without
 *  it — older durable events predate the field. */
function applyBudget(meta: RunMeta, raw: unknown): void {
	const budget = raw as Record<string, unknown> | undefined;
	const spent = num(budget?.task_active_spent_secs);
	if (spent === null) return;
	// `remaining` stays null when no ceiling is configured — the default.
	// Coercing it to zero would render an unbounded run as out of budget.
	meta.budget = { spentSecs: spent, remainingSecs: num(budget?.task_active_remaining_secs) };
}

/** Name the bound that actually fired. A silent model, a hung test and a wedged
 *  compaction are three different diagnoses; collapsing them into "Failed" is
 *  what the typed cause exists to prevent. */
function budgetStopTitle(cause: string | null | undefined): string {
	switch (cause) {
		case 'turn_timeout':
			return 'Stopped — turn ran out of time';
		case 'task_budget':
			return 'Stopped — task budget spent';
		case 'no_progress':
			return 'Stopped — no progress';
		default:
			return 'Stopped';
	}
}

export function applyCodingEvent(state: SpineState, event: CodingEvent): boolean {
	const { eventType, payload, ts } = event;
	const shadowId = str(payload.shadow_workspace_id) ?? 'coding';
	const taskId = str(payload.task_id);
	const sequence = num(payload.sequence) ?? 0;
	const label = profileLabel(payload);

	const turn = state.turnByShadow.get(shadowId) ?? 0;

	const upsert = (id: string, build: () => SpineCard, patch?: (card: SpineCard) => void): boolean => {
		const existing = state.cards.get(id);
		if (existing) {
			if (patch) patch(existing);
			// keep taskId fresh if it arrives later
			if (!existing.taskId && taskId) existing.taskId = taskId;
			// widen the card's true [start, end] span — Math.min/max so an
			// out-of-order (desc backfill) event can't invert start/end.
			existing.startTs = Math.min(existing.startTs ?? existing.ts, ts);
			existing.endTs = Math.max(existing.endTs ?? existing.ts, ts);
			return true;
		}
		const card = build();
		state.cards.set(id, card);
		state.order += 1;
		card.order = state.order;
		return true;
	};

	const baseCard = (kind: SpineCardKind, id: string, overrides: Partial<SpineCard>): SpineCard => ({
		id,
		kind,
		taskId,
		shadowId,
		turn,
		sequence,
		order: state.order,
		ts,
		startTs: ts,
		endTs: ts,
		status: 'info',
		title: '',
		detail: null,
		profileLabel: label,
		...overrides
	});

	switch (eventType) {
		case 'coding.started': {
			const m = metaFor(state, shadowId, ts);
			if (label) m.model = profileModel(payload) ?? m.model;
			return upsert(`${shadowId}::header`, () =>
				baseCard('run_header', `${shadowId}::header`, {
					status: 'running',
					title: label ? `Building with ${label}` : 'Building',
					promptPreview: str(payload.prompt_preview),
					promptFull: str(payload.prompt_full) ?? str(payload.prompt_preview),
					repoPath: str(payload.repo_path),
					detail: str(payload.prompt_preview)
				})
			);
		}

		case 'coding.turn.started': {
			state.turnByShadow.set(shadowId, turn + 1);
			return false;
		}

		case 'coding.turn.finished': {
			const m = metaFor(state, shadowId, ts);
			applyUsage(m, payload.usage);
			const cost = num(payload.cost_total);
			if (cost !== null) m.costTotal = cost;
			m.stopReason = str(payload.stop_reason) ?? m.stopReason;
			return true;
		}

		case 'coding.thinking': {
			const delta = str(payload.delta);
			if (!delta) return false;
			const id = `${shadowId}::t${turn}::reasoning`;
			return upsert(
				id,
				() => {
					const card = baseCard('reasoning', id, { status: 'running', title: 'Thinking' });
					accumulateDeltaBySequence(card, sequence, delta);
					return card;
				},
				(card) => {
					accumulateDeltaBySequence(card, sequence, delta);
				}
			);
		}

		case 'coding.message': {
			const delta = str(payload.delta);
			if (!delta) return false;
			const id = `${shadowId}::t${turn}::message`;
			return upsert(
				id,
				() => {
					const card = baseCard('message', id, { status: 'running', title: 'Assistant' });
					accumulateDeltaBySequence(card, sequence, delta);
					return card;
				},
				(card) => {
					accumulateDeltaBySequence(card, sequence, delta);
				}
			);
		}

		case 'coding.message.finished': {
			const m = metaFor(state, shadowId, ts);
			applyUsage(m, payload.usage);
			const cost = num(payload.cost_total);
			if (cost !== null) m.costTotal = cost;
			const stop = str(payload.stop_reason);
			m.stopReason = stop ?? m.stopReason;
			const id = `${shadowId}::t${turn}::message`;
			const card = state.cards.get(id);
			if (card) {
				card.status = stop === 'error' ? 'failed' : 'done';
				if (stop === 'error') {
					card.isError = true;
					card.detail = `${card.detail ?? ''}${str(payload.error_message) ? `\n${str(payload.error_message)}` : ''}`;
				}
				return true;
			}
			return true;
		}

		case 'coding.tool.started': {
			const toolName = str(payload.tool_name);
			const toolCallId = str(payload.tool_call_id) ?? `${shadowId}:${sequence}`;
			const args = valueToText(payload.args);
			const kind: SpineCardKind = isTestTool(toolName, args) ? 'test' : 'action';
			const id = `${shadowId}::tool::${toolCallId}`;
			return upsert(id, () =>
				baseCard(kind, id, {
					status: 'running',
					title: toolName ?? 'Tool',
					toolName,
					toolCallId,
					args
				})
			);
		}

		case 'coding.tool.progress': {
			const toolCallId = str(payload.tool_call_id) ?? `${shadowId}:${sequence}`;
			const id = `${shadowId}::tool::${toolCallId}`;
			const card = state.cards.get(id);
			if (card) {
				card.status = 'running';
				return true;
			}
			return false;
		}

		case 'coding.tool.finished': {
			const toolName = str(payload.tool_name);
			const toolCallId = str(payload.tool_call_id) ?? `${shadowId}:${sequence}`;
			const result = valueToText(payload.result);
			const isError = payload.is_error === true;
			const id = `${shadowId}::tool::${toolCallId}`;
			const m = metaFor(state, shadowId, ts);
			m.toolCalls = (m.toolCalls ?? 0) + 1;
			return upsert(
				id,
				() =>
					baseCard(isTestTool(toolName, result) ? 'test' : 'action', id, {
						status: isError ? 'failed' : 'done',
						title: toolName ?? 'Tool',
						toolName,
						toolCallId,
						result,
						isError
					}),
				(card) => {
					card.result = result ?? card.result;
					card.isError = isError;
					card.status = isError ? 'failed' : 'done';
				}
			);
		}

		case 'coding.approval_requested': {
			const proposalId = str(payload.proposal_id);
			const id = `${shadowId}::diff::${proposalId ?? sequence}`;
			const touched = Array.isArray(payload.touched_files)
				? (payload.touched_files as unknown[]).filter((x): x is string => typeof x === 'string')
				: undefined;
			// Touch the run meta (keeps its updatedAt fresh) but do NOT pin terminal=false:
			// an approval is a waiting state, not proof the run is still live. Pinning it false
			// made a later task-level failure (which emits no spine terminal event) show
			// "Building" forever — terminality is driven by terminal coding events / task status.
			metaFor(state, shadowId, ts);
			return upsert(id, () =>
				baseCard('diff', id, {
					status: 'waiting',
					title: 'Changes ready for review',
					proposalId,
					fileCount: num(payload.file_count),
					touchedFiles: touched
				})
			);
		}

		case 'coding.completed': {
			const m = metaFor(state, shadowId, ts);
			m.terminal = true;
			applyBudget(m, payload.budget);
			const pendingApproval = payload.pending_approval === true;
			const noChange = payload.no_change === true;
			const planRun = payload.plan_run === true;
			const id = `${shadowId}::completed`;
			// A plan run's outcome is a PLAN, not a diff / "no changes" — render it as
			// its own card kind (Phase 5) so the chain reads as plan → … → build.
			if (planRun) {
				return upsert(id, () =>
					baseCard('plan', id, {
						status: 'done',
						title: 'Plan ready',
						detail: str(payload.assistant_text)
					})
				);
			}
			return upsert(id, () =>
				baseCard('completed', id, {
					status: pendingApproval ? 'waiting' : 'done',
					title: pendingApproval
						? 'Finished — review pending'
						: noChange
							? 'Finished — no changes'
							: 'Finished',
					detail: str(payload.assistant_text),
					proposalId: str(payload.proposal_id)
				})
			);
		}

		case 'coding.failed': {
			const stage = str(payload.stage) ?? 'unknown';
			// Hard, terminal stages vs a soft pi_message failure the run recovers from.
			const terminalStage = /^(prepare|resolve|materialize|credential|run_turn)/.test(stage);
			const m = metaFor(state, shadowId, ts);
			if (terminalStage) m.terminal = true;
			applyBudget(m, payload.budget);
			const id = `${shadowId}::error::${sequence}`;
			// A budget stop is not a coding failure. Reporting the two the same
			// way is exactly what made "ran out of clock mid-thought"
			// indistinguishable from "the model was wrong".
			const termination = payload.termination as Record<string, unknown> | undefined;
			if (payload.budget_stop === true) {
				return upsert(id, () =>
					baseCard('error', id, {
						status: 'failed',
						title: budgetStopTitle(str(termination?.cause)),
						detail: str(payload.error) ?? 'The run stopped before it finished',
						isError: false
					})
				);
			}
			return upsert(id, () =>
				baseCard('error', id, {
					status: 'failed',
					title: `Failed (${stage})`,
					detail: str(payload.error) ?? 'Coding run failed',
					isError: true
				})
			);
		}

		case 'coding.budget_exhausted': {
			const m = metaFor(state, shadowId, ts);
			m.terminal = true;
			applyBudget(m, payload.budget);
			const id = `${shadowId}::budget::${sequence}`;
			return upsert(id, () =>
				baseCard('error', id, {
					status: 'failed',
					title: 'Out of task budget',
					detail: 'The task used its whole active-time budget before this turn could start',
					isError: false
				})
			);
		}

		case 'coding.stats': {
			const m = metaFor(state, shadowId, ts);
			const cost = num(payload.cost);
			if (cost !== null) m.costTotal = cost;
			const tokens = payload.tokens as Record<string, unknown> | undefined;
			const tokTotal = num(tokens?.total) ?? num(tokens?.total_tokens);
			if (tokTotal !== null) m.tokens = tokTotal;
			const ctx = payload.context_usage as Record<string, unknown> | undefined;
			const pct = num(ctx?.percent);
			if (pct !== null) m.contextPercent = pct;
			m.toolCalls = num(payload.tool_calls) ?? m.toolCalls;
			m.totalMessages = num(payload.total_messages) ?? m.totalMessages;
			return true;
		}

		case 'coding.queue': {
			const m = metaFor(state, shadowId, ts);
			m.queue = {
				steering: num(payload.steering_len) ?? 0,
				followUp: num(payload.follow_up_len) ?? 0
			};
			return true;
		}

		case 'coding.compaction.started': {
			metaFor(state, shadowId, ts).compacting = true;
			return true;
		}
		case 'coding.compaction.finished': {
			metaFor(state, shadowId, ts).compacting = false;
			return true;
		}

		case 'coding.retry.started': {
			const m = metaFor(state, shadowId, ts);
			m.retry = {
				attempt: num(payload.attempt) ?? 1,
				max: num(payload.max_attempts) ?? 0,
				active: true
			};
			return true;
		}
		case 'coding.retry.finished': {
			const m = metaFor(state, shadowId, ts);
			if (m.retry) {
				m.retry = { ...m.retry, active: false, lastError: str(payload.final_error) };
			}
			return true;
		}

		default:
			// agent_started/ended, attachments_materialized, etc. — no card today.
			return false;
	}
}

/** Snapshot the cards as a stable, chronologically-ordered list. */
export function spineCardsList(state: SpineState): SpineCard[] {
	return Array.from(state.cards.values()).sort((a, b) => {
		if (a.ts !== b.ts) return a.ts - b.ts;
		return a.order - b.order;
	});
}

/** Client-side run-chain filter: keep cards whose task is in the chain. An EMPTY
 *  chain (no run selected, or the active task not yet loaded) returns NO cards.
 *  The spine store is SCOPE-wide — it folds every run's `coding.*` events — so
 *  returning all cards on an empty chain leaked OTHER projects'/runs' streams
 *  into the no-run-selected view (and drove `runMeta`/`runLive`/hydrate off the
 *  wrong run). "No run selected" must show nothing, not everything. */
export function selectCardsForRun(cards: SpineCard[], chainIds: Set<string>): SpineCard[] {
	if (chainIds.size === 0) return [];
	return cards.filter((card) => (card.taskId ? chainIds.has(card.taskId) : false));
}

/** Merge the per-shadow meta for every shadow that belongs to a run-chain. */
export function runMetaForChain(state: SpineState, chainIds: Set<string>): RunMeta | null {
	let latest: RunMeta | null = null;
	for (const card of state.cards.values()) {
		if (chainIds.size > 0 && !(card.taskId && chainIds.has(card.taskId))) continue;
		const meta = state.meta.get(card.shadowId);
		if (!meta) continue;
		if (!latest || meta.updatedAt >= latest.updatedAt) latest = meta;
	}
	return latest;
}

/** Stuck-detection window: only the last N events count. Counting across the
 *  ENTIRE run false-flagged healthy long runs (any tool legitimately repeated
 *  3× hours apart read as "stuck"). */
export const STUCK_WINDOW_EVENTS = 12;
/** Stuck-detection window: only events within this span of the NEWEST card's
 *  timestamp count (measured card-to-card, so the check stays pure — no clock
 *  injection). */
export const STUCK_WINDOW_MS = 3 * 60 * 1000;

/** Check-runner shell commands (test / check / build / lint / format) — these
 *  are EXEMPT from stuck detection: re-running the same check is the healthy
 *  red→green loop (edit → run tests ×3 is progress, not a stall). */
const CHECK_RUNNER_RE =
	/\b(tests?|checks?|builds?|lint|linting|typecheck|type-check|compile|fmt|format|clippy|tsc|eslint|prettier|svelte-check)\b/i;

/** True for cards that represent a check-runner invocation: red→green `test`
 *  cards, test-tool calls, and shell calls whose command matches the
 *  check/build/lint patterns. */
function isCheckRunnerCall(card: SpineCard): boolean {
	if (card.kind === 'test') return true;
	if (isTestTool(card.toolName ?? null, card.args ?? null)) return true;
	if (!card.toolName || !SHELL_TOOL_RE.test(card.toolName) || !card.args) return false;
	// Same head scoping as `isTestTool`: exempt on the command being INVOKED,
	// not on check-ish words anywhere in the args (paths, prose, flags).
	const head = commandHead(card.args);
	return CHECK_RUNNER_RE.test(head ?? card.args);
}

/**
 * Detect a stuck run: the same tool args repeated ≥ `threshold` times WITHIN
 * the recent window — the last `STUCK_WINDOW_EVENTS` events AND within
 * `STUCK_WINDOW_MS` of the newest event's own timestamp. Check-runner calls
 * (`isCheckRunnerCall`) never count: a red→green loop re-running `npm test`
 * is healthy iteration, not a stall. Pure: time comes from the cards.
 */
export function detectStuck(cards: SpineCard[], threshold = 3): { toolName: string; args: string } | null {
	if (cards.length === 0) return null;
	const recent = cards.slice(-STUCK_WINDOW_EVENTS);
	const newestTs = recent.reduce((max, card) => (card.ts > max ? card.ts : max), 0);
	const counts = new Map<string, { count: number; toolName: string; args: string }>();
	for (const card of recent) {
		if (card.kind !== 'action' && card.kind !== 'test') continue;
		if (!card.args) continue;
		if (newestTs - card.ts > STUCK_WINDOW_MS) continue;
		if (isCheckRunnerCall(card)) continue;
		const key = `${card.toolName ?? ''}::${card.args}`;
		const entry = counts.get(key) ?? { count: 0, toolName: card.toolName ?? '', args: card.args };
		entry.count += 1;
		counts.set(key, entry);
	}
	for (const entry of counts.values()) {
		if (entry.count >= threshold) return { toolName: entry.toolName, args: entry.args };
	}
	return null;
}

// ── collapsed one-line preview ──────────────────────────────────────────────

/** Flatten internal whitespace and clip to `max` chars with a trailing ellipsis,
 *  so a multi-line command / prose block reads as one tidy preview line. */
function clip(text: string, max: number): string {
	const flat = text.replace(/\s+/g, ' ').trim();
	if (flat.length <= max) return flat;
	return `${flat.slice(0, max - 1).trimEnd()}…`;
}

/** Defensive parse of a tool's pre-redacted args string into a plain object.
 *  Returns null for non-JSON, arrays or non-objects — never throws. */
function parseArgsObject(args: string | null | undefined): Record<string, unknown> | null {
	if (!args) return null;
	try {
		const parsed = JSON.parse(args) as unknown;
		return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
			? (parsed as Record<string, unknown>)
			: null;
	} catch {
		return null;
	}
}

/** First non-empty scalar value in an args object — the fallback summary for
 *  tools we don't special-case. */
function firstScalarArg(obj: Record<string, unknown>): string | null {
	for (const value of Object.values(obj)) {
		if (typeof value === 'string' && value.trim().length > 0) return value;
		if (typeof value === 'number' || typeof value === 'boolean') return String(value);
	}
	return null;
}

/**
 * One-line, collapsed preview of a card's content so event/log rows aren't blank
 * before the user expands them. PURE + defensive: tolerates non-JSON args,
 * missing fields and null detail, and never throws. Returns '' when there is
 * nothing useful to show (the caller then renders no preview line).
 *
 *   reasoning ('Thinking')   → first ~120 chars of `detail`
 *   bash / shell / exec      → the `command` arg (~100 chars)
 *   read / view / delete     → the `path` / `file_path` arg
 *   edit / write             → the file path (+ a snippet of the new content)
 *   grep / search            → the `pattern` / `query` arg
 *   other tools              → first scalar arg, else the head of `result`
 */
export function cardPreview(card: SpineCard): string {
	if (card.kind === 'reasoning') {
		return card.detail ? clip(card.detail, 120) : '';
	}
	if (card.kind !== 'action' && card.kind !== 'test') return '';

	const name = (card.toolName ?? '').toLowerCase();
	const obj = parseArgsObject(card.args);

	if (obj) {
		const path = str(obj.file_path) ?? str(obj.path) ?? str(obj.filename) ?? str(obj.file);
		const command = str(obj.command) ?? str(obj.cmd) ?? str(obj.script);
		const query = str(obj.pattern) ?? str(obj.query) ?? str(obj.q) ?? str(obj.search);

		// edit / write — path plus a short snippet of the new content if present.
		if (/edit|write|patch|apply|create/.test(name) && path) {
			const snippet = str(obj.content) ?? str(obj.new_string) ?? str(obj.text);
			return snippet ? clip(`${path} — ${snippet}`, 100) : clip(path, 100);
		}
		// read / view / delete — the file path.
		if (/read|cat|view|open|delete|rm|remove/.test(name) && path) return clip(path, 100);
		// grep / search — the pattern.
		if (/grep|search|find|ripgrep|rg/.test(name) && query) return clip(query, 100);
		// bash / shell / test runners — the command line.
		if (command) return clip(command, 100);

		// generic: the most descriptive known key, else the first scalar arg.
		const summary = path ?? query ?? str(obj.url) ?? firstScalarArg(obj);
		if (summary) return clip(summary, 100);
	}

	// No parseable args object — fall back to the (pre-redacted) result head.
	if (card.result) return clip(card.result, 100);
	if (card.args) return clip(card.args, 100);
	return '';
}

// ── turn grouping + action strips ───────────────────────────────────────────
// PURE derive-on-read layer over an already-ordered card list. The event fold
// above is untouched: grouping is recomputed from the (windowed) card slice the
// view renders — grouping thousands of cards on every store publish would be
// wasted work, so callers pass the visible slice only.

/**
 * Card kinds that fold into an action strip. ONLY plain `action` (tool call)
 * cards count: `test` (red→green), `diff`, `error`, `plan`, `reasoning`,
 * `message`, `run_header` and `completed` are the legibility-bearing kinds —
 * they always break a strip and render individually.
 */
export function isStripAction(card: SpineCard): boolean {
	return card.kind === 'action';
}

/** Minimum run of consecutive action cards that folds into a strip. */
export const STRIP_MIN_RUN = 3;

/** Digest length cap for `stripSummary` (excluding the trailing `…` token's
 *  join overhead — the rendered digest can run at most ~3 chars over). */
export const STRIP_SUMMARY_MAX = 72;
const STRIP_TOKEN_MAX = 28;
const SHELL_TOOL_RE = /^(bash|shell|run_command|execute|exec|terminal)/i;

export interface ActionStrip {
	kind: 'action-strip';
	/** Deterministic id derived from the FIRST member card's id — appending more
	 *  actions to a growing run widens the strip WITHOUT changing its identity,
	 *  so per-strip expansion state survives re-derivation and the keyed each
	 *  doesn't churn (no flicker on incremental appends). The view keeps the
	 *  render window from bisecting a run (`snapWindowStart`), so the first
	 *  member — and therefore this id — is also stable as the window slides;
	 *  the sole accepted churn edge is a capped mega-run (see `snapWindowStart`). */
	id: string;
	count: number;
	/** Compact, order-preserving digest — repeated consecutive tool names
	 *  coalesce to `name ×N`; single shell calls show their command. */
	summary: string;
	cards: SpineCard[];
}

export interface TurnGroupCard {
	kind: 'card';
	card: SpineCard;
}

export type TurnGroupItem = ActionStrip | TurnGroupCard;

export interface TurnGroup {
	/** Position-independent id (`turn::<shadowId>::<turnNumber>`). A run's turn
	 *  numbers never repeat, so the id is deterministic regardless of WHERE the
	 *  render window starts — a first-card-derived id churned whenever the
	 *  window boundary sliced into the group. */
	id: string;
	shadowId: string;
	turn: number;
	/** "Turn N" for runs that actually have multiple numbered turns; null when
	 *  a header would be noise (single-turn runs, the turn-0 run-header group). */
	label: string | null;
	items: TurnGroupItem[];
}

/**
 * Order-preserving digest of a strip's member cards, e.g.
 * `npm test, read ×3, edit ×2`. Consecutive repeats of the same tool coalesce
 * with ×N; a single shell-ish call shows its command (via `cardPreview`)
 * instead of the bare tool name. Total length capped (`…` when truncated).
 */
export function stripSummary(cards: SpineCard[], max = STRIP_SUMMARY_MAX): string {
	interface Token {
		name: string;
		first: SpineCard;
		count: number;
	}
	const tokens: Token[] = [];
	for (const card of cards) {
		const name = card.toolName ?? (card.title || 'tool');
		const last = tokens[tokens.length - 1];
		if (last && last.name === name) last.count += 1;
		else tokens.push({ name, first: card, count: 1 });
	}
	const parts: string[] = [];
	let used = 0;
	for (const token of tokens) {
		let label = token.name;
		if (token.count === 1 && SHELL_TOOL_RE.test(token.name)) {
			const preview = cardPreview(token.first);
			if (preview) label = preview;
		}
		label = clip(label, STRIP_TOKEN_MAX);
		const part = token.count > 1 ? `${label} ×${token.count}` : label;
		const sep = parts.length > 0 ? 2 : 0;
		if (parts.length > 0 && used + sep + part.length > max) {
			parts.push('…');
			break;
		}
		parts.push(part);
		used += sep + part.length;
	}
	return parts.join(', ');
}

/**
 * Group an ordered card list into turn sections, folding runs of ≥
 * `STRIP_MIN_RUN` CONSECUTIVE tool-action cards into compact action strips.
 *
 * Turn boundary = a change of `(shadowId, turn)` between consecutive cards.
 * The run_header card lands in turn 0 (it's created before the first
 * `coding.turn.started` bump), so it anchors its own group at the top of each
 * run; numbered turns follow. Assistant messages/diffs/tests/errors/plans/
 * thinking never fold — they break strips and render individually.
 *
 * Live-tail rule: while the run is live (`opts.live`), the very LAST card of
 * the list — the currently-streaming/latest action — never folds into a strip.
 * Only COMPLETED runs of actions fold; the preceding actions may still fold if
 * ≥ `STRIP_MIN_RUN` remain. Live legibility improves without hiding activity.
 *
 * Pure + deterministic: same input prefix → identical group/strip ids (group
 * ids derive from `(shadowId, turn)` — position-independent — and strip ids
 * from their first member card id), so expansion state keyed on them survives
 * re-derivation across incremental appends AND window-start slides.
 */
export function groupCards(cards: SpineCard[], opts: { live?: boolean } = {}): TurnGroup[] {
	const live = opts.live === true;
	const lastCardId = cards.length > 0 ? cards[cards.length - 1].id : null;
	const groups: TurnGroup[] = [];

	let current: TurnGroup | null = null;
	let buffer: SpineCard[] = [];

	const flushBuffer = () => {
		if (!current || buffer.length === 0) return;
		let run = buffer;
		let liveTail: SpineCard | null = null;
		if (live && lastCardId !== null && run[run.length - 1].id === lastCardId) {
			liveTail = run[run.length - 1];
			run = run.slice(0, -1);
		}
		if (run.length >= STRIP_MIN_RUN) {
			current.items.push({
				kind: 'action-strip',
				id: `strip::${run[0].id}`,
				count: run.length,
				summary: stripSummary(run),
				cards: run
			});
		} else {
			for (const card of run) current.items.push({ kind: 'card', card });
		}
		if (liveTail) current.items.push({ kind: 'card', card: liveTail });
		buffer = [];
	};

	for (const card of cards) {
		if (!current || current.shadowId !== card.shadowId || current.turn !== card.turn) {
			flushBuffer();
			current = {
				id: `turn::${card.shadowId}::${card.turn}`,
				shadowId: card.shadowId,
				turn: card.turn,
				label: null,
				items: []
			};
			groups.push(current);
		}
		if (isStripAction(card)) {
			buffer.push(card);
		} else {
			flushBuffer();
			current.items.push({ kind: 'card', card });
		}
	}
	flushBuffer();

	// "Turn N" section headers only where they carry signal: runs that actually
	// span multiple numbered turns. A single-turn run stays header-free.
	const numberedTurns = new Map<string, number>();
	for (const group of groups) {
		if (group.turn >= 1) {
			numberedTurns.set(group.shadowId, (numberedTurns.get(group.shadowId) ?? 0) + 1);
		}
	}
	for (const group of groups) {
		if (group.turn >= 1 && (numberedTurns.get(group.shadowId) ?? 0) > 1) {
			group.label = `Turn ${group.turn}`;
		}
	}
	return groups;
}

/** Mega-run guard for `snapWindowStart`: the max cards a snap may hide. */
export const WINDOW_SNAP_MAX_ADVANCE = 50;

/**
 * Snap a render-window start forward past a BISECTED action run, so the first
 * strip in the visible slice always begins at a real run boundary and its
 * first-member-derived id can't churn as the window slides.
 *
 * Advances `startIdx` while `cards[i]` is a plain `action` card CONTINUING the
 * run its predecessor belongs to (same `(shadowId, turn)` AND the predecessor
 * is itself an action — a run that merely STARTS at the boundary is intact and
 * must not be skipped). Capped at +`WINDOW_SNAP_MAX_ADVANCE`: a longer
 * (mega-run) bisection would hide too much history, so on cap hit the ORIGINAL
 * start is returned unchanged — the one accepted identity-churn edge: that
 * window's first strip starts mid-run, so its id can still churn while the
 * window slides through a >cap run.
 *
 * Pure; callers must derive "hidden earlier" counts from the SNAPPED start so
 * load-earlier arithmetic stays truthful.
 */
export function snapWindowStart(cards: SpineCard[], startIdx: number): number {
	if (startIdx <= 0 || startIdx >= cards.length) return startIdx;
	let i = startIdx;
	while (
		i < cards.length &&
		cards[i].kind === 'action' &&
		cards[i - 1].kind === 'action' &&
		cards[i - 1].shadowId === cards[i].shadowId &&
		cards[i - 1].turn === cards[i].turn
	) {
		i += 1;
		if (i - startIdx > WINDOW_SNAP_MAX_ADVANCE) return startIdx;
	}
	return i;
}
