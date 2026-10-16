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

export type LiveRunState = 'needs_you' | 'working' | 'paused' | 'failed' | 'stopped' | 'done';

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
		lastProgressAt: text(raw.last_progress_at)
	};
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
