import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import TaskVerdictLine, { VERDICT_MARKER } from './TaskVerdictLine.svelte';
import type { Verdict, VerdictState } from './taskVerdict';

afterEach(cleanup);

/**
 * One fixture per state, in `deriveVerdict`'s priority order, with the copy
	 * design §3 tabulates. A `Record<VerdictState, Verdict>`, so a new state
 * fails to compile here rather than going untested — `STATES` below is read off
 * this object, and every sweep in the file runs over it.
 *
 * Four things the markup could confuse are kept apart, each pinned by an
 * assertion rather than left to coincidence:
 *
 * - `headline` and `detail` are both verdict text, so one test pins which
 *   element each lands in **and** their order; no headline here equals or
 *   contains any detail, so a swap cannot pass.
 * - `finished` is the **only** fixture with an empty `detail` — the shape
 *   `deriveVerdict` really produces — so the no-jump test cannot pass by
 *   landing on some other state's blank.
 * - every `headline` is distinct, so the sweeps cannot match the wrong render.
 * - the markers are glyphs and the states are words, so no marker assertion can
 *   accidentally be satisfied by the state name.
 */
const VERDICTS: Record<VerdictState, Verdict> = {
	waiting: {
		state: 'waiting',
		headline: 'Waiting on you · 4m',
		detail: 'Approve the plan before it can run'
	},
	failed: {
		state: 'failed',
		headline: 'Failed · at step 5 of 7',
		detail: "Couldn't read revenue.csv — file not found"
	},
	stalled: {
		state: 'stalled',
		headline: 'Stalled · no progress for 6m',
		detail: 'Still on step 4: searching memory'
	},
	running: {
		state: 'running',
		headline: 'Running · step 4 of 7',
		detail: 'Searching memory for "quarterly plan"'
	},
	paused: {
		state: 'paused',
		headline: 'Paused',
		detail: 'Ready to resume when you are'
	},
	cancelled: {
		state: 'cancelled',
		headline: 'Cancelled · after 1m 40s',
		detail: 'You stopped this at step 3'
	},
	queued: {
		state: 'queued',
		headline: 'Queued',
		detail: 'Waiting for a free slot'
	},
	archived: {
		state: 'archived',
		headline: 'Archived',
		detail: 'No longer active'
	},
	finished: {
		state: 'finished',
		headline: 'Finished · 3m 12s',
		// What `deriveVerdict` actually returns: the output summary fills this in
		// one layer up, so the component must survive having nothing here.
		detail: ''
	}
};

const STATES = Object.keys(VERDICTS) as VerdictState[];

/** The rendered root, which carries both derived treatments. */
function renderVerdict(state: VerdictState): HTMLElement {
	const { container } = render(TaskVerdictLine, { props: { verdict: VERDICTS[state] } });
	const root = container.querySelector<HTMLElement>('.verdict');
	if (!root) throw new Error(`no verdict rendered for ${state}`);
	return root;
}

const lines = (root: HTMLElement) => Array.from(root.querySelectorAll('p')).map((p) => p.textContent);

/** How many times a string appears in everything rendered, however it is split up. */
const occurrences = (needle: string) =>
	(document.body.textContent ?? '').split(needle).length - 1;

