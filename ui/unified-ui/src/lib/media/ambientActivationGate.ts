export interface AmbientActivationGate {
	/** Admit at most one bounded turn; repeated wake events coalesce. */
	admit(): void;
	/** Resolve only when explicitly admitted or cancelled. */
	wait(): Promise<boolean>;
	/** Release a pending waiter and reject every later admission. */
	cancel(): void;
}

/**
 * A one-permit asynchronous gate for Ambient Dictation. It deliberately has no
 * timers or recursive continuation: one waiter is resolved, then the caller's
 * existing iterative turn loop advances with constant stack depth.
 */
export function createAmbientActivationGate(): AmbientActivationGate {
	let permitted = false;
	let cancelled = false;
	let waiter: ((admitted: boolean) => void) | null = null;

	return {
		admit(): void {
			if (cancelled || permitted) return;
			if (waiter) {
				const resolve = waiter;
				waiter = null;
				resolve(true);
				return;
			}
			permitted = true;
		},
		wait(): Promise<boolean> {
			if (cancelled) return Promise.resolve(false);
			if (permitted) {
				permitted = false;
				return Promise.resolve(true);
			}
			return new Promise<boolean>((resolve) => {
				waiter = resolve;
			});
		},
		cancel(): void {
			if (cancelled) return;
			cancelled = true;
			permitted = false;
			if (waiter) {
				const resolve = waiter;
				waiter = null;
				resolve(false);
			}
		}
	};
}
