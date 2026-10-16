import { writable } from 'svelte/store';

import type { AttentionFeedbackAttribution } from '$lib/attention/attentionBandit';
import {
	fetchAttentionRankRecomputeHealth,
	fetchAttentionRankRecomputeJob,
	type AttentionRankRecomputeBinding,
	type AttentionRankRecomputeDisplayStatus,
	type AttentionRankRecomputeHealth,
	type AttentionRankRecomputeJob,
	type AttentionRankRecomputeReference
} from '$lib/attention/attentionRankRecompute';
import type {
	AttentionFeedbackReceipt,
	AttentionSurface
} from '$lib/channel/channelFollowUpLearning';
import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';

const RECENT_WINDOW_MS = 10 * 60_000;
const MAX_RECENT_JOBS = 20;
const POLL_BACKOFF_MS = [1_000, 2_000, 4_000, 8_000, 15_000] as const;

export interface TrackedAttentionRankRecompute {
	key: string;
	scopeKey: string;
	acceptedAt: number;
	expiresAt: number;
	reference: AttentionRankRecomputeReference | null;
	binding: AttentionRankRecomputeBinding | null;
	status: AttentionRankRecomputeDisplayStatus;
	job: AttentionRankRecomputeJob | null;
	pollAttempts: number;
	isPolling: boolean;
	error: string | null;
}

export interface AttentionRankRecomputeState {
	scopeKey: string | null;
	jobs: TrackedAttentionRankRecompute[];
	health: AttentionRankRecomputeHealth | null;
	healthLoading: boolean;
}

export interface TrackAttentionRankRecomputeContext {
	raw_candidate_id: string;
	source_revision?: string | null;
	attribution?: AttentionFeedbackAttribution | null;
}

const initialState: AttentionRankRecomputeState = {
	scopeKey: null,
	jobs: [],
	health: null,
	healthLoading: false
};

function scopeKey(principal: string, workspace: string): string {
	return [principal, workspace].join('\u0000');
}

function terminal(status: AttentionRankRecomputeDisplayStatus): boolean {
	return status === 'succeeded' || status === 'stale' || status === 'dead' ||
		status === 'enqueue_failed' || status === 'disabled';
}