describe('TaskVerdictLine treatment — L0', () => {
	it('gives every state its own marker glyph, so two states never look alike with the colour taken away', () => {
		const markers = STATES.map((state) => {
			const marker = renderVerdict(state).querySelector('.verdict__marker');
			// The wiring: this state renders the glyph the map gives it.
			expect(marker?.textContent).toBe(VERDICT_MARKER[state]);
			return marker?.textContent;
		});

		// The load-bearing half, and the reason it is a set rather than seven
		// equality checks: the assertions above pass just as happily when two
		// states share a glyph. Severity bands are deliberately shared — waiting
		// and stalled are both `attention`, cancelled and queued are both
		// `neutral` — so the band cannot be the whole treatment, and this is what
		// says the glyph makes up the difference.
		//
		// This is also the greyscale claim, made structurally: a glyph is
		// character data in the DOM, so distinct glyphs remain distinct
		// shapes whatever happens to the colour.
		expect(new Set(markers).size).toBe(STATES.length);
	});

	it('keeps the marker out of the accessible name, so nobody hears the glyph read as punctuation', () => {
		expect(renderVerdict('failed').querySelector('.verdict__marker')).toHaveAttribute(
			'aria-hidden',
			'true'
		);
	});

	it('renders each state in its severity band, taken from the one status vocabulary the UI has', () => {
		// Literals, not the component's own map: a test importing `VERDICT_TONE`
		// would assert nothing but that the map equals itself.
		const bands = Object.fromEntries(
			STATES.map((state) => [state, renderVerdict(state).dataset.tone])
		);

		expect(bands).toEqual({
			waiting: 'attention',
			failed: 'failed',
			// A wedged run needs a human as much as a blocked one does, and it is
			// neither deliberate (`paused`) nor terminal (`failed`).
			stalled: 'attention',
			running: 'running',
			paused: 'neutral',
			// Not `failed`, which is where the canonical status map puts the
			// *status* `cancelled`: this line answers "is it okay?", and a task the
			// reader stopped on purpose is okay.
			cancelled: 'neutral',
			queued: 'neutral',
			archived: 'neutral',
			finished: 'completed'
		});
	});

	it('carries the state as an attribute, so a panel can address one verdict without matching its glyph', () => {
		// Deliberately trivial — it echoes the prop, and it is not the
		// distinctness proof. It exists because the two treatments above are a
		// five-band palette and a glyph, and neither is a stable programmatic
		// identity for "which verdict is this".
		expect(renderVerdict('stalled')).toHaveAttribute('data-verdict-state', 'stalled');
	});
});

describe('TaskVerdictLine lines', () => {
	it('leads with the headline and puts the meaning under it', () => {
		// Both are verdict text and only their positions say which is which, so
		// this fails three ways: a line dropped, the two swapped, and a third line
		// appearing between them.
		expect(lines(renderVerdict('failed'))).toEqual([
			VERDICTS.failed.headline,
			VERDICTS.failed.detail
		]);
	});

	it('renders the detail line even when the verdict has no detail, so the block does not resize under the reader', () => {
		const withDetail = lines(renderVerdict('running'));
		const withoutDetail = lines(renderVerdict('finished'));

		// Against a state that *has* a detail, so this cannot pass by both
		// rendering nothing.
		expect(withDetail).toEqual([VERDICTS.running.headline, VERDICTS.running.detail]);
		expect(withoutDetail).toEqual([VERDICTS.finished.headline, '']);

		// The element is the half this environment can see. The height it reserves
		// is a `min-height` in the component's stylesheet, which jsdom does not
		// apply — so this test asserts the line box exists to be reserved, and
		// nothing about its size.
		expect(withoutDetail).toHaveLength(withDetail.length);
	});
});

describe('TaskVerdictLine announcement', () => {
	it('announces both lines from one status region, so the meaning is never dropped from what is read', () => {
		renderVerdict('waiting');
		const region = screen.getByRole('status');

		// A region wrapping only the headline announces `Waiting on you · 4m` and
		// leaves out the thing the reader has to do.
		expect(region.textContent).toContain(VERDICTS.waiting.headline);
		expect(region.textContent).toContain(VERDICTS.waiting.detail);
	});

	it('announces the words on screen rather than a hidden second copy that can go stale', () => {
		renderVerdict('waiting');

		// The alternative design — a visually hidden live region updated only on
		// state change — announces less often, and pays for it by holding a
		// duration that stopped being true. Design §6: the panel never lies about
		// what it knows.
		//
		// Counted over the document's text rather than by element, because the
		// likeliest hidden copy is one node holding both lines as a single
		// sentence, and no element-level query matches either line inside it.
		expect(occurrences(VERDICTS.waiting.headline)).toBe(1);
		expect(occurrences(VERDICTS.waiting.detail)).toBe(1);
	});

	it('keeps one status node across a state change, because a live region that is replaced may never announce', async () => {
		const { rerender } = render(TaskVerdictLine, { props: { verdict: VERDICTS.running } });
		const region = screen.getByRole('status');

		await rerender({ verdict: VERDICTS.failed });

		// The same element, mutated. Wrapping the block in a `{#key}` would read
		// identically on screen and silently stop announcing anything: a live
		// region has to be in the document *before* its content changes.
		expect(screen.getByRole('status')).toBe(region);
		expect(region.textContent).toContain(VERDICTS.failed.headline);
		expect(region.textContent).toContain(VERDICTS.failed.detail);
	});
});
