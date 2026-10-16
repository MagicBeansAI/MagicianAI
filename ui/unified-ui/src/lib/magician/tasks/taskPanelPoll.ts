/**
 * Keeping an open task panel current while the task is still moving.
 *
 * The panel is pure and renders from a prop; nothing in it fetches. Every
 * surface that opens one therefore has to decide *when* to read again, and until
 * this module existed none of them did on a clock — `/tasks` re-read only when
 * the task store happened to replace the record, which is the list's own 15–20s
 * backstop and stops entirely once nothing in the list is live. An open panel on
 * a running task showed a frozen event log under a verdict reading
 * `Running · step 4 of 7`, which is the one thing "watching something happen"
 * cannot do.
 *
 * **It is `sharedPoll` underneath**, per design §5 and the house rule for every
 * new UI poller: jittered exponential backoff on failure, a fast lease for the
 * live case, the last good value kept.
 *
 * ### The shape mismatch, and how it is answered
 *
 * `createSharedPoll` is *one poller per backend resource* — the TopBar and the
 * Observe page reading one `/meetings/active` between them. What an open panel
 * reads is not one resource: it is per-task **and** per-selected-execution, and
 * both change under the reader. Three routes were available.
 *
 * - **A poller per (task, execution)**, made on demand. Every task switch would
 *   leak a poller — `createSharedPoll` has no destructor, and a pointer to it is
 *   the only thing that could stop one. That is the failure the brief names:
 *   a task switch leaving the previous task's poll running.
 * - **A module-level singleton with a mutable target.** One `/tasks` and one
 *   `/tasks?type=internal` can be mounted at once, and a singleton would make
 *   the second one to aim win for both.
 * - **A poller per open panel, whose target is a variable it closes over** —
 *   this. The poller is created by the surface, lives exactly as long as the
 *   component, and is *aimed* at a task and a run. Re-aiming replaces the target
 *   rather than the poller, so there is never more than one timer per surface and
 *   nothing to leak. What the caller gets back is a snapshot tagged with the
 *   target it was read for, so a reply that lands after the reader has moved on
 *   is recognisable rather than merely late — the same discipline the request
 *   counters in the workspaces already enforce, expressed as identity instead of
 *   as a serial number.
 *
 * ### Failure is a callback, not a value
 *
 * `sharedPoll` keeps the last good value and says nothing when a fetch fails,
 * which is right for a store whose readers want the freshest thing known and
 * wrong for design §6, whose rule is that the panel never renders stale state as
 * current. So `read` failing is reported through `onFailure` **before** the error
 * is re-thrown into `sharedPoll`, which is what still drives the backoff. The
 * surface turns that into the panel's staleness line; a later success clears it.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`, *Live while it runs*.
 */

import type { TaskStatus } from '$lib/stores/taskStore';
import { createSharedPoll } from '$lib/stores/sharedPoll';

/**
 * How hard an open panel should be reading.
 *
 * - `fast` — the run is advancing right now, and the feed is what the reader is
 *   there for.
 * - `idle` — the task can still change without the reader doing anything (a
 *   queued task can start, a paused one can be resumed from elsewhere), but
 *   nothing is streaming.
 * - `off` — **the run has stopped and its events cannot change.** Polling it is
 *   pure waste, and on an idle panel left open it is waste that shows up as
 *   steady request traffic.
 */
export type PanelPollCadence = 'off' | 'idle' | 'fast';

/**
 * What each task status earns, as a `Record<TaskStatus, …>` so a new status is
 * a compile error rather than a panel that silently stops refreshing.
 *
 * **Keyed on the task's own status rather than on the verdict state**, and the
 * difference is not cosmetic: `VERDICT_STATUS` folds `planning` and `pending`
 * both onto `queued`, and a planning task is doing work this second while a
 * pending one is not. The verdict is a sentence for a reader; this is a question
 * about the wire.
 *
 * `paused` is `idle` rather than `off`: a paused run is suspended, not finished,
 * and the ask that unblocks it can arrive at any moment.
 */
const CADENCE: Record<TaskStatus, PanelPollCadence> = {
	running: 'fast',
	planning: 'fast',
	paused: 'idle',
	pending: 'idle',
	ready: 'idle',
	deferred: 'idle',
	archived: 'off',
	completed: 'off',
	failed: 'off',
	cancelled: 'off'
};

/** No task, no cadence. An absent status is not evidence that anything is live. */
export function panelPollCadence(status: TaskStatus | null | undefined): PanelPollCadence {
	return status ? CADENCE[status] : 'off';
}

/**
 * What one read is *about*: a task, and which of its runs.
 *
 * The execution id is part of the target rather than something re-derived from
 * each reply, and that is what makes the reader's chosen run survive a poll. A
 * poller that asked for "the task's current run" every tick would snap the Run
 * act back to the newest attempt while the reader was studying attempt 2 of 3 —
 * worse than not polling at all, because it happens under their cursor.
 */
export interface PanelPollTarget {
	taskId: string;
	/** The run the reader chose, or `null` for the one the task record points at. */
	executionId: string | null;
}

export function samePanelPollTarget(
	a: PanelPollTarget | null,
	b: PanelPollTarget | null
): boolean {
	if (a === null || b === null) return a === b;
	return a.taskId === b.taskId && a.executionId === b.executionId;
}

export interface TaskPanelPollOptions<T> {
	/** Read everything the open panel needs for one target. **Throws** on failure. */
	read: (target: PanelPollTarget) => Promise<T>;
	/** A good read. `at` is when it was read, which is the staleness line's origin. */
	onSnapshot: (target: PanelPollTarget, value: T, at: number) => void;
	/** A read that did not land. The surface renders this rather than freezing quietly. */
	onFailure: (target: PanelPollTarget, message: string) => void;
	idleMs?: number;
	fastMs?: number;
	maxBackoffMs?: number;
}

