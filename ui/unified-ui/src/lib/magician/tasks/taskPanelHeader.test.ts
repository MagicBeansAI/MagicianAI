import { describe, expect, it } from 'vitest';

import {
	HEADER_CONDENSE_AT_PX,
	HEADER_EXPAND_AT_PX,
	headerChips,
	nextHeaderCondensed
} from './taskPanelHeader';
import type { TaskPanelModel } from './UnifiedTaskPanel.svelte';

/** A model with nothing in it but the fields the header reads. */
function model(overrides: Partial<TaskPanelModel> = {}): TaskPanelModel {
	return {
		id: 'task-1',
		status: 'running',
		queuedFor: null,
		attention: null,
		error: null,
		currentStep: null,
		totalSteps: null,
		currentStepLabel: null,
		elapsedMs: null,
		lastProgressAt: null,
		ask: null,
		plan: null,
		run: null,
		output: null,
		runs: null,
		...overrides
	};
}

describe('the header condenses on scroll and stays condensed', () => {
	it('is expanded at the top', () => {
		expect(nextHeaderCondensed(false, 0)).toBe(false);
	});

	it('does not condense before the threshold', () => {
		expect(nextHeaderCondensed(false, HEADER_CONDENSE_AT_PX - 1)).toBe(false);
	});

	it('condenses at the threshold', () => {
		expect(nextHeaderCondensed(false, HEADER_CONDENSE_AT_PX)).toBe(true);
	});

	/**
	 * **The oscillation guard, and the only reason there are two thresholds.**
	 *
	 * Condensing removes the description row, which shortens the content; the
	 * browser's scroll anchoring then pulls the body back up, often across the same
	 * line that just fired. With one threshold the header expands, the row returns,
	 * the body drops again — a flicker the reader sees and nothing in the code
	 * points at. So the state has to survive a scroll position *below* the one that
	 * set it.
	 */
	it('stays condensed well below the point it condensed at', () => {
		const justBelow = HEADER_CONDENSE_AT_PX - 32;
		expect(justBelow).toBeGreaterThan(HEADER_EXPAND_AT_PX);
		expect(nextHeaderCondensed(true, justBelow)).toBe(true);
	});

	it('expands only once the reader is back at the top', () => {
		expect(nextHeaderCondensed(true, HEADER_EXPAND_AT_PX + 1)).toBe(true);
		expect(nextHeaderCondensed(true, HEADER_EXPAND_AT_PX)).toBe(false);
	});

	it('leaves a gap between the two, because a gap is the mechanism', () => {
		expect(HEADER_EXPAND_AT_PX).toBeLessThan(HEADER_CONDENSE_AT_PX);
	});

	/**
	 * A negative `scrollTop` is what an over-scrolled body reports on iOS, and
	 * `NaN` is what an element measured before layout reports. Both mean the reader
	 * is at the top, and neither may leave the header condensed.
	 */
	it('reads an impossible scroll position as the top', () => {
		expect(nextHeaderCondensed(true, -200)).toBe(false);
		expect(nextHeaderCondensed(true, Number.NaN)).toBe(false);
		expect(nextHeaderCondensed(false, Number.POSITIVE_INFINITY)).toBe(false);
	});
});

describe('the header chips say what the verdict below them does not', () => {
	it('renders nothing without a task', () => {
		expect(headerChips(null)).toEqual([]);
	});

	it('carries the status as a word, in the house tone for it', () => {
		expect(headerChips(model({ status: 'running' }))).toEqual([
			{ id: 'status', label: 'Running', tone: 'running' }
		]);
	});

	it('humanises a wire status rather than rendering the enum', () => {
		expect(headerChips(model({ status: 'needs_input' }))[0].label).toBe('Needs Input');
	});

	/**
	 * A status this app maps to `neutral` gets **no** tone rather than a wrong one,
	 * and `Badge` renders that as its default grey. The mapping lives in
	 * `$lib/shared/statusTone` and this asserts the panel does not second-guess it.
	 */
	it('leaves a neutral status without a colour', () => {
		expect(headerChips(model({ status: 'pending' }))[0].tone).toBeNull();
	});

	it('adds the plan’s own status, which no other line in a closed panel carries', () => {
		const chips = headerChips(
			model({
				plan: { status: 'draft', approvedAt: null, steps: [], questions: [], provenance: [] }
			})
		);

		expect(chips.map((chip) => chip.label)).toEqual(['Running', 'Plan: Draft']);
		// Prefixed, because `Draft` beside `Running` is two words of the same shape
		// with nothing saying which is which.
		expect(chips[1]).toEqual({ id: 'plan', label: 'Plan: Draft', tone: null });
	});

	it('omits the plan chip when the act carries no status the client recognises', () => {
		const chips = headerChips(
			model({
				plan: { status: null, approvedAt: null, steps: [], questions: [], provenance: [] }
			})
		);

		expect(chips.map((chip) => chip.id)).toEqual(['status']);
	});

	it('omits it entirely when the task has no Plan act', () => {
		expect(headerChips(model({ plan: null })).map((chip) => chip.id)).toEqual(['status']);
	});

	/**
	 * **The constraint that keeps the row from becoming noise.** The verdict line
	 * immediately below already reads `Running · step 3 of 7 · 4m`, so a chip
	 * carrying a step position or an elapsed time would be the same fact at two
	 * sizes — which is design §2's "nothing from level N+1 appears at level N",
	 * applied upward.
	 */
	it('never carries a fact the verdict headline already renders', () => {
		const chips = headerChips(
			model({
				status: 'running',
				currentStep: 3,
				totalSteps: 7,
				elapsedMs: 240_000,
				plan: { status: 'approved', approvedAt: 1, steps: ['a'], questions: [], provenance: [] }
			})
		);

		const text = chips.map((chip) => chip.label).join(' ');
		expect(text).not.toMatch(/step/i);
		expect(text).not.toMatch(/\d+m/);
		expect(chips).toHaveLength(2);
	});

	it('gives every chip a distinct id, so a keyed each keeps identity across polls', () => {
		const chips = headerChips(
			model({
				plan: { status: 'draft', approvedAt: null, steps: [], questions: [], provenance: [] }
			})
		);

		expect(new Set(chips.map((chip) => chip.id)).size).toBe(chips.length);
	});
});
