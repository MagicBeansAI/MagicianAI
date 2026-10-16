/**
 * The VibeDev cockpit's run list for the open project, a page at a time.
 *
 * The server pages runs (`GET /vibedev/projects/{id}/runs`): the newest
 * `RUN_PAGE_SIZE` runs, each with its threaded follow-up turns, and a cursor
 * for older ones. This store holds the loaded pages and keeps them fresh:
 * after every task-list load (which follows realtime task events) and, while
 * a loaded run is still going, on a short poll.
 */
import { get, writable, type Readable } from 'svelte/store';
import { taskStore, type Task, type VibeDevRunLink } from './taskStore';

export const RUN_PAGE_SIZE = 10;
/** Most runs one refresh re-reads; older loaded pages are kept as they are. */
const MAX_REFRESH_RUNS = 50;
const LIVE_POLL_MS = 10_000;
const REFRESH_DEBOUNCE_MS = 400;
const SETTLED = new Set(['completed', 'failed', 'cancelled', 'archived']);

export interface VibeDevRunsState {
	projectId: string | null;
	/** Root runs and their threaded turns, as the server returned them. */
	tasks: Task[];
	/** Typed links (parent, crew, lane) of the loaded tasks that have one. */
	links: Record<string, VibeDevRunLink>;
	/** Root runs loaded so far. */
	runs: number;
	totalRuns: number;
	hasMore: boolean;
	nextCursor: string | null;
	/** The first page has arrived for `projectId`. */
	loaded: boolean;
	loading: boolean;
	loadingMore: boolean;
	error: boolean;
}

function initialState(projectId: string | null = null): VibeDevRunsState {
	return {
		projectId,
		tasks: [],
		links: {},
		runs: 0,
		totalRuns: 0,
		hasMore: false,
		nextCursor: null,
		loaded: false,
		loading: false,
		loadingMore: false,
		error: false
	};
}

function isLive(task: Task): boolean {
	return !SETTLED.has(task.status) || Boolean(task.synthesisPending);
}

function createVibeDevRunsStore() {
	const store = writable<VibeDevRunsState>(initialState());
	const { subscribe, set, update } = store;
	let generation = 0;
	let pollTimer: ReturnType<typeof setTimeout> | null = null;
	let refreshTimer: ReturnType<typeof setTimeout> | null = null;
	let unsubscribeTasks: (() => void) | null = null;

	function clearTimers(): void {
		if (pollTimer) clearTimeout(pollTimer);
		if (refreshTimer) clearTimeout(refreshTimer);
		pollTimer = null;
		refreshTimer = null;
	}

	function schedulePoll(): void {
		if (pollTimer) clearTimeout(pollTimer);
		pollTimer = null;
		const state = get(store);
		if (!state.projectId || !state.tasks.some(isLive)) return;
		pollTimer = setTimeout(() => void refresh(), LIVE_POLL_MS);
	}

	// A finished task-list load means task events arrived (or a run was just
	// started): re-read the loaded runs too.
	function followTaskLoads(): void {
		if (unsubscribeTasks) return;
		let wasLoading = false;
		unsubscribeTasks = taskStore.subscribe((state) => {
			if (wasLoading && !state.isLoading) refreshSoon();
			wasLoading = state.isLoading;
		});
	}

	async function load(projectId: string | null): Promise<void> {
		const current = get(store);
		if (!projectId) {
			reset();
			return;
		}
		if (current.projectId === projectId && (current.loaded || current.loading)) {
			if (current.loaded) refreshSoon();
			return;
		}
		followTaskLoads();
		clearTimers();
		const token = ++generation;
		set({ ...initialState(projectId), loading: true });
		const page = await taskStore.fetchVibeDevProjectRuns(projectId, { limit: RUN_PAGE_SIZE });
		if (token !== generation) return;
		if (!page) {
			update((state) => ({ ...state, loading: false, loaded: true, error: true }));
			return;
		}
		set({
			projectId,
			tasks: page.tasks,
			links: page.links ?? {},
			runs: page.runs,
			totalRuns: page.totalRuns,
			hasMore: page.hasMore,
			nextCursor: page.nextCursor,
			loaded: true,
			loading: false,
			loadingMore: false,
			error: false
		});
		schedulePoll();
	}

	async function loadMore(): Promise<void> {
		const state = get(store);
		if (!state.projectId || !state.hasMore || state.loadingMore || !state.nextCursor) return;
		const token = generation;
		update((current) => ({ ...current, loadingMore: true }));
		const page = await taskStore.fetchVibeDevProjectRuns(state.projectId, {
			limit: RUN_PAGE_SIZE,
			cursor: state.nextCursor
		});
		if (token !== generation) return;
		update((current) => {
			if (!page) return { ...current, loadingMore: false };
			const known = new Set(current.tasks.map((task) => task.id));
			return {
				...current,
				tasks: [...current.tasks, ...page.tasks.filter((task) => !known.has(task.id))],
				links: { ...current.links, ...(page.links ?? {}) },
				runs: current.runs + page.runs,
				totalRuns: page.totalRuns,
				hasMore: page.hasMore,
				nextCursor: page.nextCursor,
				loadingMore: false
			};
		});
		schedulePoll();
	}

	/** Re-read the loaded runs (up to `MAX_REFRESH_RUNS` of them). */
	async function refresh(): Promise<void> {
		const state = get(store);
		if (!state.projectId || !state.loaded) return;
		const token = generation;
		const limit = Math.min(Math.max(state.runs, RUN_PAGE_SIZE), MAX_REFRESH_RUNS);
		const page = await taskStore.fetchVibeDevProjectRuns(state.projectId, { limit });
		if (token !== generation || !page) {
			if (token === generation) schedulePoll();
			return;
		}
		update((current) => {
			if (current.runs <= MAX_REFRESH_RUNS) {
				return {
					...current,
					tasks: page.tasks,
					links: page.links ?? {},
					runs: page.runs,
					totalRuns: page.totalRuns,
					hasMore: page.hasMore,
					nextCursor: page.nextCursor,
					error: false
				};
			}
			// More loaded than one refresh re-reads: update what it returned,
			// add new runs, keep the older pages and their cursor.
			const fresh = new Map(page.tasks.map((task) => [task.id, task]));
			const kept = current.tasks.map((task) => fresh.get(task.id) ?? task);
			const known = new Set(kept.map((task) => task.id));
			const added = page.tasks.filter((task) => !known.has(task.id));
			return {
				...current,
				tasks: [...added, ...kept],
				links: { ...current.links, ...(page.links ?? {}) },
				totalRuns: page.totalRuns,
				error: false
			};
		});
		schedulePoll();
	}

	function refreshSoon(): void {
		if (refreshTimer) clearTimeout(refreshTimer);
		refreshTimer = setTimeout(() => {
			refreshTimer = null;
			void refresh();
		}, REFRESH_DEBOUNCE_MS);
	}

	function reset(): void {
		generation += 1;
		clearTimers();
		set(initialState());
	}

	return {
		subscribe: subscribe as Readable<VibeDevRunsState>['subscribe'],
		load,
		loadMore,
		refresh,
		refreshSoon,
		reset
	};
}

export const vibeDevRunsStore = createVibeDevRunsStore();
