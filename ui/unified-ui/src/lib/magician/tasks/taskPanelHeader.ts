/**
 * The two decisions the task panel's header makes, as functions.
 *
 * Both are here rather than inside `TaskPanelDrawer.svelte` for the reason every
 * other module in this folder is: they are the parts a test can hold still. The
 * header's *layout* is four rows of markup and a visual pass; whether it should
 * be condensed, and which chips it carries, are answers over inputs.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`, *The drawer shell*.
 */

import { titleCase } from '$lib/feed/learningCards';
import type { BadgeStatusTone } from '$lib/shared/statusTone';
import { statusBadgeTone } from '$lib/shared/statusTone';

import type { TaskPanelModel } from './UnifiedTaskPanel.svelte';

/**
 * How far the body must scroll before the header condenses, and how far back
 * before it expands again.
 *
 * **Two thresholds, and the gap between them is the whole point.** A single
 * threshold self-oscillates: condensing removes the description row, the browser's
 * scroll anchoring pulls the body back up across the same line, the header expands,
 * the row returns, and the reader watches it flicker. Separate enter and exit
 * points make the state sticky through the layout shift the state change itself
 * causes.
 *
 * Recovered from `deepwork/executionPanelScroll.ts`, which was deleted with
 * `ExecutionPanel.svelte` — its numbers and its hysteresis, because the mechanism
 * was sound and the oscillation it guards against is not hypothetical.
 */
export const HEADER_CONDENSE_AT_PX = 72;
export const HEADER_EXPAND_AT_PX = 12;

/**
 * Whether the header should be condensed, given whether it is now and how far
 * the body has scrolled.
 *
 * A non-finite or negative `scrollTop` reads as `0`. That is not defensiveness
 * about a number the DOM cannot produce: an over-scrolled body on iOS reports a
 * negative one, and `NaN` arrives whenever the element is measured before layout.
 * Both mean "at the top".
 */
export function nextHeaderCondensed(currentlyCondensed: boolean, scrollTop: number): boolean {
	const top = Number.isFinite(scrollTop) ? Math.max(0, scrollTop) : 0;
	return currentlyCondensed ? top > HEADER_EXPAND_AT_PX : top >= HEADER_CONDENSE_AT_PX;
}

/**
 * One chip in the header's fourth row.
 *
 * `tone` is `null` for a chip with no status colour, which `Badge` renders in its
 * default grey — never a colour chosen here. Status → colour lives in
 * `$lib/shared/statusTone` and nowhere else, which is a rule that file states
 * about itself.
 */
export interface TaskPanelChip {
	/** Stable across polls, so a keyed `{#each}` keeps a chip's identity. */
	id: string;
	label: string;
	tone: BadgeStatusTone | null;
}

/**
 * The chips a task's header carries, in order.
 *
 * **Derived from the model, so every surface gets them without wiring.** Seven
 * surfaces mount this drawer; a `chips` prop would mean seven places to pass one
 * and six of them showing nothing until someone remembered. The model already
 * carries both facts.
 *
 * **Two chips, and neither restates the verdict.** That constraint is what keeps
 * this from being noise: the verdict line directly below already says the state as
 * a sentence with its step position and elapsed time in it, so a chip repeating any
 * of that would be the same fact at two sizes. What is left is the status *as a
 * word* — which is what the reader scanning a row of chips is matching against the
 * list they came from — and the plan's own status, which no other line in the
 * closed panel carries.
 *
 * The plan chip is absent rather than empty when there is no Plan act or the act
 * carries no status the client recognises, on the same absent-not-greyed rule the
 * acts follow.
 */
export function headerChips(model: TaskPanelModel | null): TaskPanelChip[] {
	if (model === null) return [];

	const chips: TaskPanelChip[] = [
		{ id: 'status', label: titleCase(model.status), tone: statusBadgeTone(model.status) }
	];

	const planStatus = model.plan?.status ?? null;
	if (planStatus !== null) {
		// `Plan: Draft` rather than a bare `Draft`, which beside a task status chip
		// would be two words of the same shape with nothing saying which is which.
		chips.push({ id: 'plan', label: `Plan: ${titleCase(planStatus)}`, tone: null });
	}

	return chips;
}
