/**
 * "Explain this deeper" — turning one storyboard step into a follow-up ask.
 *
 * The tutor plans a lesson by guessing where a learner will get stuck. It can
 * guess wrong, and no contract rule fixes that: the runtime can force the hard
 * step to be DECLARED and decomposed, but it cannot know which step was
 * actually hard for this person. This is the correction channel — the user
 * points at the step that did not land, and the system stops guessing.
 *
 * It deliberately composes a normal chat turn rather than mutating the run in
 * flight. The `/tutor/user-action` channel exists but means something else
 * entirely (the user PERFORMED an action App Copilot was about to automate, and
 * it can preempt a queued execution), and a completed run has completion
 * invariants that reopening would have to unwind. A fresh turn reuses the whole
 * pipeline — plan, roles, figure-backed coverage — and works identically while
 * a lesson is live and long afterwards during replay.
 */

export interface DeeperStep {
	/** The storyboard step id, so the ask names one milestone and not the lesson. */
	revealId?: string;
	label?: string;
	/** What was already said. Included so the model can avoid repeating it. */
	narration?: string;
}

export interface DeeperRequest {
	sessionId: string;
	prompt: string;
}

export type DeeperRequestFetch = (
	input: RequestInfo | URL,
	init?: RequestInit
) => Promise<Response>;

/**
 * Send the already-composed follow-up through the canonical chat-message
 * contract. Keeping this boundary beside the composer makes the field name
 * (`text`, never the legacy-looking `content`) directly testable instead of
 * hiding it inside a Svelte click handler.
 */
export async function postDeeperRequest(
	endpoint: string,
	request: DeeperRequest,
	headers: HeadersInit,
	fetcher: DeeperRequestFetch = globalThis.fetch
): Promise<void> {
	const response = await fetcher(endpoint, {
		method: 'POST',
		headers,
		body: JSON.stringify({
			text: request.prompt,
			source_surface: 'tutor_draw_overlay'
		})
	});
	if (response.ok) return;

	let detail = '';
	try {
		detail = (await response.text()).trim();
	} catch {
		// The status remains actionable even when the response body is unreadable.
	}
	throw new Error(
		`Explain-deeper request failed with HTTP ${response.status}${detail ? `: ${detail}` : ''}`
	);
}

/**
 * Whether the control can be offered.
 *
 * **Only a session is required.** An earlier version also demanded a nameable
 * step, on the theory that deepening "it" makes the model guess. That reasoning
 * was borrowed from controls where being wrong is expensive — a misplaced
 * Dismiss loses the lesson. Here the worst case of showing the button is that
 * someone clicks it and gets more explanation, which is the thing they wanted.
 * A learner can want more detail at any moment, including one the overlay has
 * no current step for, so naming the step is an OPTIMISATION of the ask rather
 * than a precondition for it.
 */
export function canRequestDeeper(
	_step: DeeperStep | null | undefined,
	sessionId: string | null | undefined
): boolean {
	return Boolean(sessionId && sessionId.trim());
}

/** Collapse whitespace so a multi-line narration reads as one quoted sentence. */
function flatten(text: string): string {
	return text.replace(/\s+/g, ' ').trim();
}

/**
 * The prompt sent for a deepen request.
 *
 * Three things it must carry, each for a reason:
 *
 * 1. **`@tutor`** — the rail is chosen by invoke word. Without it this is an
 *    ordinary chat turn and draws nothing.
 * 2. **The step's own words** — so the model decomposes THAT milestone instead
 *    of re-teaching the lesson from the top.
 * 3. **What was already said** — the storyboard contract requires a second
 *    explanation to change representation rather than volume, and the model can
 *    only avoid repeating narration it can see.
 */
export function buildDeeperPrompt(step: DeeperStep): string {
	const label = step.label?.trim();
	const narration = step.narration?.trim();
	// With no step to name, deepen the most recent explanation rather than
	// refusing: the user asked for more detail, and "which part" is answerable
	// from the lesson the model just gave.
	const named = label ? `the step "${flatten(label)}"` : 'the part you just explained';

	const parts = [`@tutor go deeper on ${named}.`];
	if (narration) {
		parts.push(`So far you explained it as: "${flatten(narration)}".`);
	}
	parts.push(
		'That was not enough. Break this one step into its sub-steps and draw the ' +
			'intermediate stages you skipped, rather than restating the same idea in more words. ' +
			'Show it — a figure changing, the quantity being matched, the region being shaded — ' +
			'and keep the rest of the lesson as it was.'
	);
	return parts.join(' ');
}

/**
 * The request to POST, or `null` when there is no session to receive it.
 *
 * Returning `null` rather than throwing keeps the caller a plain click handler:
 * the control is hidden by `canRequestDeeper` in the same conditions, so a
 * `null` here means the two disagreed and the safe move is to do nothing.
 */
export function buildDeeperRequest(
	step: DeeperStep | null | undefined,
	sessionId: string | null | undefined
): DeeperRequest | null {
	if (!canRequestDeeper(step, sessionId)) return null;
	return {
		sessionId: (sessionId as string).trim(),
		prompt: buildDeeperPrompt(step ?? {})
	};
}
