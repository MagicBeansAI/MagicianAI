/**
 * The owner's long-running work in one list: VibeDev coding runs, crews (one
 * item each) and Work tasks, ranked by the server.
 *
 * Reads `GET /work/live`, the same summary the phones' Live Activity and
 * ongoing notification show, and re-reads it on the events that move a run
 * (`changesRunState`) and on a poll that is quick while anything is working
 * or waiting on the owner. The notch ear's dots and the `/coding-runs` list
 * share this one store; `start()` / `stop()` are reference counted.
 * `follow()` / `unfollow()` pin an item to the phones (at most 3 per scope).
 */
import { writable, get, type Readable } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';
import { appendCurrentScopeQuery, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { subscribeEvents } from '$lib/realtime/eventHub';
import { changesRunState } from '$lib/stores/vibeDevLiveRunsStore';

export type WorkItemKind = 'coding_run' | 'crew' | 'work_task';
export type WorkItemState = 'needs_you' | 'working' | 'paused' | 'failed' | 'stopped' | 'done';

export interface WorkItemAction {
	kind: 'approve_diff' | 'answer';
	id: string;
}

export interface WorkItem {
	/** `run:<id>`, `crew:<id>` or `task:<id>`. */
	id: string;
	kind: WorkItemKind;
	title: string;
	/** The project name; empty for a Work task. */
	context: string;
	state: WorkItemState;
	step: string | null;
	byOwner: boolean;
	fromPhone: boolean;
	followed: boolean;
	startedAt: string;
	updatedAt: string;
	waitingSince: string | null;
	spentUsd: number | null;
	budgetUsd: number | null;
	action: WorkItemAction | null;
	deepLink: string;
	/** Where the item opens in the web app. */
	webPath: string;
	projectId: string | null;
	runId: string | null;
	liveTaskId: string | null;
	crewId: string | null;
	taskId: string | null;
}

export interface WorkCounts {
	needsYou: number;
	/** Paused items count as working. */
	working: number;
	failed: number;
	done: number;
}

export interface WorkLiveState {
	items: WorkItem[];
	featured: WorkItem | null;
	next: WorkItem[];
	counts: WorkCounts;
	followed: string[];
	loaded: boolean;
	error: string | null;
	/** Why the last Follow or Unfollow did not take. */
	followError: string | null;
}

export type FollowResult = { ok: true } | { ok: false; error: string };

/** Most items one scope can follow; the server answers 409 `follow_limit` past it. */
export const WORK_FOLLOW_LIMIT = 3;

const ACTIVE_POLL_MS = 5_000;
const IDLE_POLL_MS = 30_000;
const EVENT_DEBOUNCE_MS = 1_000;
const KINDS: readonly WorkItemKind[] = ['coding_run', 'crew', 'work_task'];
const STATES: readonly WorkItemState[] = ['needs_you', 'working', 'paused', 'failed', 'stopped', 'done'];
const ACTION_KINDS: readonly WorkItemAction['kind'][] = ['approve_diff', 'answer'];
const EMPTY_COUNTS: WorkCounts = { needsYou: 0, working: 0, failed: 0, done: 0 };

/** Events after which an item's state may have changed: a run's, or a crew's. */
export function changesWorkState(type: string): boolean {
	return changesRunState(type) || type.startsWith('vibedev.crew');
}

function text(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value : null;
}

function amount(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function count(value: unknown): number {
	return typeof value === 'number' && Number.isFinite(value) ? value : 0;
}

function parseAction(raw: unknown): WorkItemAction | null {
	if (!raw || typeof raw !== 'object') return null;
	const row = raw as Record<string, unknown>;
	const id = text(row.id);
	const kind = ACTION_KINDS.find((candidate) => candidate === row.kind);
	if (!id || !kind) return null;
	return { kind, id };
}

export function parseWorkItem(raw: Record<string, unknown>): WorkItem | null {
	const id = text(raw.id);
	const kind = KINDS.find((candidate) => candidate === raw.kind);
	const state = STATES.find((candidate) => candidate === raw.state);
	if (!id || !kind || !state) return null;
	return {
		id,
		kind,
		title: text(raw.title) ?? 'Untitled',
		context: text(raw.context) ?? '',
		state,
		step: text(raw.step),
		byOwner: raw.by_owner === true,
		fromPhone: raw.from_phone === true,
		followed: raw.followed === true,
		startedAt: text(raw.started_at) ?? '',
		updatedAt: text(raw.updated_at) ?? '',
		waitingSince: text(raw.waiting_since),
		spentUsd: amount(raw.spent_usd),
		budgetUsd: amount(raw.budget_usd),
		action: parseAction(raw.action),
		deepLink: text(raw.deep_link) ?? '',
		webPath: text(raw.web_path) ?? '',
		projectId: text(raw.project_id),
		runId: text(raw.run_id),
		liveTaskId: text(raw.live_task_id),
		crewId: text(raw.crew_id),
		taskId: text(raw.task_id)
	};
}

function parseItems(raw: unknown): WorkItem[] {
	return (Array.isArray(raw) ? raw : [])
		.map((row) => (row && typeof row === 'object' ? parseWorkItem(row as Record<string, unknown>) : null))
		.filter((row): row is WorkItem => row !== null);
}

function parseFollowed(raw: unknown): string[] {
	return (Array.isArray(raw) ? raw : []).filter((id): id is string => typeof id === 'string' && id.length > 0);
}

/** Parses the `/work/live` body. Anything it cannot draw is dropped. */
export function parseWorkLive(payload: Record<string, unknown>): Omit<WorkLiveState, 'loaded' | 'error' | 'followError'> {
	const counts =
		payload.counts && typeof payload.counts === 'object' ? (payload.counts as Record<string, unknown>) : {};
	const featured =
		payload.featured && typeof payload.featured === 'object'
			? parseWorkItem(payload.featured as Record<string, unknown>)
			: null;
	return {
		items: parseItems(payload.items),
		featured,
		next: parseItems(payload.next),
		counts: {
			needsYou: count(counts.needs_you),
			working: count(counts.working),
			failed: count(counts.failed),
			done: count(counts.done)
		},
		followed: parseFollowed(payload.followed)
	};
}

export function isWorkActive(item: WorkItem): boolean {
	return item.state === 'needs_you' || item.state === 'working' || item.state === 'paused';
}

async function errorCode(response: Response): Promise<string | null> {
	const payload = (await response.json().catch(() => null)) as { error?: unknown; message?: unknown } | null;
	return text(payload?.error) ?? text(payload?.message);
}

function createWorkLiveStore() {
	const store = writable<WorkLiveState>({
		items: [],
		featured: null,
		next: [],
		counts: EMPTY_COUNTS,
		followed: [],
		loaded: false,
		error: null,
		followError: null
	});
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
		const busy = get(store).items.some(isWorkActive);
		pollTimer = setTimeout(() => void refresh(), busy ? ACTIVE_POLL_MS : IDLE_POLL_MS);
	}

	async function load(): Promise<void> {
		const params = appendCurrentScopeQuery();
		const query = params.toString();
		try {
			const response = await timedFetch(`/api/magician/v2/work/live${query ? `?${query}` : ''}`, {
				headers: scopedRequestHeaders()
			});
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			const payload = (await response.json()) as Record<string, unknown> | null;
			const live = parseWorkLive(payload && typeof payload === 'object' ? payload : {});
			store.update((state) => ({ ...state, ...live, loaded: true, error: null }));
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

	/** The server's followed list is the truth; mark the items to match. */
	function applyFollowed(followed: string[]): void {
		const set = new Set(followed);
		const mark = (item: WorkItem): WorkItem =>
			item.followed === set.has(item.id) ? item : { ...item, followed: set.has(item.id) };
		store.update((state) => ({
			...state,
			followed,
			items: state.items.map(mark),
			featured: state.featured ? mark(state.featured) : null,
			next: state.next.map(mark),
			followError: null
		}));
	}

	async function sendFollow(url: string, init: RequestInit): Promise<FollowResult> {
		try {
			const response = await timedFetch(url, init);
			if (!response.ok) {
				const code = await errorCode(response);
				const error =
					response.status === 409 && code === 'follow_limit'
						? `You can follow at most ${WORK_FOLLOW_LIMIT} items. Unfollow one first.`
						: (code ?? `HTTP ${response.status}`);
				store.update((state) => ({ ...state, followError: error }));
				return { ok: false, error };
			}
			const payload = (await response.json().catch(() => null)) as { followed?: unknown } | null;
			applyFollowed(parseFollowed(payload?.followed));
			return { ok: true };
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			store.update((state) => ({ ...state, followError: message }));
			return { ok: false, error: message };
		}
	}

	/** Follow an item: the phones give it its own Live Activity / notification. */
	function follow(itemId: string): Promise<FollowResult> {
		const query = appendCurrentScopeQuery().toString();
		return sendFollow(`/api/magician/v2/work/follow${query ? `?${query}` : ''}`, {
			method: 'POST',
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify({ item_id: itemId })
		});
	}

	function unfollow(itemId: string): Promise<FollowResult> {
		const query = appendCurrentScopeQuery().toString();
		return sendFollow(
			`/api/magician/v2/work/follow/${encodeURIComponent(itemId)}${query ? `?${query}` : ''}`,
			{ method: 'DELETE', headers: scopedRequestHeaders() }
		);
	}

	function clearFollowError(): void {
		store.update((state) => (state.followError ? { ...state, followError: null } : state));
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
			(event) => changesWorkState(event.type),
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
		subscribe: store.subscribe as Readable<WorkLiveState>['subscribe'],
		start,
		stop,
		refresh,
		follow,
		unfollow,
		clearFollowError
	};
}

export const workLiveStore = createWorkLiveStore();