function createAttentionRankRecomputeStore() {
	const { subscribe, set, update } = writable<AttentionRankRecomputeState>(initialState);
	let snapshot = initialState;
	let generation = 0;
	let documentVisible = true;
	const consumers = new Set<string>();
	const timers = new Map<string, ReturnType<typeof setTimeout>>();
	const expiryTimers = new Map<string, ReturnType<typeof setTimeout>>();
	subscribe((state) => (snapshot = state));

	function clearTimer(key: string): void {
		const timer = timers.get(key);
		if (timer !== undefined) clearTimeout(timer);
		timers.delete(key);
	}

	function clearExpiryTimer(key: string): void {
		const timer = expiryTimers.get(key);
		if (timer !== undefined) clearTimeout(timer);
		expiryTimers.delete(key);
	}

	function pruneUntrackedTimers(): void {
		const retained = new Set(snapshot.jobs.map((job) => job.key));
		for (const key of timers.keys()) {
			if (!retained.has(key)) clearTimer(key);
		}
		for (const key of expiryTimers.keys()) {
			if (!retained.has(key)) clearExpiryTimer(key);
		}
	}

	function clearPollTimers(): void {
		for (const timer of timers.values()) clearTimeout(timer);
		timers.clear();
	}

	function clearAllTimers(): void {
		clearPollTimers();
		for (const timer of expiryTimers.values()) clearTimeout(timer);
		expiryTimers.clear();
	}

	function scheduleExpiry(key: string, expiresAt: number): void {
		const prior = expiryTimers.get(key);
		if (prior !== undefined) clearTimeout(prior);
		expiryTimers.set(key, setTimeout(() => {
			expiryTimers.delete(key);
			clearTimer(key);
			update((state) => ({ ...state, jobs: state.jobs.filter((job) => job.key !== key) }));
		}, Math.max(0, expiresAt - Date.now())));
	}

	function active(): boolean {
		return consumers.size > 0 && documentVisible;
	}

	function mutateJob(key: string, mutate: (job: TrackedAttentionRankRecompute) => TrackedAttentionRankRecompute): void {
		update((state) => ({
			...state,
			jobs: state.jobs.map((job) => job.key === key ? mutate(job) : job)
		}));
	}

	function schedule(key: string): void {
		clearTimer(key);
		const tracked = snapshot.jobs.find((job) => job.key === key);
		if (!tracked || terminal(tracked.status) || !tracked.reference?.status_href || !tracked.binding ||
			tracked.expiresAt <= Date.now() || tracked.scopeKey !== snapshot.scopeKey || !active()) return;
		const delay = POLL_BACKOFF_MS[Math.min(tracked.pollAttempts, POLL_BACKOFF_MS.length - 1)];
		timers.set(key, setTimeout(() => void poll(key), delay));
	}

	async function poll(key: string): Promise<void> {
		clearTimer(key);
		const tracked = snapshot.jobs.find((job) => job.key === key);
		if (!tracked || terminal(tracked.status) || !tracked.reference?.status_href || !tracked.binding ||
			tracked.expiresAt <= Date.now() || tracked.scopeKey !== snapshot.scopeKey || !active()) return;
		const scope = getCurrentScopeIdentity();
		if (scopeKey(scope.principal, scope.workspace) !== tracked.scopeKey) return;
		const requestGeneration = generation;
		mutateJob(key, (job) => ({ ...job, isPolling: true }));
		const result = await fetchAttentionRankRecomputeJob(
			tracked.reference.status_href,
			scope,
			tracked.binding
		);
		if (requestGeneration !== generation || scopeKey(scope.principal, scope.workspace) !== snapshot.scopeKey) return;
		if (!result.ok) {
			mutateJob(key, (job) => ({
				...job,
				isPolling: false,
				pollAttempts: job.pollAttempts + 1,
				error: result.error
			}));
			if (result.retryable) schedule(key);
			return;
		}
		mutateJob(key, (job) => ({
			...job,
			status: result.job.status,
			job: result.job,
			isPolling: false,
			pollAttempts: job.pollAttempts + 1,
			error: null
		}));
		if (!terminal(result.job.status)) {
			schedule(key);
		} else {
			void refreshHealth();
		}
	}

	async function refreshHealth(): Promise<void> {
		if (!active() || !snapshot.scopeKey || snapshot.healthLoading) return;
		const scope = getCurrentScopeIdentity();
		if (scopeKey(scope.principal, scope.workspace) !== snapshot.scopeKey) return;
		const requestGeneration = generation;
		update((state) => ({ ...state, healthLoading: true }));
		const health = await fetchAttentionRankRecomputeHealth(scope);
		if (requestGeneration !== generation) return;
		update((state) => ({ ...state, health, healthLoading: false }));
	}

	function resume(): void {
		if (!active()) return;
		void refreshHealth();
		for (const job of snapshot.jobs) schedule(job.key);
	}

	function setScopeValue(nextScopeKey: string): void {
		if (snapshot.scopeKey === nextScopeKey) return;
		generation += 1;
		clearAllTimers();
		set({ ...initialState, scopeKey: nextScopeKey });
		resume();
	}

	return {
		subscribe,
		setScope: setScopeValue,
		mount(consumerId: string): void {
			consumers.add(consumerId);
			resume();
		},
		unmount(consumerId: string): void {
			consumers.delete(consumerId);
			if (consumers.size === 0) clearPollTimers();
		},
		setDocumentVisible(visible: boolean): void {
			documentVisible = visible;
			if (visible) resume();
			else clearPollTimers();
		},
		track(receipt: AttentionFeedbackReceipt | null, context: TrackAttentionRankRecomputeContext): void {
			if (!receipt || receipt.rank_recompute === undefined) return;
			const scope = getCurrentScopeIdentity();
			const currentScopeKey = scopeKey(scope.principal, scope.workspace);
			if (snapshot.scopeKey !== currentScopeKey) setScopeValue(currentScopeKey);
			const reference = receipt.rank_recompute;
			const acceptedAt = Date.now();
			const attribution = context.attribution ?? null;
			const binding: AttentionRankRecomputeBinding | null = reference?.job_id ? {
				job_id: reference.job_id,
				outcome_id: receipt.outcome_id,
				origin_surface: receipt.surface,
				raw_candidate_id: context.raw_candidate_id,
				...(Object.prototype.hasOwnProperty.call(context, 'source_revision')
					? { source_revision: context.source_revision }
					: {}),
				outcome: receipt.outcome,
				decision_id: attribution?.decision_id ?? null,
				delivery_id: attribution?.delivery_id ?? null,
				impression_id: attribution?.impression_id ?? null,
				affected_rank_before: reference.affected_rank_before,
				...(receipt.posterior_update?.policy_snapshot_id !== undefined
					? { enqueue_policy_snapshot_id: receipt.posterior_update.policy_snapshot_id }
					: {}),
				...(receipt.posterior_update?.posterior_version_after !== undefined
					? { enqueue_posterior_version: receipt.posterior_update.posterior_version_after }
					: {})
			} : null;
			const key = reference?.job_id ?? `${reference?.enqueue_status ?? 'disabled'}:${receipt.outcome_id}`;
			const tracked: TrackedAttentionRankRecompute = {
				key,
				scopeKey: currentScopeKey,
				acceptedAt,
				expiresAt: acceptedAt + RECENT_WINDOW_MS,
				reference,
				binding,
				status: reference === null
					? 'disabled'
					: reference.enqueue_status === 'failed'
						? 'enqueue_failed'
						: reference.job_status ?? 'dead',
				job: null,
				pollAttempts: 0,
				isPolling: false,
				error: reference?.enqueue_status === 'failed' ? 'enqueue_failed' : null
			};
			update((state) => ({
				...state,
				jobs: [tracked, ...state.jobs.filter((job) => job.key !== key && job.expiresAt > acceptedAt)]
					.slice(0, MAX_RECENT_JOBS)
			}));
			pruneUntrackedTimers();
			scheduleExpiry(key, tracked.expiresAt);
			void refreshHealth();
			schedule(key);
		},
		refreshHealth,
		clear(): void {
			generation += 1;
			clearAllTimers();
			consumers.clear();
			set(initialState);
		}
	};
}

export const attentionRankRecomputeStore = createAttentionRankRecomputeStore();
