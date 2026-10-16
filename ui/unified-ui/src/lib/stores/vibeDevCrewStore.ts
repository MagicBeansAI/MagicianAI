/**
 * VibeDev crews for one project: a goal split into lanes that run at once.
 *
 * Reads `GET /vibedev/projects/{id}/crews` and re-reads it when a task, coding
 * or HITL event arrives (a lane ended, the start question was answered) and on
 * a poll while a crew is waiting or running. Creating, starting and stopping
 * go through the crew endpoints; the server owns every state change.
 */
import { get, writable, type Readable } from 'svelte/store';
import { LONG_FETCH_TIMEOUT_MS, timedFetch } from '$lib/shared/fetch';
import { appendCurrentScopeQuery, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { subscribeEvents } from '$lib/realtime/eventHub';
import { changesRunState } from '$lib/stores/vibeDevLiveRunsStore';

export type CrewStatus =
	| 'proposed'
	| 'running'
	| 'choosing'
	| 'ready'
	| 'integrated'
	| 'conflict'
	| 'failed'
	| 'cancelled';
export type CrewLaneStatus = 'pending' | 'running' | 'done' | 'failed' | 'stopped';
export type CrewMode = 'split' | 'race';
export type CrewLaneRole = 'work' | 'integrate' | 'review';

export interface CrewLane {
	lane: string;
	brief: string;
	owns: string[];
	codingProfileId: string | null;
	runTaskId: string | null;
	status: CrewLaneStatus;
	role: CrewLaneRole;
}

export interface VibeDevCrew {
	crewId: string;
	goal: string;
	status: CrewStatus;
	mode: CrewMode;
	lanes: CrewLane[];
	review: boolean;
	/** In a race, the index of the lane kept. */
	winner: number | null;
	budgetUsd: number | null;
	spentUsd: number | null;
	startRequestId: string | null;
	/** The integration branch the lanes are merged into. */
	branch: string | null;
	conflict: { lane: string; files: string[] } | null;
	pullRequestUrl: string | null;
	note: string | null;
	createdAtMs: number;
	updatedAtMs: number;
}

export interface CrewLaneInput {
	lane: string;
	brief: string;
	owns?: string[];
	coding_profile_id?: string;
}

export interface CreateCrewInput {
	goal: string;
	/** Omitted: the planner splits the goal. */
	lanes?: CrewLaneInput[];
	budgetUsd?: number;
	maxLanes?: number;
	/** Start at once instead of asking in Attention. */
	start?: boolean;
	mode?: CrewMode;
	/** Race: how many attempts (default 3). */
	racers?: number;
	/** Review the merged branch before the pull-request question. */
	review?: boolean;
}

export interface CrewState {
	projectId: string | null;
	/** The crews still in play (`isCrewOpen`), plus the ones the owner acted on
	 *  here since this project was loaded, so a Stop or a pull request shows
	 *  its result instead of vanishing. */
	crews: VibeDevCrew[];
	loaded: boolean;
	error: string | null;
	/** Earlier (settled) crews loaded so far, most recently changed first. */
	settled: VibeDevCrew[];
	/** How many settled crews the project has. */
	settledTotal: number;
	settledLoading: boolean;
	/** The crews create/start/stop calls are working on (`'new'` while creating). */
	busy: string[];
}

const CREW_STATUSES: readonly CrewStatus[] = [
	'proposed',
	'running',
	'choosing',
	'ready',
	'integrated',
	'conflict',
	'failed',
	'cancelled'
];
const LANE_STATUSES: readonly CrewLaneStatus[] = ['pending', 'running', 'done', 'failed', 'stopped'];
const LANE_ROLES: readonly CrewLaneRole[] = ['work', 'integrate', 'review'];
const ACTIVE_POLL_MS = 10_000;
const EVENT_DEBOUNCE_MS = 1_000;

function text(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value : null;
}

function parseConflict(raw: unknown): VibeDevCrew['conflict'] {
	if (!raw || typeof raw !== 'object') return null;
	const conflict = raw as Record<string, unknown>;
	const lane = text(conflict.lane);
	if (!lane) return null;
	const files = Array.isArray(conflict.files)
		? conflict.files.filter((file): file is string => typeof file === 'string')
		: [];
	return { lane, files };
}

export function parseCrew(raw: unknown): VibeDevCrew | null {
	if (!raw || typeof raw !== 'object') return null;
	const row = raw as Record<string, unknown>;
	const crewId = text(row.crew_id);
	if (!crewId) return null;
	const status = CREW_STATUSES.find((value) => value === row.status) ?? 'proposed';
	const lanes = (Array.isArray(row.lanes) ? row.lanes : []).flatMap((entry) => {
		if (!entry || typeof entry !== 'object') return [];
		const lane = entry as Record<string, unknown>;
		const label = text(lane.lane);
		if (!label) return [];
		return [
			{
				lane: label,
				brief: text(lane.brief) ?? '',
				owns: Array.isArray(lane.owns) ? lane.owns.filter((path): path is string => typeof path === 'string') : [],
				codingProfileId: text(lane.coding_profile_id),
				runTaskId: text(lane.run_task_id),
				status: LANE_STATUSES.find((value) => value === lane.status) ?? 'pending',
				role: LANE_ROLES.find((value) => value === lane.role) ?? 'work'
			}
		];
	});
	return {
		crewId,
		goal: text(row.goal) ?? '',
		status,
		mode: row.mode === 'race' ? 'race' : 'split',
		lanes,
		review: row.review === true,
		winner: typeof row.winner === 'number' ? row.winner : null,
		budgetUsd: typeof row.budget_usd === 'number' ? row.budget_usd : null,
		spentUsd: typeof row.spent_usd === 'number' ? row.spent_usd : null,
		startRequestId: text(row.start_request_id),
		branch: text(row.branch),
		conflict: parseConflict(row.conflict),
		pullRequestUrl: text(row.pull_request_url),
		note: text(row.note),
		createdAtMs: typeof row.created_at_ms === 'number' ? row.created_at_ms : 0,
		updatedAtMs: typeof row.updated_at_ms === 'number' ? row.updated_at_ms : 0
	};
}

/** Still has something to do or decide: waiting, running, merging, or merged without a pull request. */
export function isCrewOpen(crew: VibeDevCrew): boolean {
	return (
		crew.status === 'proposed' ||
		crew.status === 'running' ||
		crew.status === 'choosing' ||
		crew.status === 'ready' ||
		crew.status === 'conflict' ||
		(crew.status === 'integrated' && !crew.pullRequestUrl)
	);
}

/** Earlier crews load this many at a time. */
export const SETTLED_CREW_PAGE = 5;

function crewUrl(projectId: string, suffix = '', params: Record<string, string> = {}): string {
	const search = appendCurrentScopeQuery();
	for (const [key, value] of Object.entries(params)) search.set(key, value);
	const query = search.toString();
	return `/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/crews${suffix}${query ? `?${query}` : ''}`;
}

async function readError(response: Response): Promise<string> {
	const payload = (await response.json().catch(() => null)) as { message?: string; error?: string } | null;
	return payload?.message ?? payload?.error ?? `HTTP ${response.status}`;
}

function createVibeDevCrewStore() {
	const empty = (projectId: string | null): CrewState => ({
		projectId,
		crews: [],
		loaded: false,
		error: null,
		settled: [],
		settledTotal: 0,
		settledLoading: false,
		busy: []
	});
	const store = writable<CrewState>(empty(null));
	let pollTimer: ReturnType<typeof setTimeout> | null = null;
	let eventTimer: ReturnType<typeof setTimeout> | null = null;
	let unsubscribeEvents: (() => void) | null = null;
	let generation = 0;
	// Every read or action result takes a number when it is issued (a read)
	// or answered (an action); a result older than the last one applied is
	// dropped, so a slow read cannot undo a newer Start or Stop.
	let sequence = 0;
	let applied = 0;
	// Crews a call here answered, kept in view until the project changes.
	let actedOn = new Map<string, VibeDevCrew>();

	function schedulePoll(): void {
		if (pollTimer) clearTimeout(pollTimer);
		pollTimer = null;
		const state = get(store);
		if (!state.projectId || !state.crews.some(isCrewOpen)) return;
		pollTimer = setTimeout(() => void refresh(), ACTIVE_POLL_MS);
	}

	async function refresh(): Promise<void> {
		const { projectId } = get(store);
		if (!projectId) return;
		const token = generation;
		const mine = ++sequence;
		try {
			// The open crews only; the settled ones load a page at a time
			// (`loadSettled`), and this read counts them.
			const response = await timedFetch(crewUrl(projectId, '', { state: 'open' }), {
				headers: scopedRequestHeaders()
			});
			if (!response.ok) throw new Error(await readError(response));
			const payload = (await response.json()) as { crews?: unknown[]; settled_total?: unknown };
			if (token !== generation || mine < applied) return;
			applied = mine;
			const listed = (payload.crews ?? []).map(parseCrew).filter((crew): crew is VibeDevCrew => crew !== null);
			// An older server ignores `state` and sends every crew.
			const legacy = typeof payload.settled_total !== 'number';
			const open = legacy ? listed.filter(isCrewOpen) : listed;
			const openIds = new Set(open.map((crew) => crew.crewId));
			const crews = [...open, ...[...actedOn.values()].filter((crew) => !openIds.has(crew.crewId))];
			const settledTotal = legacy
				? listed.length - open.length
				: Math.max(0, Math.floor(payload.settled_total as number));
			store.update((state) => ({
				...state,
				crews,
				settledTotal,
				...(legacy ? { settled: listed.filter((crew) => !isCrewOpen(crew)) } : {}),
				loaded: true,
				error: null
			}));
		} catch (error) {
			if (token !== generation) return;
			store.update((state) => ({
				...state,
				loaded: true,
				error: error instanceof Error ? error.message : String(error)
			}));
		} finally {
			if (token === generation) schedulePoll();
		}
	}

	function follow(): void {
		if (unsubscribeEvents) return;
		unsubscribeEvents = subscribeEvents(
			(event) => changesRunState(event.type),
			() => {
				if (eventTimer) return;
				eventTimer = setTimeout(() => {
					eventTimer = null;
					void refresh();
				}, EVENT_DEBOUNCE_MS);
			},
			() => void refresh()
		);
	}

	/** Show `projectId`'s crews; `null` stops following. */
	function load(projectId: string | null): void {
		if (get(store).projectId === projectId) return;
		generation += 1;
		if (pollTimer) clearTimeout(pollTimer);
		pollTimer = null;
		actedOn = new Map();
		store.set(empty(projectId));
		if (!projectId) {
			unsubscribeEvents?.();
			unsubscribeEvents = null;
			return;
		}
		follow();
		void refresh();
	}

	async function call(busy: string, url: string, body?: unknown): Promise<VibeDevCrew | null> {
		const token = generation;
		store.update((state) => ({ ...state, busy: [...state.busy, busy], error: null }));
		try {
			// Planning runs a model and a pull request pushes and calls `gh`:
			// both can outlast the default timeout, and a timed-out create the
			// server still finishes would invite a duplicate crew.
			const response = await timedFetch(url, {
				method: 'POST',
				headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
				body: body === undefined ? undefined : JSON.stringify(body),
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			});
			if (!response.ok) throw new Error(await readError(response));
			const payload = (await response.json()) as { crew?: unknown };
			const crew = parseCrew(payload.crew);
			// The owner moved to another project while this ran.
			if (token !== generation) return crew;
			applied = ++sequence;
			if (crew) {
				actedOn.set(crew.crewId, crew);
				store.update((state) => ({
					...state,
					crews: [crew, ...state.crews.filter((existing) => existing.crewId !== crew.crewId)]
				}));
			}
			return crew;
		} catch (error) {
			if (token === generation) {
				store.update((state) => ({ ...state, error: error instanceof Error ? error.message : String(error) }));
			}
			return null;
		} finally {
			if (token === generation) {
				store.update((state) => {
					const at = state.busy.indexOf(busy);
					return at < 0 ? state : { ...state, busy: state.busy.filter((_, index) => index !== at) };
				});
				schedulePoll();
			}
		}
	}

	/** Load earlier crews: the next page, or from the start when `reset`
	 *  (as many as were showing, so the list does not shrink under the owner). */
	async function loadSettled(reset = false): Promise<void> {
		const before = get(store);
		const { projectId } = before;
		if (!projectId || before.settledLoading) return;
		const token = generation;
		const offset = reset ? 0 : before.settled.length;
		const limit = reset ? Math.max(SETTLED_CREW_PAGE, before.settled.length) : SETTLED_CREW_PAGE;
		store.update((state) => ({ ...state, settledLoading: true }));
		try {
			const response = await timedFetch(
				crewUrl(projectId, '', { state: 'settled', offset: String(offset), limit: String(limit) }),
				{ headers: scopedRequestHeaders() }
			);
			if (!response.ok) throw new Error(await readError(response));
			const payload = (await response.json()) as { crews?: unknown[]; total?: unknown };
			if (token !== generation) return;
			const page = (payload.crews ?? []).map(parseCrew).filter((crew): crew is VibeDevCrew => crew !== null);
			store.update((state) => {
				const kept = reset ? [] : state.settled;
				const seen = new Set(kept.map((crew) => crew.crewId));
				return {
					...state,
					settled: [...kept, ...page.filter((crew) => !seen.has(crew.crewId))],
					settledTotal: typeof payload.total === 'number' ? payload.total : state.settledTotal,
					settledLoading: false
				};
			});
		} catch (error) {
			if (token !== generation) return;
			store.update((state) => ({
				...state,
				settledLoading: false,
				error: error instanceof Error ? error.message : String(error)
			}));
		}
	}

	function create(input: CreateCrewInput): Promise<VibeDevCrew | null> {
		const { projectId } = get(store);
		if (!projectId) return Promise.resolve(null);
		return call('new', crewUrl(projectId), {
			goal: input.goal,
			lanes: input.lanes,
			budget_usd: input.budgetUsd,
			max_lanes: input.maxLanes,
			start: input.start ?? false,
			mode: input.mode ?? 'split',
			racers: input.racers,
			review: input.review ?? false
		});
	}

	function start(crewId: string): Promise<VibeDevCrew | null> {
		const { projectId } = get(store);
		if (!projectId) return Promise.resolve(null);
		return call(crewId, crewUrl(projectId, `/${encodeURIComponent(crewId)}/start`));
	}

	function stop(crewId: string): Promise<VibeDevCrew | null> {
		const { projectId } = get(store);
		if (!projectId) return Promise.resolve(null);
		return call(crewId, crewUrl(projectId, `/${encodeURIComponent(crewId)}/stop`));
	}

	function integrate(crewId: string): Promise<VibeDevCrew | null> {
		const { projectId } = get(store);
		if (!projectId) return Promise.resolve(null);
		return call(crewId, crewUrl(projectId, `/${encodeURIComponent(crewId)}/integrate`));
	}

	function resolve(crewId: string): Promise<VibeDevCrew | null> {
		const { projectId } = get(store);
		if (!projectId) return Promise.resolve(null);
		return call(crewId, crewUrl(projectId, `/${encodeURIComponent(crewId)}/resolve`));
	}

	/** Keep one finished race attempt; `null` keeps none. */
	function choose(crewId: string, laneIndex: number | null): Promise<VibeDevCrew | null> {
		const { projectId } = get(store);
		if (!projectId) return Promise.resolve(null);
		return call(crewId, crewUrl(projectId, `/${encodeURIComponent(crewId)}/choose`), {
			lane_index: laneIndex
		});
	}

	function openPullRequest(crewId: string): Promise<VibeDevCrew | null> {
		const { projectId } = get(store);
		if (!projectId) return Promise.resolve(null);
		return call(crewId, crewUrl(projectId, `/${encodeURIComponent(crewId)}/pull-request`));
	}

	return {
		subscribe: store.subscribe as Readable<CrewState>['subscribe'],
		load,
		refresh,
		loadSettled,
		create,
		start,
		stop,
		integrate,
		resolve,
		choose,
		openPullRequest
	};
}

export const vibeDevCrewStore = createVibeDevCrewStore();
