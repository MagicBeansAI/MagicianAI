/**
 * Model names for the activity card's LLM rows.
 *
 * `llm.requested` names the model the turn ASKED for. When an external
 * harness (e.g. Claude Code) answers a chat turn, the server stamps the
 * human engine label there ("Claude Code · opus") plus `engine` /
 * `harness_model`; older servers stamped the native profile name, which
 * then read as the model that answered when another engine did. The
 * settled event (`llm.succeeded` / `LLMResponseReceived`) names the model
 * that actually answered, so the row's label follows it once it lands.
 */

type Payload = Record<string, unknown> | undefined | null;

function text(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value.trim() : null;
}

/** The label a running row shows: requested model, else capability. */
export function requestedModelLabel(payload: Payload): string {
	return text(payload?.model) ?? text(payload?.capability) ?? 'model';
}

/**
 * The model that answered, from a settled LLM event, or `null` when the
 * event names none (then the running label stays as-is).
 */
export function answeredModelLabel(payload: Payload): string | null {
	return text(payload?.model) ?? text(payload?.harness_model) ?? text(payload?.provider);
}

/**
 * The label of a model call that is in flight but not yet routed. A typed
 * `LLMRequestSent` names only the capability ("decision"), which is an
 * operation rather than a model, so the row says it is thinking and names the
 * model once `LLMRouted` (or the settled event) does.
 */
export const THINKING_LABEL = 'Thinking…';

/**
 * The model `LLMRouted` says the router chose, or `null` when it names none.
 * The model id first; the profile, then the provider, for a router that only
 * names those. A fallback re-route sends a second `LLMRouted` with the new
 * model, and the row follows it.
 */
export function routedModelLabel(payload: Payload): string | null {
	return text(payload?.model) ?? text(payload?.profile) ?? text(payload?.provider);
}

/**
 * The per-call id that pairs an LLM call's events: `llm_call_id` on the
 * chat-side `llm.*` events, `correlation.llm_call_id` on the typed
 * `LLMRequestSent` / `LLMRouted` / `LLMResponseReceived`. `null` for a server
 * that predates it, and the hosts then fall back to their older keys.
 */
export function llmCallIdOf(payload: Payload): string | null {
	const correlation = payload?.correlation;
	const nested =
		correlation && typeof correlation === 'object'
			? (correlation as Record<string, unknown>).llm_call_id
			: undefined;
	return text(payload?.llm_call_id) ?? text(nested);
}
