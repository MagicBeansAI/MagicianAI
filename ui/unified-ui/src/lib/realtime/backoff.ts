// Shared reconnect/poll backoff with full jitter.
//
// Why jitter: a backend restart disconnects EVERY client at the same
// instant; fixed delays make them all reconnect in lockstep, hammering
// the server in synchronized waves (and, in dev, flooding the vite
// proxy log — a dead backend produced ~19k reconnect attempts in 90
// minutes before this existed). Randomizing each delay to 50–100% of
// the current step spreads the herd.
//
// Usage: one instance per connection/poller (state is per-instance).
//   const backoff = createBackoff({ initialMs: 1_000, maxMs: 60_000 });
//   ...on failure: await sleep(backoff.nextMs());
//   ...on success: backoff.reset();

export interface Backoff {
	/** Next delay: 50–100% of the current step, then double the step (capped). */
	nextMs(): number;
	/** Back to the initial step (call when data flows again). */
	reset(): void;
}

export function createBackoff(opts: { initialMs: number; maxMs: number }): Backoff {
	let stepMs = opts.initialMs;
	return {
		nextMs(): number {
			const jittered = Math.round(stepMs / 2 + Math.random() * (stepMs / 2));
			stepMs = Math.min(stepMs * 2, opts.maxMs);
			return jittered;
		},
		reset(): void {
			stepMs = opts.initialMs;
		}
	};
}