export interface TaskPanelPoll {
	/**
	 * Point the poll at a target, at a cadence. `null`, or `off`, parks it — no
	 * timer, no requests. Safe to call on every render: aiming at the target it is
	 * already on changes nothing, and aiming somewhere new reads immediately.
	 */
	aim(target: PanelPollTarget | null, cadence: PanelPollCadence): void;
	/**
	 * Read the current target now, whatever the cadence — for a mutation, or the
	 * panel's Retry. It works while parked, which is the point: the states that
	 * park it (`failed`, `cancelled`, `completed`) are exactly the ones a reader
	 * asks to retry.
	 */
	refreshNow(): void;
	/** Park it for good. For `onDestroy`. */
	stop(): void;
}

/** The panel's live cadence. Fast enough that a tool call appears while it is still running. */
export const PANEL_POLL_FAST_MS = 4_000;
/** The not-streaming cadence: a status change should reach the reader, unprompted, within a breath. */
export const PANEL_POLL_IDLE_MS = 20_000;
/** A dead backend costs a request a minute, not a steady hammer. */
export const PANEL_POLL_MAX_BACKOFF_MS = 60_000;

interface Snapshot<T> {
	target: PanelPollTarget;
	value: T;
	at: number;
}

export function createTaskPanelPoll<T>(opts: TaskPanelPollOptions<T>): TaskPanelPoll {
	let target: PanelPollTarget | null = null;
	let unsubscribe: (() => void) | null = null;
	let releaseFast: (() => void) | null = null;
	/** The last snapshot handed to the caller, so a store replay is not delivered twice. */
	let delivered: Snapshot<T> | null = null;

	function message(error: unknown): string {
		const text = error instanceof Error ? error.message.trim() : String(error ?? '').trim();
		return text || 'Could not refresh this task';
	}

	async function attempt(aimed: PanelPollTarget): Promise<Snapshot<T>> {
		try {
			const value = await opts.read(aimed);
			return { target: aimed, value, at: Date.now() };
		} catch (error) {
			// Reported here rather than at the far end, because `sharedPoll` swallows
			// the rejection to keep its last good value — so this is the only place a
			// failed read is visible. Re-thrown immediately after: the backoff is what
			// keeps a dead backend from being hammered, and it is driven by the throw.
			opts.onFailure(aimed, message(error));
			throw error;
		}
	}

	const poll = createSharedPoll<Snapshot<T>>({
		fetcher: async () => {
			const aimed = target;
			// Unreachable while aimed — the poller is unsubscribed whenever the target
			// is null — and a throw rather than a hang if that ever stops being true.
			if (aimed === null) throw new Error('task panel poll is not aimed');
			return attempt(aimed);
		},
		idleMs: opts.idleMs ?? PANEL_POLL_IDLE_MS,
		fastMs: opts.fastMs ?? PANEL_POLL_FAST_MS,
		maxBackoffMs: opts.maxBackoffMs ?? PANEL_POLL_MAX_BACKOFF_MS
	});

	function deliver(snapshot: Snapshot<T> | null): void {
		if (snapshot === null || snapshot === delivered) return;
		// A store replay after a re-aim can carry the previous target's reply. It was
		// true about that target and is not about this one, so it is dropped here
		// rather than at the far end — the surface's own id checks are its last line,
		// not its first.
		if (!samePanelPollTarget(snapshot.target, target)) return;
		delivered = snapshot;
		opts.onSnapshot(snapshot.target, snapshot.value, snapshot.at);
	}

	function park(): void {
		releaseFast?.();
		releaseFast = null;
		unsubscribe?.();
		unsubscribe = null;
	}

	/** One read, outside the cadence, applied through the same two callbacks. */
	function readOnce(aimed: PanelPollTarget): void {
		void attempt(aimed)
			.then(deliver)
			.catch(() => undefined);
	}

	function aim(next: PanelPollTarget | null, cadence: PanelPollCadence): void {
		const moved = !samePanelPollTarget(next, target);
		target = next;

		if (next === null || cadence === 'off') {
			park();
			// **A parked target still has to be read once.** `off` means "this cannot
			// change", not "this is already on screen": a reader opening a finished
			// task's panel, or picking an earlier attempt on one, has aimed at
			// something nobody has fetched — and the run picker is at its most useful
			// on exactly those tasks. Not polling it and never reading it are
			// different things, and only the first is what the cadence decides.
			if (moved && next !== null) readOnce(next);
			return;
		}

		if (cadence === 'fast' && releaseFast === null) {
			releaseFast = poll.requestFast();
		} else if (cadence !== 'fast' && releaseFast !== null) {
			releaseFast();
			releaseFast = null;
		}

		if (unsubscribe === null) {
			// Subscribing is what starts the timer: `sharedPoll`'s readable parks
			// itself with no subscribers, and schedules a read the moment it has one.
			unsubscribe = poll.value.subscribe(deliver);
		} else if (moved) {
			// A new task, or a run the reader just chose. Read it now rather than at
			// the end of the current interval — the control they clicked has already
			// moved, and the act under it must catch up.
			poll.pollNow();
		}
	}

	function refreshNow(): void {
		const aimed = target;
		if (aimed === null) return;
		if (unsubscribe !== null) {
			// Coalesces with an in-flight read, which is `sharedPoll`'s own guarantee.
			poll.pollNow();
			return;
		}
		// Parked: a settled task. One read, through the same path a re-aim takes, so
		// a retry and a poll cannot come to mean different things.
		readOnce(aimed);
	}

	return {
		aim,
		refreshNow,
		stop(): void {
			target = null;
			park();
		}
	};
}
