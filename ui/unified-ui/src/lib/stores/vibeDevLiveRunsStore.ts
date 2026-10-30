/**
 * Every project's in-flight and just-ended VibeDev runs, one state each.
 *
 * Reads `GET /vibedev/runs/live`, which derives each run's state from the
 * stored tasks and pending requests (never from events), and re-reads it when
 * a task, coding or HITL event arrives, and on a poll that is quick while
 * anything is going. At-a-glance surfaces (the notch drawer, the notch ear's
 * dots) share this one store; `start()` / `stop()` are reference counted.
 */
import { writable, get, type Readable } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';
import { appendCurrentScopeQuery, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { subscribeEvents } from '$lib/realtime/eventHub';
import { hitlCallFromHandle, readHitlRespondHandle } from '$lib/hitl/adapters';
import type { HitlRespondHandle } from '$lib/hitl/types';
import { STALL_AFTER_MS } from '$lib/magician/tasks/taskVerdict';

export type LiveRunState = 'needs_you' | 'working' | 'paused' | 'failed' | 'stopped' | 'done';
/**
 * What a run waits on the owner for (`waiting_kind`): a staged diff to review,
 * a decision, or an acknowledge-only notice. Only the first two are
 * `needs_you`; a notice rides a run in whatever state it is (usually done).
 */
export type LiveRunWaitingKind = 'diff_review' | 'decision' | 'notice';

/** An acknowledge-only owner briefing on a run (`notice`). */
export interface LiveRunNotice {
	/** The user request id `POST /hitl/{id}/respond` answers. */
	requestId: string;
	/** The one option that answers it (`acknowledge`). */
	optionId: string;
	title: string | null;
	message: string;
	severity: string | null;
	/** The server's respond handle for the acknowledgement; absent from older servers. */
	respond: HitlRespondHandle | null;
}

export interface LiveRun {
	runId: string;
	/** The id `/vibedev/runs/{id}/control` takes. */
	liveTaskId: string;
	projectId: string;
	projectName: string;
	title: string;
	latestTurnTitle: string | null;
	state: LiveRunState;
	status: string;
	step: string | null;
	summary: string | null;
	turns: number;
	/** The crew this run is a lane of, and the lane's label. */
	crewId: string | null;
	lane: string | null;
	updatedAt: string;
	lastProgressAt: string | null;
	/** Absent from older servers, and when nothing waits. */
	waitingKind: LiveRunWaitingKind | null;
	notice: LiveRunNotice | null;
}

export interface LiveRunsState {
	runs: LiveRun[];
	loaded: boolean;
	error: string | null;
}

const ACTIVE_POLL_MS = 5_000;
const IDLE_POLL_MS = 30_000;
const EVENT_DEBOUNCE_MS = 1_000;
const STATES: readonly LiveRunState[] = ['needs_you', 'working', 'paused', 'failed', 'stopped', 'done'];
const WAITING_KINDS: readonly LiveRunWaitingKind[] = ['diff_review', 'decision', 'notice'];
/** Events after which a run's state may have changed. */
const REFRESH_EVENT_PREFIXES = ['task.', 'hitl.', 'execution.', 'delegation.results_ready'];
/** `coding.*` lifecycle events; the streaming ones (`coding.message`, tool progress) change nothing. */
const CODING_STATE_EVENTS = new Set([
	'coding.queued',
	'coding.started',
	'coding.agent_started',
	'coding.agent_ended',
	'coding.agent_settled',
	'coding.turn.started',
	'coding.turn.finished',
	'coding.completed',
	'coding.failed',
	'coding.budget_exhausted',
	'coding.approval_requested',
	'coding.sandbox_write_blocked'
]);

export function changesRunState(type: string): boolean {
	return CODING_STATE_EVENTS.has(type) || REFRESH_EVENT_PREFIXES.some((prefix) => type.startsWith(prefix));
}

function text(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value : null;
}

function parseNotice(raw: unknown): LiveRunNotice | null {
	if (!raw || typeof raw !== 'object') return null;
	const record = raw as Record<string, unknown>;
	const requestId = text(record.request_id);
	const message = text(record.message) ?? text(record.title);
	if (!requestId || !message) return null;
	return {
		requestId,
		optionId: text(record.option_id) ?? 'acknowledge',
		title: text(record.title),
		message,
		severity: text(record.severity),
		respond: readHitlRespondHandle(record.respond)
	};
}

export function parseLiveRun(raw: Record<string, unknown>): LiveRun | null {
	const runId = text(raw.run_id);
	const state = STATES.find((candidate) => candidate === raw.state);
	if (!runId || !state) return null;
	return {
		runId,
		liveTaskId: text(raw.live_task_id) ?? runId,
		projectId: text(raw.project_id) ?? '',
		projectName: text(raw.project_name) ?? '',
		title: text(raw.title) ?? 'Untitled run',
		latestTurnTitle: text(raw.latest_turn_title),
		state,
		status: text(raw.status) ?? '',
		step: text(raw.step),
		summary: text(raw.summary),
		turns: typeof raw.turns === 'number' ? raw.turns : 1,
		crewId: text(raw.crew_id),
		lane: text(raw.lane),
		updatedAt: text(raw.updated_at) ?? '',
		lastProgressAt: text(raw.last_progress_at),
		waitingKind: WAITING_KINDS.find((candidate) => candidate === raw.waiting_kind) ?? null,
		notice: parseNotice(raw.notice)
	};
}

/**
 * Acknowledge a run's notice through the one HITL respond endpoint (the
 * request is a `user_request` whose only option is `optionId`). An
 * already-answered notice (409) counts as done.
 */
export async function acknowledgeLiveRunNotice(
	notice: LiveRunNotice
): Promise<{ ok: boolean; error: string | null }> {
	// The server's handle is the route; the request id is the fallback for
	// servers that predate it.
	const call = notice.respond
		? hitlCallFromHandle(notice.respond, {
				type: 'choice',
				selected_id: notice.respond.options?.[0]?.id ?? notice.optionId
			})
		: {
				url: `/api/magician/v2/hitl/${encodeURIComponent(notice.requestId)}/respond`,
				body: {
					source: 'user_request',
					input_type: 'choice',
					value: { type: 'choice', selected_id: notice.optionId },
					channel: 'web'
				},
				soft_success_statuses: [409]
			};
	try {
		const response = await timedFetch(call.url, {
			method: 'POST',
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify(call.body)
		});
		if (response.ok || call.soft_success_statuses.includes(response.status)) {
			return { ok: true, error: null };
		}
		return { ok: false, error: `HTTP ${response.status}` };
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}

/**
 * How long a working run has made no progress, when that is a stall by the
 * task verdict's rule (`STALL_AFTER_MS` of silence on a `running` turn), else
 * `null`. A parent waiting on a delegated child that never resumed keeps its
 * turn `running` with `last_progress_at` frozen; this is what tells it apart
 * from a run that is working. An absent or unreadable instant is no stall.
 */
export function liveRunStalledForMs(run: LiveRun, now: number): number | null {
	if (run.state !== 'working' || run.status !== 'running' || !run.lastProgressAt) return null;
	const at = Date.parse(run.lastProgressAt);
	if (!Number.isFinite(at)) return null;
	const silent = now - at;
	return silent >= STALL_AFTER_MS ? silent : null;
}

export function isActive(run: LiveRun): boolean {
	return run.state === 'needs_you' || run.state === 'working' || run.state === 'paused';
}

/** Path that opens a run in the VibeDev studio. */
export function liveRunPath(run: LiveRun): string {
	const params = new URLSearchParams({ project: run.projectId, task: run.runId });
	return `/vibe?${params.toString()}`;
}

function createVibeDevLiveRunsStore() {
	const store = writable<LiveRunsState>({ runs: [], loaded: false, error: null });
	let users = 0;
	let pollTimer: ReturnType<typeof setTimeout> | null = null;
	let eventTimer: ReturnType<typeof setTimeout> | null = null;
	let unsubscribeEvents: (() => void) | null = null;
	let inFlight: Promise<void> | null = null;
	let again = false;

	function schedulePoll(): void {
		if (pollTimer) clearTimeout(pollTimer);
		pollTimer = null;
		if (users === 0) return;
		const busy = get(store).runs.some(isActive);
		pollTimer = setTimeout(() => void refresh(), busy ? ACTIVE_POLL_MS : IDLE_POLL_MS);
	}

	async function load(): Promise<void> {
		const params = appendCurrentScopeQuery();
		const query = params.toString();
		try {
			const response = await timedFetch(
				`/api/magician/v2/vibedev/runs/live${query ? `?${query}` : ''}`,
				{ headers: scopedRequestHeaders() }
			);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			const payload = (await response.json()) as { runs?: unknown };
			const runs = (Array.isArray(payload.runs) ? payload.runs : [])
				.map((row) => (row && typeof row === 'object' ? parseLiveRun(row as Record<string, unknown>) : null))
				.filter((row): row is LiveRun => row !== null);
			store.set({ runs, loaded: true, error: null });
		} catch (error) {
			store.update((state) => ({
				...state,
				loaded: true,
				error: error instanceof Error ? error.message : String(error)
			}));
		}
	}

	/** Re-read now. Calls made while a read is in flight coalesce into one more read. */
	async function refresh(): Promise<void> {
		if (inFlight) {
			again = true;
			return inFlight;
		}
		inFlight = (async () => {
			do {
				again = false;
				await load();
			} while (again);
		})();
		try {
			await inFlight;
		} finally {
			inFlight = null;
			schedulePoll();
		}
	}

	function onEvent(): void {
		if (eventTimer) return;
		eventTimer = setTimeout(() => {
			eventTimer = null;
			void refresh();
		}, EVENT_DEBOUNCE_MS);
	}

	function start(): void {
		users += 1;
		if (users > 1) return;
		unsubscribeEvents = subscribeEvents(
			(event) => changesRunState(event.type),
			onEvent,
			() => void refresh()
		);
		void refresh();
	}

	function stop(): void {
		users = Math.max(0, users - 1);
		if (users > 0) return;
		unsubscribeEvents?.();
		unsubscribeEvents = null;
		if (pollTimer) clearTimeout(pollTimer);
		if (eventTimer) clearTimeout(eventTimer);
		pollTimer = null;
		eventTimer = null;
	}

	return {
		subscribe: store.subscribe as Readable<LiveRunsState>['subscribe'],
		start,
		stop,
		refresh
	};
}

export const vibeDevLiveRunsStore = createVibeDevLiveRunsStore();
