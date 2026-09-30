import { get, writable } from 'svelte/store';

export type ScreenLockState = 'unknown' | 'locked' | 'unlocked' | 'unsupported';

interface IdleDetectorLike extends EventTarget {
	screenState?: string | null;
	start(options: { threshold: number; signal: AbortSignal }): Promise<void>;
}

interface IdleDetectorConstructorLike {
	new (): IdleDetectorLike;
	requestPermission?: () => Promise<string>;
}

type IdleGlobal = typeof globalThis & { IdleDetector?: IdleDetectorConstructorLike };

interface IdleDetectionPermissions {
	query(descriptor: { name: 'idle-detection' }): Promise<PermissionStatus>;
}

export const screenLockStateStore = writable<ScreenLockState>('unknown');

let detector: IdleDetectorLike | null = null;
let detectorAbort: AbortController | null = null;
let detectorChangeListener: (() => void) | null = null;
let lifecycleGeneration = 0;

interface StartAttempt {
	generation: number;
	requestPermission: boolean;
	abort: AbortController;
	promise: Promise<ScreenLockState>;
}

let startAttempt: StartAttempt | null = null;

export function normalizeIdleScreenState(value: unknown): ScreenLockState {
	if (value === 'locked') return 'locked';
	if (value === 'unlocked') return 'unlocked';
	return 'unknown';
}

function idleDetectorConstructor(): IdleDetectorConstructorLike | null {
	return (globalThis as IdleGlobal).IdleDetector ?? null;
}

async function currentPermission(
	ctor: IdleDetectorConstructorLike,
	requestPermission: boolean
): Promise<string> {
	if (requestPermission && ctor.requestPermission) {
		try {
			return await ctor.requestPermission();
		} catch {
			return 'prompt';
		}
	}
	if (typeof navigator === 'undefined' || !navigator.permissions?.query) return 'prompt';
	try {
		// Idle Detection is a Chromium API whose permission descriptor is not yet
		// included in TypeScript's cross-browser `PermissionName` union. Keep the
		// compatibility cast at this experimental browser boundary.
		const permissions = navigator.permissions as unknown as IdleDetectionPermissions;
		const status = await permissions.query({ name: 'idle-detection' });
		return status.state;
	} catch {
		return 'prompt';
	}
}

/**
 * Start exact OS screen-lock observation where Chromium exposes Idle Detection.
 * A hidden tab is deliberately not treated as a locked screen. The permission
 * prompt is requested only from a user gesture (the Voice control pointer-down).
 */
export async function startScreenLockMonitoring(
	requestPermission = false
): Promise<ScreenLockState> {
	if (detector) {
		const current = normalizeIdleScreenState(detector.screenState);
		screenLockStateStore.set(current);
		return current;
	}
	if (startAttempt) {
		if (!requestPermission || startAttempt.requestPermission) {
			return startAttempt.promise;
		}
		// A passive mount-time permission query must never consume/coalesce the
		// later user gesture that is allowed to prompt. Invalidate it and upgrade
		// to a gesture-bearing attempt; its eventual completion cannot publish.
		lifecycleGeneration += 1;
		startAttempt.abort.abort();
		startAttempt = null;
	}

	const generation = lifecycleGeneration + 1;
	lifecycleGeneration = generation;
	const attemptAbort = new AbortController();
	const isCurrentAttempt = () =>
		lifecycleGeneration === generation && !attemptAbort.signal.aborted;
	const promise = (async () => {
		const ctor = idleDetectorConstructor();
		if (!ctor) {
			if (isCurrentAttempt()) screenLockStateStore.set('unsupported');
			return 'unsupported';
		}
		const permission = await currentPermission(ctor, requestPermission);
		if (!isCurrentAttempt()) return get(screenLockStateStore);
		if (permission !== 'granted') {
			screenLockStateStore.set('unknown');
			return 'unknown';
		}
		const next = new ctor();
		const publish = () => {
			if (isCurrentAttempt()) {
				screenLockStateStore.set(normalizeIdleScreenState(next.screenState));
			}
		};
		next.addEventListener('change', publish);
		try {
			await next.start({ threshold: 60_000, signal: attemptAbort.signal });
			if (!isCurrentAttempt()) {
				next.removeEventListener('change', publish);
				return get(screenLockStateStore);
			}
			detector = next;
			detectorAbort = attemptAbort;
			detectorChangeListener = publish;
			publish();
			return get(screenLockStateStore);
		} catch {
			next.removeEventListener('change', publish);
			attemptAbort.abort();
			if (lifecycleGeneration === generation) {
				screenLockStateStore.set('unknown');
			}
			return get(screenLockStateStore);
		}
	})().finally(() => {
		if (startAttempt?.generation === generation) startAttempt = null;
	});
	startAttempt = { generation, requestPermission, abort: attemptAbort, promise };
	return promise;
}

export function isScreenLocked(): boolean {
	return get(screenLockStateStore) === 'locked';
}

/** Test/lifecycle seam. Production monitoring normally lives for the page. */
export function stopScreenLockMonitoring(): void {
	lifecycleGeneration += 1;
	startAttempt?.abort.abort();
	startAttempt = null;
	if (detector && detectorChangeListener) {
		detector.removeEventListener('change', detectorChangeListener);
	}
	detectorAbort?.abort();
	detectorAbort = null;
	detectorChangeListener = null;
	detector = null;
	screenLockStateStore.set('unknown');
}
