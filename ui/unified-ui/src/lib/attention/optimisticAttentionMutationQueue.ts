import { writable } from 'svelte/store';

export interface OptimisticAttentionMutationResult {
	ok: boolean;
	error?: string;
}

export type OptimisticAttentionMutationStatus = 'pending' | 'committed';

export interface OptimisticAttentionMutationState {
	statusByKey: ReadonlyMap<string, OptimisticAttentionMutationStatus>;
}

const COMMITTED_GRACE_MS = 120_000;

function normalizeFailure(error: unknown): OptimisticAttentionMutationResult {
	return {
		ok: false,
		error: error instanceof Error ? error.message : String(error)
	};
}

function createOptimisticAttentionMutationQueue() {
	const { subscribe, set } = writable<OptimisticAttentionMutationState>({
		statusByKey: new Map()
	});
	const statusByKey = new Map<string, OptimisticAttentionMutationStatus>();
	const inFlight = new Map<string, Promise<OptimisticAttentionMutationResult>>();
	const expiryTimers = new Map<string, ReturnType<typeof setTimeout>>();
	let generation = 0;

	function publish(): void {
		set({ statusByKey: new Map(statusByKey) });
	}

	function clearExpiry(key: string): void {
		const timer = expiryTimers.get(key);
		if (timer !== undefined) clearTimeout(timer);
		expiryTimers.delete(key);
	}

	function release(key: string): void {
		clearExpiry(key);
		statusByKey.delete(key);
		publish();
	}

	function commit(key: string, graceMs = COMMITTED_GRACE_MS): void {
		statusByKey.set(key, 'committed');
		publish();
		clearExpiry(key);
		expiryTimers.set(
			key,
			setTimeout(() => release(key), Math.max(0, graceMs))
		);
	}

	function enqueue<T extends OptimisticAttentionMutationResult>(
		key: string,
		execute: () => Promise<T>
	): Promise<T> {
		const existing = inFlight.get(key);
		if (existing) return existing as Promise<T>;
		const requestGeneration = generation;

		clearExpiry(key);
		statusByKey.set(key, 'pending');
		publish();

		let execution: Promise<OptimisticAttentionMutationResult>;
		try {
			// Invoke transport in the same interaction stack, after publishing the
			// tombstone. Besides preserving true eager UI, this freezes scoped
			// request headers before a subsequent workspace switch can retarget it.
			execution = Promise.resolve(execute());
		} catch (error) {
			execution = Promise.resolve(normalizeFailure(error));
		}

		const queued: Promise<OptimisticAttentionMutationResult> = execution
			.catch(normalizeFailure)
			.then((result) => {
				if (requestGeneration !== generation || inFlight.get(key) !== queued) return result;
				inFlight.delete(key);
				if (result.ok) commit(key);
				else release(key);
				return result;
			});
		inFlight.set(key, queued);
		return queued as Promise<T>;
	}

	return {
		subscribe,
		enqueue,
		isSuppressed(key: string): boolean {
			return statusByKey.has(key);
		},
		status(key: string): OptimisticAttentionMutationStatus | null {
			return statusByKey.get(key) ?? null;
		},
		clear(key: string): void {
			inFlight.delete(key);
			release(key);
		},
		reset(): void {
			generation += 1;
			for (const timer of expiryTimers.values()) clearTimeout(timer);
			expiryTimers.clear();
			inFlight.clear();
			statusByKey.clear();
			publish();
		}
	};
}

export interface OptimisticAttentionMutationScope {
	principal: string;
	workspace: string;
}

function scopedMutationKey(
	kind: 'follow_up' | 'worth_a_look',
	id: string,
	scope: OptimisticAttentionMutationScope
): string {
	// JSON framing avoids delimiter collisions while keeping the opaque key
	// stable for every owner rendering the same scoped server entity.
	return JSON.stringify([kind, scope.principal, scope.workspace, id]);
}

export function followUpAttentionMutationKey(
	annotationId: string,
	scope: OptimisticAttentionMutationScope
): string {
	return scopedMutationKey('follow_up', annotationId, scope);
}

export function worthAttentionMutationKey(
	candidateId: string,
	scope: OptimisticAttentionMutationScope
): string {
	return scopedMutationKey('worth_a_look', candidateId, scope);
}

export function countScopedOptimisticMutations(
	statusByKey: ReadonlyMap<string, OptimisticAttentionMutationStatus>,
	kind: 'follow_up' | 'worth_a_look',
	scope: OptimisticAttentionMutationScope
): number {
	let count = 0;
	for (const key of statusByKey.keys()) {
		try {
			const parsed = JSON.parse(key) as unknown;
			if (
				Array.isArray(parsed) &&
				parsed[0] === kind &&
				parsed[1] === scope.principal &&
				parsed[2] === scope.workspace
			) {
				count += 1;
			}
		} catch {
			// Opaque keys that are not ours stay out of the badge math.
		}
	}
	return count;
}

export const optimisticAttentionMutationQueue = createOptimisticAttentionMutationQueue();
