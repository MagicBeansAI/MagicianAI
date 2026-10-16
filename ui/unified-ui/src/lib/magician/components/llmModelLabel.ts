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
