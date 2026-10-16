/**
 * Interpretation progress — what the facilitator is doing, per stage.
 *
 * The backend narrates each owner-triggered `/interpret` run as
 * `ThinkingMapInterpretProgress` events whose stages are bound to real steps
 * of its pipeline (`preparing` → `loading_context` → `facilitating` →
 * `parsing` → `shaping`, then a terminal `idle`). This module owns the two
 * pure decisions every client makes about them, so both are testable without
 * a socket:
 *
 * - **wording** — one label + detail per stage, matching iOS and Android so
 *   the same wait reads the same on every surface;
 * - **routing** — whether an event belongs on this page's strip at all.
 *
 * The vocabulary is deliberately closed. iOS once declared labels for stages
 * nothing emitted (`openingThread`, `grounding`) and they sat unset for a
 * year; a stage earns an entry here by being emitted, not by sounding likely.
 */

export type InterpretStage =
	| 'preparing'
	| 'loading_context'
	| 'facilitating'
	| 'parsing'
	| 'shaping'
	| 'idle';

const STAGES: ReadonlySet<string> = new Set([
	'preparing',
	'loading_context',
	'facilitating',
	'parsing',
	'shaping',
	'idle'
]);

/** A recognised stage, or null for one this build predates. */
export function parseInterpretStage(stage: string): InterpretStage | null {
	return STAGES.has(stage) ? (stage as InterpretStage) : null;
}

export interface InterpretStageLine {
	label: string;
	detail: string;
}

/**
 * The line shown for a stage. `preparing` folds in how many thoughts the
 * facilitator said it was reading — the number is what makes a several-second
 * wait feel like motion. No count (or an empty board) falls back to the
 * generic line: "Reading 0 thoughts" reads as a bug, not a board.
 */
export function interpretStageLine(stage: InterpretStage, nodeCount?: number | null): InterpretStageLine {
	switch (stage) {
		case 'preparing':
			return {
				label: 'Reading map',
				detail:
					nodeCount && nodeCount > 0
						? `Reading ${nodeCount} thought${nodeCount === 1 ? '' : 's'} and the active branch…`
						: 'Reading the board and the active branch…'
			};
		case 'loading_context':
			return {
				label: 'Loading context',
				detail: 'Giving the facilitator the graph slice and the move budget…'
			};
		case 'facilitating':
			return {
				label: 'Exploring',
				detail: 'Finding the few directions that would change the next minute of thinking…'
			};
		case 'parsing':
			return { label: 'Reading back', detail: 'Reading the facilitator’s answer back…' };
		case 'shaping':
			return { label: 'Shaping moves', detail: 'Removing repeats and shaping the strongest cards…' };
		case 'idle':
			return { label: 'Ready', detail: 'Ready for another direction.' };
	}
}

/**
 * Whether a narrated stage belongs on this page's strip.
 *
 * Four gates, each refusing a real hazard:
 * - **another map's run** — the bus is shared; two maps thinking at once must
 *   not drive each other's strip;
 * - **a run this page did not start** — ambient auto-maps and other devices
 *   narrate against the same map id; only the utterance id minted for THIS
 *   request is ours;
 * - **a stage this build has never heard of** — keep the current line rather
 *   than guess, which is what lets the vocabulary grow server-first;
 * - **the terminal `idle`** — settling is the HTTP response's job (it also
 *   owns error handling), and a realtime idle can outrun the response body;
 *   clearing early would show "Ready" over a run still being applied.
 */
export function interpretProgressApplies(
	event: { map_id: string; utterance_id: string; stage: string },
	openMapId: string | null,
	inFlightUtteranceId: string | null
): boolean {
	const stage = parseInterpretStage(event.stage);
	return (
		stage !== null &&
		stage !== 'idle' &&
		openMapId !== null &&
		event.map_id === openMapId &&
		inFlightUtteranceId !== null &&
		event.utterance_id === inFlightUtteranceId
	);
}
