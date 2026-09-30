/**
 * `timedFetch` — `fetch` with a default timeout.
 *
 * Bare `fetch` has no timeout, so a request that never gets sent (e.g.
 * because the browser's per-origin HTTP/1.1 connection pool is saturated by
 * long-lived SSE/WS streams) will hang forever. Symptom across the UI:
 * spinners that never resolve and tabs that can't even be refreshed because
 * the JS event loop is waiting on a never-arriving response.
 *
 * Default timeout is 30 seconds — chosen because steady-state magician API
 * calls are tens of milliseconds; the only legitimate stretches happen when
 * a per-session lock is held by an in-flight LLM turn, in which case 30s is
 * a sensible "give up, surface error" boundary. Pass `timeoutMs` to override
 * (operator-instant endpoints like cancel-run typically use 10s; long-poll
 * style endpoints can pass a larger value).
 *
 * If the caller already passes an `AbortSignal` (e.g. a scope-switch cancel
 * token), it's composed with the timeout via `AbortSignal.any` so either can
 * trigger abort.
 *
 * When the timeout fires, `fetch` rejects with a `DOMException` whose `name`
 * is `'TimeoutError'`. Call sites that want to render a custom error message
 * to the user should match on that name rather than treating it as a generic
 * network failure.
 */

export const DEFAULT_FETCH_TIMEOUT_MS = 30_000;

/**
 * Long-running endpoint timeout — 10 minutes.
 *
 * Use for connections where the server is expected to hold the request
 * for a full LLM-bound turn: chat-send POSTs, the SSE streaming
 * endpoint, etc. The default 30s aborts mid-turn when the LLM + tool
 * dispatch loop legitimately runs longer, which manifested as
 * "assistant response doesn't show until refresh" because the abort
 * killed the response delivery before the body arrived.
 */
export const LONG_FETCH_TIMEOUT_MS = 600_000;

export function timedFetch(
    input: RequestInfo | URL,
    init?: RequestInit & { timeoutMs?: number }
): Promise<Response> {
    if (typeof window !== 'undefined' && (window as any).__MAGICIAN_MISSING__) {
        return Promise.reject(new Error("Magician backend is missing (Marketing Mode)."));
    }
    const { timeoutMs, signal: existing, ...rest } = init ?? {};
    const timeoutSignal = AbortSignal.timeout(timeoutMs ?? DEFAULT_FETCH_TIMEOUT_MS);
    const signal = existing
        ? AbortSignal.any([existing, timeoutSignal])
        : timeoutSignal;
    return fetch(input, { ...rest, signal });
}
