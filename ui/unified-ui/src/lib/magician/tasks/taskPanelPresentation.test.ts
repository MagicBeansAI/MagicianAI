import { readFileSync } from 'node:fs';
import { resolve as resolvePath } from 'node:path';

import { describe, expect, it } from 'vitest';

/**
 * The task panel's presentation, asserted against its stylesheets.
 *
 * **Why the source and not a render.** Every claim in this file is about
 * computed style — a type ratio, a shared left edge, a contrast ratio in a
 * theme nobody has loaded — and jsdom computes no layout and applies no
 * component CSS. `TaskVerdictLine.component.test.ts` says as much where it
 * gives up on the detail line's reserved height. So the choice is between
 * asserting these in the one place they exist, which is the text of the
 * stylesheets, and not asserting them at all. The panel already reads its own
 * source once, for the same reason: a claim no render can observe.
 *
 * What that buys, concretely: a later edit that flattens the type scale, moves
 * one marker off the shared column, drops a hover state, or points a control at
 * a colour that fails in one of the twenty-three themes in `app.css` fails
 * here rather than in a screenshot nobody takes.
 *
 * What it does not buy: this cannot see whether the result reads as a story.
 * That is still a visual pass.
 */

const ROOT = 'src/lib/magician/tasks';
const APP_CSS = 'src/app.css';

const FILES = {
	verdict: `${ROOT}/TaskVerdictLine.svelte`,
	act: `${ROOT}/TaskActSection.svelte`,
	panel: `${ROOT}/UnifiedTaskPanel.svelte`,
	drawer: `${ROOT}/TaskPanelDrawer.svelte`,
	/**
	 * **The house control, which the panel now renders rather than imitates.**
	 *
	 * It is read here because the panel's controls *are* this file: nine of them
	 * were hand-written CSS approximating this component variant by variant, and
	 * measuring what is on screen now means measuring what this declares. The four
	 * files above are the panel's; this one is not, and nothing below may assert
	 * anything about it that is not already true of every other consumer.
	 */
	button: 'src/lib/magician/components/native/Button.svelte'
} as const;

/** The four files the panel owns. The `button` entry above is a dependency. */
const PANEL_FILES = ['verdict', 'act', 'panel', 'drawer'] as const;

type FileKey = keyof typeof FILES;

/** From the project root rather than `import.meta.url`, which vite rewrites. */
const read = (relative: string): string =>
	readFileSync(resolvePath(process.cwd(), relative), 'utf8');

const SOURCE: Record<FileKey, string> = {
	verdict: read(FILES.verdict),
	act: read(FILES.act),
	panel: read(FILES.panel),
	drawer: read(FILES.drawer),
	button: read(FILES.button)
};

// ── reading CSS ──────────────────────────────────────────────────────────────

/** Comments carry example values and prose; neither is a declaration. */
const uncommented = (css: string): string => css.replace(/\/\*[\s\S]*?\*\//g, '');

/** A Svelte component's one `<style>` block, with its comments taken out. */
function stylesheet(key: FileKey): string {
	const block = SOURCE[key].match(/<style>([\s\S]*)<\/style>/);
	if (!block) throw new Error(`${FILES[key]} has no <style> block`);
	return uncommented(block[1]);
}

interface Rule {
	selectors: string[];
	declarations: Map<string, string>;
}

function parseRules(css: string): Rule[] {
	return [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((match) => ({
		// Anything before the last statement terminator is preamble, never part
		// of the selector list — `app.css` opens with `@tailwind` statements.
		selectors: match[1]
			.split(';')
			.slice(-1)[0]
			.split(',')
			.map((one) => one.trim()),
		declarations: declarationsOf(match[2])
	}));
}

function declarationsOf(body: string): Map<string, string> {
	const out = new Map<string, string>();
	// Split on top-level semicolons only: `background: color-mix(a, b)` has none
	// inside it today, but a nested data-URI or a `;` in a string would.
	for (const chunk of body.split(';')) {
		const at = chunk.indexOf(':');
		if (at === -1) continue;
		out.set(chunk.slice(0, at).trim(), chunk.slice(at + 1).trim());
	}
	return out;
}

/**
 * Every declaration that reaches an element matching exactly this selector,
 * in source order, later winning — which is how the cascade resolves a set of
 * rules that all have the same specificity.
 */
function styleOf(key: FileKey, selector: string): Map<string, string> {
	const merged = new Map<string, string>();
	let seen = false;
	for (const rule of parseRules(stylesheet(key))) {
		if (!rule.selectors.includes(selector)) continue;
		seen = true;
		for (const [prop, value] of rule.declarations) merged.set(prop, value);
	}
	// A typo'd selector would otherwise return an empty map and every assertion
	// against it would read as "the property is absent" rather than "the rule is".
	if (!seen) throw new Error(`${FILES[key]} has no rule for ${selector}`);
	return merged;
}

function requireDeclaration(key: FileKey, selector: string, prop: string): string {
	const value = styleOf(key, selector).get(prop);
	if (value === undefined) throw new Error(`${FILES[key]} ${selector} declares no ${prop}`);
	return value;
}

/**
 * Is this rule a box already as wide as its container's content — `flex: 0 0 100%`
 * or `flex-basis: 100%`? Those are the boxes for which a horizontal margin is an
 * overflow rather than an indent.
 */
const isFullBasis = (declarations: Map<string, string>): boolean => {
	if (declarations.get('flex-basis') === '100%') return true;
	const shorthand = declarations.get('flex');
	// The basis is the shorthand's third value, but matching the token anywhere is
	// enough here and stays right if a rule writes `flex: 1 1 100%`.
	return shorthand !== undefined && /(^|\s)100%(\s|$)/.test(shorthand);
};

const rem = (value: string): number => {
	const match = value.trim().match(/^([\d.]+)rem$/);
	if (!match) throw new Error(`not a rem length: ${value}`);
	return Number(match[1]);
};

// ── A1.0 the stylesheets this file can read at all ───────────────────────────

/**
 * **A comment that swallows a rule, which is the one defect that makes every other
 * case in this file silently weaker.**
 *
 * `uncommented` above strips comments before anything else reads the CSS. So a
 * dangling comment opener does not produce a parse error here: it produces a
 * stylesheet that is *missing rules*, and every assertion about those rules
 * disappears with them. `styleOf` throws only when a selector is named and absent,
 * and nothing names most rules.
 *
 * It happened. Retiring the `Details` control deleted the
 * `.act__body :global(.act__details)` rule and the tail of its comment but left the
 * opening `/*`, so the comment ran on to the next one and took `.act__provenance`
 * with it — L3 lost its size step and its top margin, in the browser, for a commit.
 * Nothing failed, here or anywhere: the rule was not gone, it was invisible.
 *
 * The discriminator is a brace block containing a declaration. Prose in these files
 * quotes `{#if …}` and `{@const …}` freely, and neither has a `:` and a `;` inside
 * the braces.
 */
describe('the panel’s stylesheets say what they appear to say', () => {
	it('closes every comment, so no rule is hidden from every case below', () => {
		const swallowed: string[] = [];

		for (const key of PANEL_FILES) {
			const block = SOURCE[key].match(/<style>([\s\S]*)<\/style>/);
			if (!block) throw new Error(`${FILES[key]} has no <style> block`);
			const css = block[1];

			for (let at = css.indexOf('/*'); at !== -1; at = css.indexOf('/*', at + 2)) {
				const end = css.indexOf('*/', at + 2);
				if (end === -1) {
					swallowed.push(`${key}: comment opened and never closed`);
					break;
				}
				const body = css.slice(at + 2, end);
				if (/\{[^}]*:[^}]*;[^}]*\}/.test(body)) {
					swallowed.push(`${key}: a comment contains a rule — ${body.trim().slice(0, 60)}…`);
				}
				at = end;
			}
		}

		expect(swallowed).toEqual([]);
	});

	/**
	 * The rule that was swallowed, named so the specific loss cannot recur quietly
	 * even if the sweep above is ever narrowed. L3 is a step below the act body it
	 * sits in, and it is spaced from it.
	 */
	it('keeps L3 a step below the body it sits in, and spaced from it', () => {
		const provenance = styleOf('act', '.act__provenance');
		expect(rem(requireDeclaration('act', '.act__provenance', 'font-size'))).toBeLessThan(
			rem(requireDeclaration('act', '.act__summary', 'font-size'))
		);
		expect(provenance.get('margin')).toContain('var(--space-sm)');
	});
});

// ── A1.1 the type scale ──────────────────────────────────────────────────────

/**
 * The three nominal levels, top down. Each names the selector that carries the
 * treatment, so the table cannot drift from the stylesheets without this file
 * failing.
 */
const LEVELS = [
	{ level: 'L0 · the verdict', key: 'verdict' as FileKey, selector: '.verdict__headline' },
	{ level: 'L1 · an act', key: 'act' as FileKey, selector: '.act__title' },
	{ level: 'chrome · the drawer heading', key: 'drawer' as FileKey, selector: '.task-panel__title' }
];

/**
 * The smallest ratio that reads as a deliberate step rather than as two things
 * that happen to differ. The three levels shipped at 1.067 between the top two
 * and 1.000 between the top and the drawer heading.
 */
const STEP = 1.125;

describe('the panel ranks its three levels by type', () => {
	it('steps the size down a real interval at each level', () => {
		const sizes = LEVELS.map((row) => rem(requireDeclaration(row.key, row.selector, 'font-size')));

		// Reported as the pairs rather than as a loop of bare booleans, so a
		// failure says which step collapsed and by how much.
		const steps = sizes.slice(0, -1).map((size, index) => ({
			from: LEVELS[index].level,
			to: LEVELS[index + 1].level,
			ratio: Number((size / sizes[index + 1]).toFixed(3))
		}));

		for (const step of steps) expect(step).toMatchObject({ ratio: expect.any(Number) });
		expect(steps.every((step) => step.ratio >= STEP)).toBe(true);
	});

	it('steps the weight down alongside it, so the ranking survives a size the reader has rescaled', () => {
		const weights = LEVELS.map((row) =>
			Number(requireDeclaration(row.key, row.selector, 'font-weight'))
		);

		// Strictly decreasing. All three were 650 — one weight doing no work.
		expect(weights).toEqual([...weights].sort((a, b) => b - a));
		expect(new Set(weights).size).toBe(weights.length);
	});

	/**
	 * **The greyscale claim, made structurally.**
	 *
	 * Two of the seven verdict states are `neutral` and get no wash, and several
	 * themes in `app.css` collapse every wash to one grey, so the wash cannot be
	 * what separates L0 from L1. This is the assertion that says it is not: the
	 * two levels are painted in the *same* colour token, so whatever ranks them
	 * is not colour — and the two tests above are what is left.
	 *
	 * `.act__title` sets no colour of its own; it takes the header's, which is
	 * why the header is what is read here.
	 */
	it('ranks L0 above L1 without using colour to do it', () => {
		expect(requireDeclaration('verdict', '.verdict__headline', 'color')).toBe(
			requireDeclaration('act', '.act__header', 'color')
		);
		expect(styleOf('act', '.act__title').has('color')).toBe(false);
	});

	/**
	 * The panel has two L0s — the verdict, and `Can't load this task` when there
	 * is no verdict to give. They answer the same question in the same place, so
	 * they take the same step; without this the unloadable branch flattens on its
	 * own and nothing notices, because no fixture renders both.
	 */
	it('gives the unloadable panel the same L0 treatment as the verdict', () => {
		for (const prop of ['font-size', 'font-weight', 'font-family']) {
			expect(requireDeclaration('panel', '.panel__load-error', prop)).toBe(
				requireDeclaration('verdict', '.verdict__headline', prop)
			);
		}
	});

	/** The drawer heading steps down *in* the body face, which is a lever of its own. */
	it('takes the drawer heading out of the display face the two levels below it share', () => {
		const display = requireDeclaration('verdict', '.verdict__headline', 'font-family');
		expect(requireDeclaration('act', '.act__title', 'font-family')).toBe(display);
		expect(requireDeclaration('drawer', '.task-panel__title', 'font-family')).not.toBe(display);
	});
});

// ── A1.2 the spine ───────────────────────────────────────────────────────────

/** The `[left, right]` components of a `padding` shorthand, as written. */
function horizontalPadding(shorthand: string): [string, string] {
	const parts = splitTopLevel(shorthand, ' ');
	if (parts.length === 1) return [parts[0], parts[0]];
	if (parts.length === 4) return [parts[3], parts[1]];
	return [parts[1], parts[1]];
}

describe('the verdict and the acts share one left edge', () => {
	/**
	 * The defect this replaces: `.verdict` carried `padding: var(--space-md)` and
	 * `.act__header` carried `padding: var(--space-sm) 0`, both as direct children
	 * of an unpadded `.panel`, so the verdict's state glyph sat a whole
	 * `--space-md` right of every act's `▸` — while a comment in the verdict's own
	 * stylesheet asserted the two shared an edge.
	 *
	 * Both now inset by `--task-panel-bleed` rather than by nothing, which is what
	 * makes their backgrounds full-width bands (see the full-bleed section below).
	 * The claim is unchanged and is still one comparison: whatever the inset is, it
	 * is the *same* inset, read out of one place by both.
	 */
	it('insets both of them by the same amount, read from one place', () => {
		expect(horizontalPadding(requireDeclaration('verdict', '.verdict', 'padding'))).toEqual(
			horizontalPadding(requireDeclaration('act', '.act__header', 'padding'))
		);
		expect(horizontalPadding(requireDeclaration('act', '.act__header', 'padding'))[0]).toBe(
			'var(--task-panel-bleed, 0px)'
		);
	});

	it('gives the disclosure column exactly one home', () => {
		const declarations = PANEL_FILES.filter((key) =>
			uncommented(SOURCE[key]).includes('--task-panel-disclosure:')
		);

		// One file, and the one that owns the element both readers are inside.
		expect(declarations).toEqual(['panel']);
		expect(styleOf('panel', '.panel').get('--task-panel-disclosure')).toBe('0.75rem');
	});

	it('has both markers read that one home, with no fallback restating the number', () => {
		// An exact match rather than a `toContain`: `var(--x, 0.75rem)` is the same
		// number written three times again, with two of the copies hidden in a
		// fallback nobody reads until the declaration moves.
		expect(requireDeclaration('verdict', '.verdict__marker', 'width')).toBe(
			'var(--task-panel-disclosure)'
		);
		expect(requireDeclaration('act', '.act__marker', 'width')).toBe(
			'var(--task-panel-disclosure)'
		);
	});

	it('has the act body hang off the same column rather than off a copy of its width', () => {
		const padding = requireDeclaration('act', '.act__body', 'padding');
		expect(padding).toContain('var(--task-panel-disclosure)');
		expect(padding).not.toMatch(/[\d.]+rem/);
	});

	/** Same column, and the glyph on the same axis within it. */
	it('centres both glyphs in the column', () => {
		expect(requireDeclaration('verdict', '.verdict__marker', 'text-align')).toBe('center');
		expect(requireDeclaration('act', '.act__marker', 'text-align')).toBe('center');
	});
});

// ── A1.3 the timeline row's own hierarchy ────────────────────────────────────

/**
 * **The defect this section replaces, in the owner's words: "there is no
 * typographic hierarchy".**
 *
 * Every element of a timeline row — the kind, the title, the latency, the cost
 * line — shipped at one size and one colour, so a row read as five equal facts
 * and a feed read as a wall. Two of those facts (the token bill and the latency)
 * were reported as *missing* when they had been on screen all along, which is
 * what an undifferentiated wall does to something inside it.
 *
 * These are the ranking claims. What they cannot say is whether the result reads
 * as a story; that is still a visual pass.
 */
describe('a timeline row ranks its facts instead of levelling them', () => {
	/** Every element on the row that is a fact *about* it rather than the row itself. */
	const SUBORDINATE = [
		'.timeline-row__when',
		'.timeline-row__kind',
		'.timeline-row__latency',
		'.timeline-row__meta',
		'.timeline-row__capture'
	];

	it('gives the title the row’s only weight, because it is what a reader scans for', () => {
		expect(Number(requireDeclaration('panel', '.timeline-row__title', 'font-weight'))).toBeGreaterThan(
			400
		);

		// And nothing else on the row competes for it. Named rather than counted, so
		// a failure says which fact started shouting.
		const alsoWeighted = SUBORDINATE.filter((selector) =>
			styleOf('panel', selector).has('font-weight')
		);
		expect(alsoWeighted).toEqual([]);
	});

	it('puts every other fact on the row below the title in size', () => {
		// The title's size is the feed's — it sets none of its own, which the case
		// below pins — so that is what the others are measured against.
		const title = rem(requireDeclaration('panel', '.run-timeline', 'font-size'));
		const bigger = SUBORDINATE.filter(
			(selector) => rem(requireDeclaration('panel', selector, 'font-size')) >= title
		);

		expect(bigger).toEqual([]);
	});

	/**
	 * The title sets no size of its own: it inherits the feed's, which is the act
	 * body's. So the ratio is read off the feed rather than off the row, and the
	 * assertion above needs the same number — hence this, which is what makes
	 * `.timeline-row__title` resolvable at all.
	 */
	it('leaves the title at the feed’s own size rather than restating it', () => {
		expect(styleOf('panel', '.timeline-row__title').has('font-size')).toBe(false);
		expect(rem(requireDeclaration('panel', '.run-timeline', 'font-size'))).toBeGreaterThan(0);
	});

	it('gives the leading time column one home, read by both the cluster and the indent', () => {
		// The same rule `--task-panel-disclosure` follows one level up, and for the
		// same reason: the column's width is declared by the cluster and read again
		// by the indent every sub-line hangs off. Written out twice they agree until
		// the first edit, and the failure — a cost line half a character off the
		// title above it — is the kind nothing points at.
		expect(styleOf('panel', '.run-timeline').get('--timeline-when')).toBe('3.5rem');
		expect(requireDeclaration('panel', '.timeline-row__when', 'width')).toBe(
			'var(--timeline-when)'
		);
		// **Read off `padding-left`, and that is not interchangeable with the
		// `margin-left` this used to ask for.** The claim being made here is that the
		// indent and the column agree on one number; *which property carries the
		// indent* is a different claim, and putting it in this assertion is what let
		// the two drift — the margin spelling shipped, overflowed 85 elements, and
		// this case went on passing because a margin is also an indent. The case
		// below owns the property, so this one owns only the number.
		expect(requireDeclaration('panel', '.timeline-row__meta', 'padding-left')).toContain(
			'var(--timeline-when)'
		);
		// And no copy of the number anywhere else in the stylesheet.
		expect(stylesheet('panel').match(/3\.5rem/g)).toHaveLength(1);
	});

	/**
	 * **The overflow the indent above caused, pinned as the rule that prevents it
	 * rather than as the declaration that fixed one instance of it.**
	 *
	 * These sub-lines carry `flex-basis: 100%`, which already sizes the box to the
	 * container's whole content width. A left *margin* adds to that total, so the
	 * box ends up wider than its container by exactly the indent — measured in a
	 * real browser, 85 elements past the panel's right edge, the worst by 60px.
	 * Padding under `border-box` indents the content inside the same 100%.
	 *
	 * The two spellings read identically in a stylesheet and differ by the width of
	 * the overflow, and jsdom computes no layout, so this file is the only place the
	 * difference can be seen at all.
	 *
	 * **Written as a sweep, because the selector-shaped version of this test was the
	 * bug.** One declaration was copied across four sub-lines; the fix was applied
	 * to those four, and two more instances of the identical mistake were left
	 * live — `.run-step__who`, a margin, and `.output-preview`, padding on a
	 * full-basis box with no `border-box` to put it inside. Both were found by this
	 * sweep and neither by anything else.
	 */
	it('indents a full-basis line inside its own box, never past its container', () => {
		const offenders: string[] = [];

		for (const key of PANEL_FILES) {
			for (const rule of parseRules(stylesheet(key))) {
				if (!isFullBasis(rule.declarations)) continue;
				const where = `${key} ${rule.selectors.join(', ')}`;

				// Horizontal margins only. `margin-top` on a full-basis line is the
				// ordinary way to space it from the line above and costs no width.
				for (const property of ['margin', 'margin-left', 'margin-right']) {
					if (rule.declarations.has(property)) offenders.push(`${where} › ${property}`);
				}

				// Padding is only inside the 100% if the box is measured that way.
				const pads = ['padding', 'padding-left', 'padding-right'].some((property) =>
					rule.declarations.has(property)
				);
				if (pads && rule.declarations.get('box-sizing') !== 'border-box') {
					offenders.push(`${where} › padding with no box-sizing: border-box`);
				}
			}
		}

		expect(offenders).toEqual([]);
	});

	/**
	 * **The time cluster is one column of two rows, and the width halved with it.**
	 *
	 * Side by side, this column plus the marker, the kind and the latency spent
	 * roughly half of a ~508px content column on facts *about* the row, leaving about
	 * thirty characters of title before it wrapped — on the one element a reader
	 * scans a feed for. Stacking costs one line of height on a row that is already
	 * taller than one line and hands the width back.
	 *
	 * The right edge is declared **once, on the container**, so the two halves share
	 * it by construction: a reader running down the offsets to find where the run
	 * slowed needs that edge, and it cannot drift if only one element sets it.
	 */
	it('stacks the clock over the offset and shares one right edge between them', () => {
		const when = styleOf('panel', '.timeline-row__when');
		expect(when.get('flex-direction')).toBe('column');
		expect(when.get('text-align')).toBe('right');

		// Neither half restates the alignment.
		for (const half of ['.timeline-row__clock', '.timeline-row__offset']) {
			expect(styleOf('panel', half).has('text-align')).toBe(false);
		}

		// Both values are still rendered, and still tabular — stacking was about
		// width, not about dropping one of them.
		expect(when.get('font-variant-numeric')).toBe('tabular-nums');
		expect(SOURCE.panel).toContain('class="timeline-row__clock"');
		expect(SOURCE.panel).toContain('class="timeline-row__offset"');
	});

	/**
	 * Latency stays at the **trailing** edge. It answers a different question from
	 * the leading column — how long this took, rather than when it happened — and
	 * folding it into the stack would put three numbers in one gutter.
	 */
	it('leaves latency at the trailing edge rather than folding it into the stack', () => {
		const latency = styleOf('panel', '.timeline-row__latency');
		expect(latency.get('text-align')).toBe('right');
		expect(latency.get('flex')).toBe('none');
		// Not inside the `when` cluster: it is a sibling item of the row.
		expect(SOURCE.panel).not.toMatch(/timeline-row__when[\s\S]{0,400}timeline-row__latency/);
	});

	/**
	 * Isolation is content-driven in **both** directions: a row several lines tall
	 * needs a bound, and a feed of two hundred bounded rows is design §1's card
	 * grid rebuilt one level down. So the border belongs to the attribute selector
	 * and the bare row must carry none.
	 */
	it('bounds only the rows that carry a body or a stdout block', () => {
		const isolated = styleOf('panel', ".timeline-row[data-timeline-isolated='true']");
		expect(isolated.get('border-top')).toContain('var(--border-soft)');
		expect(isolated.get('border-bottom')).toContain('var(--border-soft)');

		const bare = styleOf('panel', '.timeline-row');
		for (const prop of ['border', 'border-top', 'border-bottom', 'background']) {
			expect(bare.has(prop)).toBe(false);
		}
		// No inset either: a box that indented its own content would take this row's
		// marker off the column every other row shares.
		expect(isolated.get('padding')).toBe('var(--space-xs) 0');
	});

	/**
	 * **Both scrollbars the owner saw, and the reason there were two.**
	 *
	 * A box with one overflow axis `visible` and the other not resolves the visible
	 * one to `auto` — so `overflow-y: auto` alone put a horizontal scrollbar on the
	 * feed, on an axis where every child is built to wrap. The `<pre>` had one of
	 * its own on top of that. Both axes are now stated on both boxes.
	 */
	it('never scrolls the feed or a stdout block sideways', () => {
		for (const selector of ['.run-timeline', '.timeline-row__detail']) {
			expect(requireDeclaration('panel', selector, 'overflow-x')).toBe('hidden');
			expect(requireDeclaration('panel', selector, 'overflow-y')).toBe('auto');
		}

		// Wrapping is what makes `hidden` safe rather than lossy: the newlines that
		// make a log readable are kept, a long line folds, and a single unbroken
		// 400-character token folds too.
		const detail = styleOf('panel', '.timeline-row__detail');
		expect(detail.get('white-space')).toBe('pre-wrap');
		expect(detail.get('overflow-wrap')).toBe('anywhere');
		expect(detail.get('font-family')).toBe('var(--font-mono)');
	});
});

// ── A1.33 a highlighted section runs edge to edge ─────────────────────────────

/**
 * **The defect, in the owner's words: "each section has padding and background
 * colors just coloring the padded box, not entire row of section."**
 *
 * `.task-panel__body` carried `padding: var(--space-md)` and every section lived
 * inside it, so an act's hover wash and the verdict's tone band both stopped a
 * whole `--space-md` short of the drawer on each side — a highlight floating in a
 * gutter instead of a band across the panel.
 *
 * The fix inverts it: the scroll container insets **nothing** horizontally, and
 * each section pads its own content by `--task-panel-bleed`. A section's background
 * then starts at the drawer's left edge and ends at its right, while the text
 * inside it lines up with every other section's.
 *
 * **Negative margins were the alternative and this is why they were not used:**
 * they need two numbers that must agree — the container's padding and each
 * section's pull-back — and when they drift the result is a section a few pixels
 * wider than the panel, which reads as a rendering bug rather than a mistake. There
 * is one number here and nothing to keep in step, which is also what makes it
 * survive the drawer being resized.
 *
 * What these cases cannot say: whether the bands look right. jsdom computes no
 * layout, so "runs edge to edge" is asserted as the *structure that produces it*.
 */
describe('a highlighted section spans the panel rather than its padded box', () => {
	it('takes the horizontal inset off the scroll container entirely', () => {
		// The load-bearing half. With any horizontal padding here, no amount of
		// per-section padding can reach the drawer's edge.
		expect(horizontalPadding(requireDeclaration('drawer', '.task-panel__body', 'padding'))).toEqual([
			'0',
			'0'
		]);
	});

	it('gives the inset exactly one home, on the box that used to carry it', () => {
		const declarations = PANEL_FILES.filter((key) =>
			uncommented(SOURCE[key]).includes('--task-panel-bleed:')
		);

		expect(declarations).toEqual(['drawer']);
		expect(styleOf('drawer', '.task-panel__body').get('--task-panel-bleed')).toBe(
			'var(--space-md)'
		);
	});

	/**
	 * **Every reader spells the fallback `0px`, and that is a meaning rather than a
	 * hidden copy of the value.** A panel rendered outside a drawer — which the
	 * component harness does — is inset by nothing, so its sections pad by nothing.
	 * A reader that wrote a length there would be a second declaration of the inset,
	 * invisible until the real one moved: the failure `--task-panel-disclosure`
	 * documents, which is why that one takes no fallback at all.
	 */
	it('has every reader spell the same fallback, and none of them restate a length', () => {
		const readers = PANEL_FILES.flatMap((key) =>
			[...uncommented(SOURCE[key]).matchAll(/var\(--task-panel-bleed([^)]*)\)/g)].map(
				(match) => `${key}: var(--task-panel-bleed${match[1]})`
			)
		);

		// Something reads it in all four files, and every one of them agrees.
		expect(readers.length).toBeGreaterThanOrEqual(6);
		expect([...new Set(readers.map((one) => one.split(': ')[1]))]).toEqual([
			'var(--task-panel-bleed, 0px)'
		]);
	});

	/**
	 * The two elements in the panel that paint a background across a whole section.
	 * Both must take their horizontal inset from the shared variable — a padding of
	 * `0` would put the text on the drawer's edge, and a length would put the band
	 * back inside a gutter.
	 */
	it('pads the two washed sections by the shared inset, so their washes are bands', () => {
		const washed: [FileKey, string][] = [
			['verdict', '.verdict'],
			['act', '.act__header']
		];

		for (const [key, selector] of washed) {
			const style = styleOf(key, selector);
			// It really does paint a background — otherwise this asserts nothing.
			expect(style.has('background')).toBe(true);
			expect(horizontalPadding(style.get('padding') as string)).toEqual([
				'var(--task-panel-bleed, 0px)',
				'var(--task-panel-bleed, 0px)'
			]);
		}
	});

	/**
	 * A rounded band with square edges outside it reads as a card that failed to
	 * load. The radius was right while the block was inset and had corners; it is
	 * wrong now that it reaches the drawer's edges.
	 */
	it('drops the radius from the verdict band, which now has no corners to round', () => {
		expect(styleOf('verdict', '.verdict').has('border-radius')).toBe(false);
	});

	/**
	 * The act body's content edge has to keep matching its header's, so its
	 * `padding-left` is the **sum** of the inset and the disclosure column — written
	 * as those two things rather than as a number that happens to equal them, which
	 * is the rule both custom properties in this feature follow.
	 */
	it('hangs the act body off the inset and the column together', () => {
		const padding = requireDeclaration('act', '.act__body', 'padding');
		expect(padding).toContain('var(--task-panel-bleed, 0px)');
		expect(padding).toContain('var(--task-panel-disclosure)');
		// No length anywhere in it: every part is one of the shared values.
		expect(padding).not.toMatch(/[\d.]+rem/);
	});

	/**
	 * **`act--open` finally has a rule.** The class was on the markup from the first
	 * commit with nothing selecting it, so which act was open could only be read
	 * from the body under it — a reader scrolled into a long Run act had nothing at
	 * the top of the column telling them which section they were inside.
	 *
	 * The *same* wash as the hover rather than a second colour, which is what keeps
	 * this from being a new visual language, and it is why no new contrast pairing is
	 * needed: `--bg-soft` under an act's text is already one of the two surfaces the
	 * sweep measures.
	 */
	it('marks the open act with the wash the hover already uses, on the header only', () => {
		const rules = parseRules(stylesheet('act')).filter((rule) =>
			rule.selectors.includes('.act--open > .act__header')
		);

		expect(rules).toHaveLength(1);
		expect(rules[0].declarations.get('background')).toBe('var(--bg-soft)');
		// The same rule as the hover, not a copy of it — one selector list, so the two
		// states cannot drift apart.
		expect(rules[0].selectors).toContain('.act__header:hover');
		// And nothing washes the body, which carries `--bg-soft` blocks of its own.
		expect(styleOf('act', '.act__body').has('background')).toBe(false);
	});
});

// ── A1.35 the flex floor, everywhere ─────────────────────────────────────────

/**
 * **The defect this section replaces, and why it is a section rather than a
 * case.**
 *
 * `.act__provenance dd` declared `overflow-wrap: anywhere` from its first commit
 * and never wrapped anything. The reason is not in that declaration: the `dd` is
 * a flex item, and a flex item's default `min-width: auto` is a *content-based
 * floor*. An unbroken 40-character execution id therefore **sizes** the cell,
 * the cell is wider than the row, the row is wider than the drawer, and the wrap
 * rule is never reached — every declaration involved individually correct.
 *
 * `.timeline-row__detail` one file over already carried `min-width: 0`. So the
 * rule was known in one place and missed in another, which is the actual defect:
 * a test for the one `dd` would let the next instance through, and there were
 * four more (`.timeline-row__offset`, `.task-panel`, `.output-file__actions`,
 * `.output-file__meta`).
 *
 * So the guard is two claims, not one:
 *
 * 1. **The table below names every flex container in these four files**, checked
 *    against the stylesheets. A new one has to be classified here rather than
 *    quietly joining the set.
 * 2. **Every item of a row-direction container that clips or wraps its content
 *    declares the floor override.** Those two declarations are the ones a floor
 *    silently disables, so they are the ones that identify an item holding text
 *    it does not control.
 *
 * `column` containers are listed and exempt, and that is a fact about the spec
 * rather than a relaxation: the automatic minimum size applies to the *main*
 * axis, so a column container's floor is `min-height: auto` and its items cannot
 * blow out horizontally by this mechanism.
 */
interface FlexBox {
	key: FileKey;
	selector: string;
	direction: 'row' | 'column';
	/** The item selectors, as the markup nests them. `[]` for a leaf control. */
	items: string[];
	/** Why a leaf has no items worth listing. */
	leaf?: string;
}

const FLEX: FlexBox[] = [
	// The verdict block.
	{
		key: 'verdict',
		selector: '.verdict',
		direction: 'row',
		items: ['.verdict__marker', '.verdict__lines']
	},

	// The act column.
	{
		key: 'act',
		selector: '.act__header',
		direction: 'row',
		items: ['.act__marker', '.act__title', '.act__summary']
	},
	// `.act__provenance-row` was here, and is now in `GRID` below: as a flex row it
	// rendered a 100-character path at 0px wide and 110 lines tall. It did not stop
	// being a container that has to be classified, so it moved rather than left.

	// The panel's own column.
	{ key: 'panel', selector: '.panel', direction: 'column', items: [] },
	{ key: 'panel', selector: '.ask', direction: 'column', items: [] },
	{
		key: 'panel',
		selector: '.ask__actions',
		direction: 'row',
		items: [],
		leaf: 'one submit, whose label is one of five strings this component owns'
	},
	{
		key: 'panel',
		selector: '.output-scope-heading',
		direction: 'row',
		items: ['.output-scope-heading > div', '.output-scope-heading__badge']
	},
	{ key: 'panel', selector: '.output-scope-heading > div', direction: 'column', items: [] },
	{ key: 'panel', selector: '.output-group-heading', direction: 'column', items: [] },
	{ key: 'panel', selector: '.output-files', direction: 'column', items: [] },
	{
		key: 'panel',
		selector: '.output-intermediates__summary',
		direction: 'row',
		items: ['.output-intermediates__title-group', '.output-intermediates__hint']
	},
	{
		key: 'panel',
		selector: '.output-intermediates__title-group',
		direction: 'row',
		items: ['.output-intermediates__title', '.output-intermediates__count']
	},
	{
		key: 'panel',
		selector: '.output-file',
		direction: 'row',
		items: [
			'.output-file__thumb',
			'.output-file__name',
			'.output-file__meta',
			'.output-file__actions',
			'.output-preview'
		]
	},
	{
		key: 'panel',
		selector: '.output-file__thumb',
		direction: 'row',
		items: [],
		leaf: 'one image at a fixed 2.5rem'
	},
	{
		key: 'panel',
		selector: '.output-file__actions',
		direction: 'row',
		items: [],
		leaf: 'up to five controls, every label a literal in this component'
	},
	/**
	 * The run picker: a label, the `Select`, and the total. It **wraps**, which is
	 * the only reason the floor matters here — a long option in a narrow drawer has
	 * to reflow rather than push the count off the right edge, and the wrap rule
	 * could never fire while the control floored at its widest option's intrinsic
	 * width. The two words beside it are literals this component owns and neither
	 * clips nor wraps, so neither needs the override.
	 */
	{
		key: 'panel',
		selector: '.run-picker',
		direction: 'row',
		items: ['.run-picker__label', '.run-picker__control', '.run-picker__count']
	},
	{
		key: 'panel',
		selector: '.run-step',
		direction: 'row',
		items: [
			'.run-step__marker',
			'.run-step__label',
			'.run-step__retries',
			'.run-step__duration',
			'.run-step__who'
		]
	},
	{
		key: 'panel',
		selector: '.run-timeline__header',
		direction: 'row',
		items: [],
		leaf: 'run activity title/window note and view mode controls'
	},
	{
		key: 'panel',
		selector: '.run-timeline__modes',
		direction: 'row',
		items: [],
		leaf: 'grouped and chronological view mode controls'
	},
	{
		key: 'panel',
		selector: '.timeline-delegation__summary',
		direction: 'row',
		items: ['.timeline-delegation__agent', '.timeline-delegation__meta']
	},
	{
		key: 'panel',
		selector: '.timeline-delegation__footer',
		direction: 'row',
		items: ['.timeline-delegation__footer-icon', '.timeline-delegation__footer-text']
	},
	{
		key: 'panel',
		selector: '.timeline-row',
		direction: 'row',
		items: [
			'.timeline-row__when',
			'.timeline-row__marker',
			'.timeline-row__kind',
			'.timeline-row__title',
			'.timeline-row__latency',
			'.timeline-row__body',
			'.timeline-row__meta',
			// The stdout block is inside this figure now, not a flex item of the row.
			'.timeline-row__code',
			'.timeline-row__capture'
		]
	},
	// The time cluster is a column — the clock over the offset — so its two halves are
	// exempt for the reason every column's items are.
	{ key: 'panel', selector: '.timeline-row__when', direction: 'column', items: [] },
	{
		key: 'panel',
		selector: '.timeline-row__meta',
		direction: 'row',
		items: ['.timeline-row__id', '.timeline-row__figures']
	},
	{
		key: 'panel',
		selector: '.timeline-row__code-actions',
		direction: 'row',
		items: [],
		leaf: 'one Copy control'
	},

	// The drawer shell.
	{
		key: 'drawer',
		selector: '.task-panel-backdrop',
		direction: 'row',
		items: ['.task-panel']
	},
	{ key: 'drawer', selector: '.task-panel', direction: 'column', items: [] },
	// The header is four stacked rows, so it is a column and its rows are exempt for
	// the reason every column is. The rows that are themselves rows are below it.
	{ key: 'drawer', selector: '.task-panel__header', direction: 'column', items: [] },
	{
		key: 'drawer',
		selector: '.task-panel__actions',
		direction: 'row',
		items: ['.task-panel__thread']
	},
	/**
	 * The thread mover's own row — the label and the select group beside it.
	 *
	 * **It landed in `101bb5fc4` without a row here**, and this table's whole point
	 * is that a new flex container cannot quietly join the set: the case above
	 * failed on `drawer .task-panel__move` until it was classified. The label
	 * declares `flex: none` and `white-space: nowrap`, which is a deliberate refusal
	 * to shrink rather than something the floor could rescue; the group beside it is
	 * the item that grows, and it is classified in its own right below.
	 */
	{
		key: 'drawer',
		selector: '.task-panel__move',
		direction: 'row',
		items: ['.task-panel__move-label', '.task-panel__thread']
	},
	{
		key: 'drawer',
		selector: '.task-panel__thread',
		direction: 'row',
		items: [],
		leaf: 'a Select and, once the pick differs, a Move button'
	},
	{
		key: 'drawer',
		selector: '.task-panel__chips',
		direction: 'row',
		items: [],
		leaf: 'Badge components, which cap their own width'
	},
	{
		key: 'drawer',
		selector: '.task-panel__id',
		direction: 'row',
		// The elided id and the copied tick. The tick has no rule of its own, so it
		// cannot be listed — `styleOf` throws on a selector with no rule, which is
		// the property that keeps this table honest.
		items: ['.task-panel__id-value']
	},
	{
		key: 'drawer',
		selector: '.task-panel__header :global(.task-panel__action)',
		direction: 'row',
		items: [],
		leaf: "a surface's own label, slotted; the shell can style it but not size it"
	},
	{ key: 'drawer', selector: '.task-panel__loading', direction: 'column', items: [] }
];

/**
 * **The grid containers, which exist because one of the flex rows above could not
 * be one.**
 *
 * `.act__provenance-row` was a flex row and it collapsed catastrophically: measured
 * in a real browser, a 100-character output path rendered at **0px wide and 110
 * lines tall** — one character per line — while a 36-character execution id beside
 * it was fine. `min-width: 0` on the item is what did it: with
 * `overflow-wrap: anywhere` the content-based floor is one character, and declaring
 * `0` removed even that, so the cell had no width to keep.
 *
 * A grid track floors itself. `minmax(0, 1fr)` is a floor on the *track* rather
 * than a declaration each item has to remember, which is why the fix was a change
 * of layout mode rather than another item-level override.
 *
 * So the enumeration below covers both modes: a container that changes from flex to
 * grid moves between these two tables and a new one in either mode has to be
 * classified. A container that merely vanished from `FLEX` would have taken its
 * classification with it.
 */
interface GridBox {
	key: FileKey;
	selector: string;
	/** The track list, asserted so a value column cannot lose its floor. */
	columns: string;
	items: string[];
}

const GRID: GridBox[] = [
	{
		key: 'act',
		selector: '.act__provenance-row',
		columns: '7rem minmax(0, 1fr)',
		items: ['.act__provenance dt', '.act__provenance dd']
	},
	// The responsibility block's rows are a second label/value pair in the same
	// drawer, and its value column holds an agent id — the same shape of long
	// unbroken token that collapsed the provenance row. Grid from the start rather
	// than flex-then-fixed, which is why it joins this table and not `FLEX`.
	{
		key: 'panel',
		selector: '.run-responsibility__row',
		columns: '7rem minmax(0, 1fr)',
		items: ['.run-responsibility dt', '.run-responsibility dd']
	}
];

/**
 * Does this item clip or wrap **text**, and therefore depend on the floor being
 * lifted?
 *
 * `overflow-wrap` on its own is enough — it exists only to break text. `overflow`
 * needs a second signal, because a box can clip for reasons a floor does not
 * touch: `.output-file__thumb` clips a fixed-size image to a border radius, which
 * no content-based minimum can defeat. `text-overflow` or `white-space` beside it
 * is what says the thing being clipped is a line of text.
 */
function clipsText(style: Map<string, string>): boolean {
	if (style.has('overflow-wrap')) return true;
	const clips = ['overflow', 'overflow-x', 'overflow-y'].some((prop) => style.has(prop));
	return clips && (style.has('text-overflow') || style.has('white-space'));
}

describe('every flex item that has to yield to its container says so', () => {
	/** Every container in these four files whose `display` is one of `modes`. */
	const containers = (modes: string[]): Set<string> => {
		const found = new Set<string>();
		for (const key of PANEL_FILES) {
			for (const rule of parseRules(stylesheet(key))) {
				const display = rule.declarations.get('display');
				if (display === undefined || !modes.includes(display)) continue;
				// A `:global(…)` wrapper is scoping, not part of the selector's shape.
				for (const one of rule.selectors) found.add(`${key} ${one}`);
			}
		}
		return found;
	};

	/** Reported as the two differences rather than as a count, so a failure says
	    which container appeared and which one went away. */
	const expectSame = (found: Set<string>, declared: Set<string>): void => {
		expect([...found].filter((one) => !declared.has(one)).sort()).toEqual([]);
		expect([...declared].filter((one) => !found.has(one)).sort()).toEqual([]);
	};

	it('names every flex container in these four files, so a new one has to be classified', () => {
		expectSame(
			containers(['flex', 'inline-flex']),
			new Set(FLEX.map((box) => `${box.key} ${box.selector}`))
		);
	});

	/**
	 * The same claim for grid, and the reason it is a second case rather than a
	 * second mode in the one above: a container that *changes* layout mode has to
	 * fail one of these two, and a single combined set would let it move from flex
	 * to grid with its classification — including the wrong item-level floor —
	 * carried along unread. That is precisely what `.act__provenance-row` did.
	 */
	it('names every grid container too, so changing layout mode has to be classified', () => {
		expectSame(
			containers(['grid', 'inline-grid']),
			new Set(GRID.map((box) => `${box.key} ${box.selector}`))
		);
	});

	/**
	 * A grid row's value column carries the floor on the **track**, which is what
	 * makes it the fix rather than a fourth place to remember `min-width: 0`.
	 * `minmax(0, …)` is the whole of it: a bare `1fr` track has an `auto` minimum
	 * and floors at its content just as the flex item did.
	 */
	it('floors every grid value track, rather than each item in it', () => {
		for (const box of GRID) {
			const columns = requireDeclaration(box.key, box.selector, 'grid-template-columns');
			expect(columns, box.selector).toBe(box.columns);
			expect(columns, box.selector).toContain('minmax(0,');
		}
	});

	it('gives every row-direction item that clips or wraps its content a `min-width: 0`', () => {
		const missing: string[] = [];

		for (const box of FLEX) {
			if (box.direction !== 'row') continue;
			for (const item of box.items) {
				// `styleOf` throws on a selector with no rule, which keeps the table
				// from naming an item that no longer exists.
				const style = styleOf(box.key, item);
				if (!clipsText(style)) continue;
				// `flex: none` is a deliberate refusal to shrink, and the floor override
				// is inert on one — such an item must not clip or wrap in the first
				// place, which is why none is exempted here.
				if (style.get('min-width') !== '0') {
					missing.push(`${box.selector} › ${item}`);
				}
			}
		}

		expect(missing).toEqual([]);
	});

	/**
	 * The one that was reported, pinned by name as well as by the sweep above —
	 * the sweep would pass again if the wrap rule were removed instead of the
	 * floor being overridden, and removing it is not the fix.
	 */
	it('lets a long identifier in the provenance list wrap rather than widen the drawer', () => {
		const value = styleOf('act', '.act__provenance dd');
		expect(value.get('overflow-wrap')).toBe('anywhere');
		expect(value.get('min-width')).toBe('0');
	});

	/**
	 * The instance with the widest blast radius: the dialog itself. It sets
	 * `width: min(560px, 100%)`, which a content-based floor turns into a
	 * suggestion — and the scrim justifies to `flex-end`, so an over-wide dialog
	 * pushes its own left edge off screen rather than merely overflowing.
	 */
	it('keeps the drawer at its declared width whatever is inside it', () => {
		const dialog = styleOf('drawer', '.task-panel');
		expect(dialog.get('width')).toBe('min(560px, 100%)');
		expect(dialog.get('min-width')).toBe('0');
	});
});

// ── A1.37 the header's four rows ──────────────────────────────────────────────

/**
 * **What jsdom cannot see here, stated up front.** Line clamping, the condense,
 * and whether the four rows read as a hierarchy are all computed layout, and this
 * file computes none. What is assertable is that each row declares the clamp it is
 * meant to have, that the two rows are ranked by type, and that the transition is
 * guarded for a reader who asked for less motion. Whether it *looks* right is the
 * visual pass.
 */
describe('the drawer header stacks four rows and gives up the right one first', () => {
	it('is a column, because only the first of the four rows is right-aligned', () => {
		const header = styleOf('drawer', '.task-panel__header');
		expect(header.get('display')).toBe('flex');
		expect(header.get('flex-direction')).toBe('column');
		// And it never gives up height to the body, so the condense is the only thing
		// that changes its size.
		expect(header.get('flex')).toBe('none');
		expect(requireDeclaration('drawer', '.task-panel__actions', 'justify-content')).toBe(
			'flex-end'
		);
	});

	/**
	 * Two lines for the title, five for the description — the owner's numbers. Both
	 * need the prefixed *and* unprefixed property: `line-clamp` is the standard name
	 * and `-webkit-line-clamp` is what is implemented, and a rule with only one of
	 * them clamps in some browsers and not others.
	 */
	it('clamps each row to its own number of lines, in both spellings', () => {
		const rows: [string, string][] = [
			['.task-panel__title', '2'],
			['.task-panel__description', '5']
		];

		for (const [selector, lines] of rows) {
			const style = styleOf('drawer', selector);
			expect(style.get('-webkit-line-clamp')).toBe(lines);
			expect(style.get('line-clamp')).toBe(lines);
			// The clamp does nothing without these three beside it.
			expect(style.get('display')).toBe('-webkit-box');
			expect(style.get('-webkit-box-orient')).toBe('vertical');
			expect(style.get('overflow')).toBe('hidden');
		}
	});

	/**
	 * The description sits below the title, which is itself below the verdict — so
	 * the header's own two prose rows have to be ranked against each other as well
	 * as against the column below them.
	 */
	it('ranks the description below the title', () => {
		expect(rem(requireDeclaration('drawer', '.task-panel__description', 'font-size'))).toBeLessThan(
			rem(requireDeclaration('drawer', '.task-panel__title', 'font-size'))
		);
		// And the title keeps a weight the description does not declare at all.
		expect(Number(requireDeclaration('drawer', '.task-panel__title', 'font-weight'))).toBeGreaterThan(
			400
		);
		expect(styleOf('drawer', '.task-panel__description').has('font-weight')).toBe(false);
	});

	it('condenses the title to one line and a smaller size, and takes the description out', () => {
		const condensed = styleOf('drawer', '.task-panel__header--condensed .task-panel__title');
		expect(condensed.get('-webkit-line-clamp')).toBe('1');
		expect(condensed.get('line-clamp')).toBe('1');
		expect(rem(condensed.get('font-size') as string)).toBeLessThan(
			rem(requireDeclaration('drawer', '.task-panel__title', 'font-size'))
		);

		// The description is removed from the document rather than hidden, which is
		// the same rule the acts follow for a level that is not showing. Asserted on
		// the markup, because there is no CSS to read for something that is absent.
		//
		// **The condition gained a term and this assertion had to stop quoting it
		// whole.** Tapping a clamped description now expands it, and an expanded
		// description outlives the condense — the reader asked for those lines, so
		// scrolling does not take them back. What is being pinned is the part that is
		// a rule rather than a product decision: the row is gated by an `{#if}` on
		// `condensed`, and nothing hides it with CSS instead.
		const gate = SOURCE.drawer.match(/\{#if description !== null && [^}]*\}/);
		expect(gate, 'the description row is gated by an {#if} on `condensed`').not.toBeNull();
		expect(gate?.[0]).toContain('condensed');
		expect(styleOf('drawer', '.task-panel__description').get('display')).not.toBe('none');
	});

	/**
	 * **The transition is guarded here and could not be guarded anywhere else.**
	 * `app.css`'s `prefers-reduced-motion: reduce` block lists specific
	 * class-scoped selectors and reaches nothing in this file — the same gap
	 * `.ui-no-press` exists for. So the obvious reading, that a global block covers
	 * this, is wrong, and the test says so.
	 */
	it('turns the condense transition off for a reader who asked for less motion', () => {
		const css = stylesheet('drawer');
		const guard = css.slice(css.indexOf('@media (prefers-reduced-motion: reduce)'));
		expect(guard).toContain('.task-panel__header');
		expect(guard).toContain('.task-panel__title');
		expect(guard).toContain('transition: none');

		// Both of the things that animate are in it — a transition added without being
		// listed there is the failure this pins.
		const animated = ['.task-panel__header', '.task-panel__title'].filter((selector) =>
			styleOf('drawer', selector).has('transition')
		);
		expect(animated).toEqual(['.task-panel__header', '.task-panel__title']);
	});

	/**
	 * `Badge` is a 999px pill, and a row of pills under a row of `--radius-sm`
	 * controls is two vocabularies in one header. This is the only thing the drawer
	 * says about a chip — the colour, the tone mapping and the per-theme treatments
	 * are all the component's.
	 */
	it('squares the chips to match the controls above them, and says nothing else about them', () => {
		const chip = styleOf('drawer', '.task-panel__chips :global(.task-panel__chip)');
		expect(chip.get('border-radius')).toBe('var(--radius-sm)');
		expect([...chip.keys()]).toEqual(['border-radius']);
	});
});

// ── A1.4 the output list ─────────────────────────────────────────────────────

describe('the output act gives each deliverable a row of its own', () => {
	/**
	 * Two or three files, each with a name, a size, a mime, up to five controls and
	 * possibly an opened preview. Flat against each other they read as one list of
	 * strings, which is what the owner reported.
	 */
	it('bounds each row and spaces it from the next', () => {
		const row = styleOf('panel', '.output-file');
		expect(row.get('border')).toContain('var(--border-soft)');
		expect(row.get('border-radius')).toBe('var(--radius-sm)');
		expect(row.get('padding')).toBe('var(--space-sm)');
		expect(styleOf('panel', '.output-files').get('gap')).toBe('var(--space-sm)');
	});

	/**
	 * **They were `<button>`s dressed as links, and the fix was not more CSS.**
	 *
	 * An underline is the web's word for navigation, and three of the five navigate
	 * nowhere — Preview expands the row, Open and Reveal ask the OS. The previous
	 * round answered that with twenty-three hand-written declarations naming the
	 * `Button` variant they copied; they are now that component, so the claim to
	 * check is where the treatment comes from rather than what the panel restates.
	 *
	 * Two of the five are `<a>` and the underline has to be off *there* — a
	 * `<button>` has none to remove — which is why this reads the component.
	 */
	it('makes every output control look like a control rather than a link', () => {
		const base = styleOf('button', '.native-button');
		expect(base.get('text-decoration')).toBe('none');
		expect(styleOf('button', '.native-button--outline').get('border-color')).toContain(
			'var(--border-soft)'
		);
		// The house outline hover fills rather than only recolouring, so the one
		// under the pointer is unambiguous among five.
		expect(
			styleOf('button', '.native-button--outline:not(:disabled):hover').get('background')
		).toContain('var(--bg-soft)');
	});
});

// ── A1.5 the drawer's place in the stack ─────────────────────────────────────

/**
 * The panel rendered **behind** the app's top bar: a dialog with
 * `aria-modal="true"` covered by the chrome it was modal over.
 *
 * Read out of the neighbours' own stylesheets rather than against remembered
 * numbers, so a later edit to either ladder rung fails here instead of putting
 * the panel back under something.
 */
describe('the drawer sits above the chrome and below the surfaces summoned over it', () => {
	const zIndexes = (relative: string): number[] =>
		[...uncommented(read(relative)).matchAll(/z-index:\s*(\d+)/g)].map((match) => Number(match[1]));

	/** The prop default, which every surface but the internal route takes. */
	const defaultLayer = (): number => {
		const match = SOURCE.drawer.match(/export let layer = (\d+);/);
		if (!match) throw new Error('TaskPanelDrawer declares no default layer');
		return Number(match[1]);
	};

	it('clears every rung the top bar occupies', () => {
		const topBar = zIndexes('src/lib/shell/TopBar.svelte').filter((value) => value > 0);
		expect(topBar.length).toBeGreaterThan(0);
		expect(defaultLayer()).toBeGreaterThan(Math.max(...topBar));
	});

	it('stays under the history drawer and the command palette, which open over it', () => {
		const above = [
			...zIndexes('src/lib/shell/HistoryDrawer.svelte'),
			...zIndexes('src/lib/shell/CommandPalette.svelte')
		].filter((value) => value > 0);

		expect(above.length).toBeGreaterThan(0);
		expect(defaultLayer()).toBeLessThan(Math.min(...above));
	});

	it('states the same number in the stylesheet’s fallback as the prop’s default', () => {
		// The fallback is unreachable — the element always sets the property inline —
		// so its only reader is a person, and a person must not be told a different
		// number than the one that ships.
		const declared = requireDeclaration('drawer', '.task-panel-backdrop', 'z-index');
		expect(declared).toBe(`var(--task-panel-layer, ${defaultLayer()})`);
	});
});

// ── A2 interaction ───────────────────────────────────────────────────────────

/**
 * Every control the panel and its drawer still **style themselves**, and where.
 *
 * It is one row long, and that is the finding of A2 rather than an omission: the
 * other four — `Details`, the five output-row controls, `Retry`, the close — went
 * through `native/Button.svelte`, so their size, border, hover, focus ring and
 * per-theme treatments are the component's and are not restated here. What is
 * left is the one control the component cannot be: a full-width disclosure row
 * carrying a marker, a title and a summary, which `Button` has no slot for.
 *
 * `adopts the house control` below is what keeps this list from growing back.
 */
const CONTROLS: { key: FileKey; selector: string }[] = [{ key: 'act', selector: '.act__header' }];

/** The floor `native/Button.svelte` gives even its `sm` size. */
const TARGET_FLOOR = 1.75;

describe('the panel borrows the house interaction rather than hand-rolling one', () => {
	/**
	 * `app.css` presses every `button` by `scale(0.96)`. On a row spanning a
	 * 560px drawer that is a ~21px squeeze, which makes the row behave like a
	 * card — the one gesture this component exists not to make. `.ui-no-press` is
	 * the house opt-out, already used by Today, Square and `rowInteractions.ts`.
	 *
	 * Read off the markup rather than the stylesheet: the rule lives in
	 * `app.css`, and what this component contributes is the class on the button.
	 */
	it('suppresses the app-wide press transform on the act row', () => {
		const header = SOURCE.act.match(/class="act__header[^"]*"/);
		expect(header?.[0]).toContain('ui-no-press');
	});

	/**
	 * The press rule is not covered by `app.css`'s reduced-motion guard, which
	 * lists the class-scoped selectors and not the bare `button:active` one. So
	 * the opt-out above is the only thing standing between a reader who asked for
	 * less motion and the squeeze — worth stating, because the obvious reading of
	 * "there is a reduced-motion block" is that this is already handled.
	 */
	it('is the only thing suppressing it, because the reduced-motion guard does not reach that rule', () => {
		const css = uncommented(read(APP_CSS));
		const guard = css.slice(css.indexOf('@media (prefers-reduced-motion: reduce)'));
		expect(css).toContain("button:active:not(:disabled, .ui-no-press)");
		expect(guard.slice(0, guard.indexOf('}\n}'))).not.toContain('button:active:not(:disabled,');
	});

	it('gives every control a hover state, not only a focus ring', () => {
		const withoutHover = CONTROLS.filter((control) => {
			const rules = parseRules(stylesheet(control.key));
			return !rules.some((rule) => rule.selectors.includes(`${control.selector}:hover`));
		});

		// Named rather than counted, so a failure says which control a mouse user
		// still gets no signal from.
		expect(withoutHover.map((control) => control.selector)).toEqual([]);
	});

	it('gives every control a focus ring as well, so the hover above did not replace one', () => {
		const withoutFocus = CONTROLS.filter((control) => {
			const rules = parseRules(stylesheet(control.key));
			return !rules.some((rule) => rule.selectors.includes(`${control.selector}:focus-visible`));
		});

		expect(withoutFocus.map((control) => control.selector)).toEqual([]);
	});

	/**
	 * The act row is sized by its own content and its `--space-sm` vertical
	 * padding; every other control in the panel is now `Button`, whose smallest
	 * size is where this floor came from — so the floor is asserted at its source
	 * rather than restated per control.
	 */
	it('clears the target floor at the size every one of these controls is rendered at', () => {
		expect(rem(requireDeclaration('button', '.native-button--sm', 'min-height'))).toBeGreaterThanOrEqual(
			TARGET_FLOOR
		);
	});
});

// ── A2 the design system, not an imitation of it ──────────────────────────────

/**
 * **The finding this section exists for, in the owner's words: "the output
 * section is not themed".**
 *
 * It was themed, in the sense that every colour came from a token. What it was
 * not was *the design system*: `UnifiedTaskPanel.svelte` imported zero components
 * from `components/native/` or `components/generative/` and hand-rolled nine
 * controls out of `border: 1px solid var(--border-soft)` and friends — with
 * comments on individual declarations naming the `Button` variant each one was
 * copying. Design §1 records that `InternalTasksWorkspace` was 1,843 lines that
 * never touched the design system, and this panel replaced it by reproducing that
 * property.
 *
 * The concrete cost, which is what makes this a defect rather than a preference:
 * `Button`, `Badge` and `Tag` each carry a `[data-theme^='retro-16bit']` block
 * that squares the corners and switches to the mono face. Hand-rolled CSS gets
 * none of it, so in that theme every control in the app changed shape except the
 * ones in this panel. A token is not a theme.
 *
 * What these cases cannot say: whether the result looks right. That is still a
 * visual pass.
 */
describe('the panel adopts the house control rather than approximating it', () => {
	/** `<button` in markup, outside comments — the shape of a hand-rolled control. */
	const handRolledButtons = (key: FileKey): string[] => {
		const source = SOURCE[key];
		const markup = source.slice(source.indexOf('</script>')).replace(/<!--[\s\S]*?-->/g, '');
		return [...markup.matchAll(/<button[\s\S]{0,400}?class="([^"]*)"/g)].map((match) => match[1]);
	};

	/**
	 * **The hand-rolled controls, each with the reason `Button` cannot be it.**
	 *
	 * A count would say "four" and a list of class names would say which four; what
	 * matters is that each one is a stated limit of the component rather than
	 * somebody reaching for `border: 1px solid var(--border-soft)` again. A new
	 * `<button>` in these four files has to be added here with its reason, which is
	 * the same shape as the flex table above and exists for the same reason.
	 *
	 * It shipped as a single-element assertion and three controls joined the header
	 * in one commit, so the assertion said only that the number had changed.
	 */
	const HAND_ROLLED: Record<string, string> = {
		'act: act__header ui-no-press':
			'a full-width row carrying three spans and `aria-expanded`; `Button` renders a label and an icon slot',
		'panel: output-file__thumb':
			'an authenticated image hit target whose child is the thumbnail itself; `Button` cannot render arbitrary image content',
		'panel: output-preview__figure':
			'an authenticated full-size figure whose child is the preview image; `Button` cannot render arbitrary figure content',
		'drawer: task-panel__title':
			'the text of an `<h2>`, clamped to two lines and expanding on tap — a heading, not a control with a label',
		'drawer: task-panel__description':
			'the same clamped-prose shape as the title, five lines, and it must read as the paragraph it used to be',
		'drawer: task-panel__id':
			'the smallest tier, dimmed and monospace, deliberately not looking like a control; every `Button` declaration here would be a reset'
	};

	it('leaves only the hand-rolled controls the component cannot be, each with its reason', () => {
		const found = PANEL_FILES.flatMap((key) =>
			handRolledButtons(key).map((classes) => `${key}: ${classes}`)
		);

		// Sorted comparison of the two key sets, so a failure names the control that
		// appeared or the entry that is now stale rather than reporting a length.
		expect(found.filter((one) => HAND_ROLLED[one] === undefined)).toEqual([]);
		expect(Object.keys(HAND_ROLLED).filter((one) => !found.includes(one))).toEqual([]);
		// Every reason is a reason, not an empty string standing in for one.
		expect(Object.values(HAND_ROLLED).filter((why) => why.length < 20)).toEqual([]);
	});

	it('renders every other control through `native/Button.svelte`', () => {
		const importers = PANEL_FILES.filter((key) =>
			SOURCE[key].includes("from '$lib/magician/components/native/Button.svelte'")
		);

		// **`act` was here and dropped off when the `Details` disclosure was retired**
		// — that button was its only `Button`, so the file now renders one hand-rolled
		// header and nothing else. `TaskVerdictLine` renders no control at all: it is
		// one read-only line. Both absences are by construction rather than by
		// omission, which is why they are named here rather than left as a shorter
		// list.
		expect(importers).toEqual(['panel', 'drawer']);
		expect(SOURCE.verdict).not.toContain('<button');
	});

	/**
	 * **The two protected-output actions that must stay authenticated buttons.**
	 *
	 * A direct anchor cannot attach the workspace bearer and would put authority
	 * in a URL if scope were added there. Both actions therefore fetch through
	 * the authenticated helper and only then open or download a short-lived blob.
	 */
	it('routes open-in-tab and download through authenticated output helpers', () => {
		const markup = SOURCE.panel.slice(SOURCE.panel.indexOf('</script>'));

		const uses = [...markup.matchAll(/<Button[\s\S]{0,600}?\/>/g)].map((match) => match[0]);
		const inTab = uses.filter((use) => use.includes('label="In tab"'));
		const downloads = uses.filter((use) => use.includes('label="Download"'));
		expect(inTab).toHaveLength(1);
		expect(downloads).toHaveLength(1);
		expect(inTab[0]).toContain('openAuthenticatedTaskOutput(file.url!)');
		expect(downloads[0]).toContain('downloadAuthenticatedTaskOutput(file.url!, file.name)');
		expect(markup).not.toContain('href={file.url}');
		expect(markup).not.toMatch(/<a\s+class="output-file__action"/);
	});

	/**
	 * The panel keeps **no** CSS for the controls it handed over. A leftover rule
	 * would not merely be dead — `className` puts the class on an element inside
	 * the component, which carries the component's scope, so a scoped rule here
	 * would silently match nothing while reading as though it still applied.
	 */
	it('keeps no stylesheet copy of the controls it handed over', () => {
		const handedOver = [
			'.act__details',
			'.output-file__action',
			'.panel__retry',
			'.task-panel__close',
			'.ask__submit'
		];

		const restated = PANEL_FILES.flatMap((key) =>
			parseRules(stylesheet(key))
				.filter((rule) =>
					rule.selectors.some((one) => handedOver.includes(one))
				)
				.map((rule) => `${key}: ${rule.selectors.join(', ')}`)
		);

		expect(restated).toEqual([]);
	});

	/**
	 * The one thing the panel still says about a handed-over control is where it
	 * sits, and it has to say it through `:global` for the scoping reason above.
	 * Pinned so a later edit does not "fix" the `:global` away and silently lose
	 * the placement.
	 */
	it('positions them through `:global`, which is the only way a parent can reach them', () => {
		// `:global(.act__details)` was the third of these and went with the control:
		// the act has no handed-over button left to place. It stays in `handedOver`
		// above, which asserts no stylesheet copy of it comes back.
		expect(stylesheet('panel')).toContain(':global(.panel__retry)');
		expect(stylesheet('drawer')).toContain(':global(.task-panel__close)');
	});

	/**
	 * **The disclosure attributes are on the component, not worked around it.**
	 *
	 * Two of the handed-over controls expanded a region — `Details` and `Preview` —
	 * and `Button` carried no way to say so, which is part of why they were
	 * hand-rolled: a disclosure announced as a plain button is a component with a
	 * hole in it rather than a control that needed bespoke markup.
	 *
	 * **`Details` has since been retired, so `Preview` is the one consumer left.**
	 * The prop is asserted on the component either way: it is the component's own
	 * hole that was filled, and the panel's disclosure is the case that needs it.
	 */
	it('gives the component the disclosure attributes its consumers were hand-rolling for', () => {
		expect(SOURCE.button).toContain('export let ariaExpanded: boolean | null = null;');
		expect(SOURCE.button).toContain('aria-expanded={ariaExpanded === null ? undefined : ariaExpanded}');
		// `null` omits the attribute rather than asserting `false` on a button that
		// expands nothing.
		expect(SOURCE.panel).toContain('ariaExpanded={openPreview === index}');
	});

	/**
	 * **What the panel deliberately does not adopt, so the next reader does not
	 * "finish the job".**
	 *
	 * `native/Card.svelte` is the obvious candidate for the act sections and the
	 * output rows, and it is the one component this panel must not use: design §1's
	 * whole diagnosis is that the previous panel made everything a card in a grid,
	 * so nothing read as more important than anything else. An output row is
	 * `border: 1px solid var(--border-soft)` on the surface's own fill — a bounded
	 * row, explicitly not a raised card — and `.timeline-row` carries no frame at
	 * all for the same reason one level down.
	 */
	it('renders no Card, which is a design constraint rather than an oversight', () => {
		for (const key of PANEL_FILES) {
			expect(SOURCE[key]).not.toContain('components/native/Card.svelte');
			expect(SOURCE[key]).not.toContain('components/generative/Card.svelte');
		}
		// And the row that would have been one keeps its flat treatment.
		expect(styleOf('panel', '.output-file').get('box-shadow')).toBeUndefined();
		expect(styleOf('panel', '.timeline-row').has('border')).toBe(false);
	});
});

// ── A3 contrast, in every theme ──────────────────────────────────────────────

type Rgba = [number, number, number, number];

function splitTopLevel(text: string, separator: string): string[] {
	const parts: string[] = [];
	let depth = 0;
	let current = '';
	for (const char of text) {
		if (char === '(') depth += 1;
		if (char === ')') depth -= 1;
		if (char === separator && depth === 0) {
			if (current.trim()) parts.push(current.trim());
			current = '';
			continue;
		}
		current += char;
	}
	if (current.trim()) parts.push(current.trim());
	return parts;
}

const NAMED: Record<string, Rgba> = {
	transparent: [0, 0, 0, 0],
	white: [255, 255, 255, 1],
	black: [0, 0, 0, 1]
};

/**
 * Resolve a CSS colour expression against one theme's custom properties.
 *
 * Handles the four forms this app's tokens are written in — `var()` with and
 * without a fallback, `color-mix(in srgb, …)`, hex, and `rgb()`/`rgba()` — and
 * returns `null` for anything else rather than guessing, so an unresolvable
 * value fails loudly below instead of scoring as black on black.
 */
function resolveColour(
	value: string,
	tokens: Map<string, string>,
	seen: ReadonlySet<string> = new Set()
): Rgba | null {
	const text = value.trim();

	if (text.startsWith('var(')) {
		const args = splitTopLevel(text.slice(4, -1), ',');
		const name = args[0].trim();
		// A token defined in terms of itself resolves to nothing rather than
		// looping; `app.css`'s backfill block aliases several names both ways.
		if (seen.has(name)) return null;
		const next = new Set(seen).add(name);
		const declared = tokens.get(name);
		if (declared !== undefined) {
			const got = resolveColour(declared, tokens, next);
			if (got) return got;
		}
		return args.length < 2 ? null : resolveColour(args.slice(1).join(','), tokens, next);
	}

	if (text.startsWith('color-mix(')) {
		const args = splitTopLevel(text.slice('color-mix('.length, -1), ',');
		if (args[0].trim() !== 'in srgb') return null;
		const side = (part: string) => {
			const percent = part.match(/\s(-?[\d.]+)%$/);
			return {
				colour: resolveColour(
					percent ? part.slice(0, part.length - percent[0].length) : part,
					tokens,
					seen
				),
				weight: percent ? Number(percent[1]) / 100 : null
			};
		};
		const a = side(args[1]);
		const b = side(args[2]);
		if (!a.colour || !b.colour) return null;
		let wa = a.weight ?? (b.weight === null ? 0.5 : 1 - b.weight);
		let wb = b.weight ?? 1 - wa;
		const total = wa + wb || 1;
		wa /= total;
		wb /= total;
		// Premultiplied, as the specification mixes.
		const alpha = a.colour[3] * wa + b.colour[3] * wb;
		const channel = (i: 0 | 1 | 2) =>
			alpha === 0 ? 0 : (a.colour![i] * a.colour![3] * wa + b.colour![i] * b.colour![3] * wb) / alpha;
		return [channel(0), channel(1), channel(2), alpha];
	}

	if (text in NAMED) return NAMED[text];

	if (text.startsWith('#')) {
		const digits = text.slice(1);
		const pairs = digits.length <= 4 ? digits.split('').map((d) => d + d) : digits.match(/../g);
		if (!pairs || pairs.length < 3) return null;
		const [r, g, b, a] = pairs.map((pair) => parseInt(pair, 16));
		return [r, g, b, a === undefined ? 1 : a / 255];
	}

	const functional = text.match(/^rgba?\(([^)]*)\)$/);
	if (functional) {
		const parts = splitTopLevel(functional[1].replace(/\//g, ','), ',');
		if (parts.length < 3) return null;
		const channel = (part: string) =>
			part.endsWith('%') ? (Number(part.slice(0, -1)) / 100) * 255 : Number(part);
		const alphaPart = parts[3];
		const alpha =
			alphaPart === undefined
				? 1
				: alphaPart.endsWith('%')
					? Number(alphaPart.slice(0, -1)) / 100
					: Number(alphaPart);
		return [channel(parts[0]), channel(parts[1]), channel(parts[2]), alpha];
	}

	return null;
}

/**
 * Every colour a background expression can paint, so a background is judged at
 * its worst stop rather than at whichever stop happens to be written first.
 *
 * `resolveColour` returns `null` for a gradient, which for a *foreground* is the
 * right answer — there is no such thing as gradient text here. For a background
 * it silently discards the question: eight themes give the primary button a
 * multi-stop gradient, and two of them were hiding a failure in a stop no
 * single-colour reading reaches. `arcane-terminal` runs from a bright teal out
 * to `#9d4edd`, a purple dark enough that only pure black clears AA against it;
 * measured at its first stop that theme scored 10:1 and looked fine.
 */
function backgroundStops(
	value: string,
	tokens: Map<string, string>,
	seen: ReadonlySet<string> = new Set()
): Rgba[] {
	const text = value.trim();

	if (text.startsWith('var(')) {
		const args = splitTopLevel(text.slice(4, -1), ',');
		const name = args[0].trim();
		if (seen.has(name)) return [];
		const next = new Set(seen).add(name);
		const declared = tokens.get(name);
		if (declared !== undefined) {
			const got = backgroundStops(declared, tokens, next);
			if (got.length) return got;
		}
		return args.length < 2 ? [] : backgroundStops(args.slice(1).join(','), tokens, next);
	}

	const gradient = text.match(/^(?:repeating-)?(?:linear|radial|conic)-gradient\(([\s\S]*)\)$/);
	if (gradient) {
		const stops: Rgba[] = [];
		for (const part of splitTopLevel(gradient[1], ',')) {
			// A direction, shape or interpolation keyword is not a colour stop.
			if (/^(to\s|from\s|at\s|in\s|circle|ellipse|-?[\d.]+(deg|turn|rad|grad))/.test(part)) continue;
			// A stop may carry one or two positions after its colour.
			const colour = part.replace(/(\s+-?[\d.]+(%|px|rem|em|vw|vh)){1,2}$/, '').trim();
			const got = resolveColour(colour, tokens, seen);
			if (got) stops.push(got);
		}
		return stops;
	}

	const one = resolveColour(text, tokens, seen);
	return one ? [one] : [];
}

const composite = (over: Rgba, under: Rgba): Rgba =>
	over[3] >= 1
		? over
		: [
				over[0] * over[3] + under[0] * (1 - over[3]),
				over[1] * over[3] + under[1] * (1 - over[3]),
				over[2] * over[3] + under[2] * (1 - over[3]),
				1
			];

function relativeLuminance([r, g, b]: Rgba): number {
	const linear = (value: number) => {
		const channel = value / 255;
		return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
	};
	return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

function contrastRatio(foreground: Rgba, background: Rgba): number {
	const a = relativeLuminance(composite(foreground, background));
	const b = relativeLuminance(background);
	return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

// ── the themes ───────────────────────────────────────────────────────────────

const THEME_CSS = uncommented(read(APP_CSS));
const THEME_RULES = parseRules(THEME_CSS);

/**
 * Does this rule's selector list reach the root of a document running `theme`?
 *
 * `:root`, a bare `[data-theme]` and `[data-theme="x"]` all have the same
 * specificity, so the cascade among them is pure source order — which is why
 * the token map below is built by walking every rule once, in order, letting
 * later declarations win. The trailing backfill block in `app.css` depends on
 * exactly that: it is written as `:root, [data-theme]` and sits after every
 * theme so its aliases resolve against whichever theme is live.
 */
function reaches(selectors: string[], theme: string): boolean {
	return selectors.some((one) => {
		if (one === ':root') return true;
		const exact = one.match(/^\[data-theme(\^?=)("|')([^"']+)\2\]$/);
		if (exact) return exact[1] === '^=' ? theme.startsWith(exact[3]) : theme === exact[3];
		return one === '[data-theme]';
	});
}

function tokensFor(theme: string): Map<string, string> {
	const tokens = new Map<string, string>();
	for (const rule of THEME_RULES) {
		if (!reaches(rule.selectors, theme)) continue;
		for (const [prop, value] of rule.declarations) {
			if (prop.startsWith('--')) tokens.set(prop, value);
		}
	}
	return tokens;
}

/**
 * Does *this theme* declare the property, as opposed to inheriting it?
 *
 * `tokensFor` cannot answer that: it merges `:root` and the backfill in, which is
 * correct for measuring a colour and useless for asking whether a theme took a
 * position. Only selectors that name the theme count — which is the distinction
 * the whole `--text-on-accent` bug lived in.
 */
function declaresLocally(theme: string, property: string): boolean {
	return THEME_RULES.some((rule) => {
		if (!rule.declarations.has(property)) return false;
		return rule.selectors.some((one) => {
			const exact = one.match(/^\[data-theme(\^?=)("|')([^"']+)\2\]$/);
			if (!exact) return false;
			return exact[1] === '^=' ? theme.startsWith(exact[3]) : theme === exact[3];
		});
	});
}

/**
 * Every named theme in `app.css`, plus the pre-theme `:root` defaults the first
 * paint uses before the theme store mounts. Discovered rather than listed: a
 * theme added to the stylesheet joins this sweep without anyone remembering to
 * add it, which is the half of "check every theme" that a hand-kept list loses
 * first.
 */
const THEMES = [
	// A name no `[data-theme="…"]` rule matches, which is what an unthemed
	// document is: `:root` and the backfill, and nothing else.
	'(pre-theme :root)',
	...[
		...new Set(
			THEME_RULES.flatMap((rule) =>
				rule.selectors.flatMap((one) => {
					const exact = one.match(/^\[data-theme=("|')([^"']+)\1\]$/);
					return exact ? [exact[2]] : [];
				})
			)
		)
	].sort()
];

// ── the pairs the panel actually renders ─────────────────────────────────────

/** The drawer's body, which everything in the panel is painted on. */
const SURFACE = 'var(--bg-elevated, var(--bg-secondary))';

/**
 * The washes the verdict block paints behind itself, as `VERDICT_TONE` bands
 * them. Paused and archived are neutral, so they render on the unwashed surface;
 * `TaskVerdictLine.component.test.ts` pins every state-to-band mapping.
 */
const VERDICT_WASHES = [
	null,
	'var(--status-attention-soft)',
	'var(--status-failed-soft)',
	'var(--status-running-soft)',
	'var(--status-completed-soft)'
];

/** Nothing washes an act; the header takes `--bg-soft` under the cursor. */
const ACT_WASHES = [null, 'var(--bg-soft)'];

/** WCAG AA: 4.5:1 for text, 3:1 for a graphic that carries meaning. */
const TEXT_FLOOR = 4.5;
const GLYPH_FLOOR = 3;

interface Pair {
	what: string;
	key: FileKey;
	selector: string;
	/** The colour the stylesheet must declare, so this table cannot drift from it. */
	colour: string;
	washes: (string | null)[];
	floor: number;
}

const PAIRS: Pair[] = [
	// L0 — on the wash, which is the pairing `jarvis` made unreadable.
	{
		what: 'verdict headline',
		key: 'verdict',
		selector: '.verdict__headline',
		colour: 'var(--text-primary)',
		washes: VERDICT_WASHES,
		floor: TEXT_FLOOR
	},
	{
		what: 'verdict detail',
		key: 'verdict',
		selector: '.verdict__detail',
		colour: 'var(--text-secondary)',
		washes: VERDICT_WASHES,
		floor: TEXT_FLOOR
	},
	{
		what: 'verdict state glyph',
		key: 'verdict',
		selector: '.verdict__marker',
		colour: 'color-mix(in srgb, var(--verdict-tone) 60%, var(--text-primary))',
		washes: VERDICT_WASHES,
		floor: GLYPH_FLOOR
	},

	// L1 and L2 — the act column.
	{
		what: 'act header text',
		key: 'act',
		selector: '.act__header',
		colour: 'var(--text-primary)',
		washes: ACT_WASHES,
		floor: TEXT_FLOOR
	},
	{
		what: 'act disclosure glyph',
		key: 'act',
		selector: '.act__marker',
		colour: 'var(--text-secondary)',
		washes: ACT_WASHES,
		floor: GLYPH_FLOOR
	},
	{
		what: 'act summary',
		key: 'act',
		selector: '.act__summary',
		colour: 'var(--text-secondary)',
		washes: ACT_WASHES,
		floor: TEXT_FLOOR
	},
	// The controls the panel handed to `native/Button.svelte`, measured where they
	// are now declared. One entry per *variant the panel renders*, not one per
	// control: `Details`, the five output-row controls, `Retry` and the close are
	// all `outline` at `sm`, so they are one pairing and a regression in it is a
	// regression in all eight.
	{
		what: 'a handed-over control (Button, outline)',
		key: 'button',
		selector: '.native-button--outline',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: 'a handed-over control, hovered',
		key: 'button',
		selector: '.native-button--outline:not(:disabled):hover',
		colour: 'var(--button-outline-hover-color, var(--text-primary))',
		washes: [null, 'var(--button-outline-hover-bg, var(--bg-soft))'],
		floor: TEXT_FLOOR
	},
	{
		what: "the ask's submit (Button, secondary)",
		key: 'button',
		selector: '.native-button--secondary',
		colour: 'var(--button-secondary-color, var(--text-secondary))',
		// It fills, so its own fill is the only surface it is ever read on.
		washes: ['var(--button-secondary-bg, var(--bg-soft))'],
		floor: TEXT_FLOOR
	},

	// L3 — provenance.
	{
		what: 'provenance label',
		key: 'act',
		selector: '.act__provenance dt',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: 'provenance value',
		key: 'act',
		selector: '.act__provenance dd',
		colour: 'var(--text-primary)',
		washes: [null],
		floor: TEXT_FLOOR
	},

	// The act bodies.
	{
		what: 'a planning ask',
		key: 'panel',
		selector: '.plan-ask',
		colour: 'var(--text-primary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: 'a run step',
		key: 'panel',
		selector: '.run-step',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: 'the live run step',
		key: 'panel',
		selector: '.run-step--current',
		colour: 'var(--text-primary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a step's retry count",
		key: 'panel',
		selector: '.run-step__retries',
		colour: 'color-mix(in srgb, var(--status-attention) 50%, var(--text-primary))',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a step's duration",
		key: 'panel',
		selector: '.run-step__duration',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: 'an output row',
		key: 'panel',
		selector: '.output-file',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},

	// The Run act's second list. Every tier of a timeline row, swept for the first
	// time: the slice shipped without an entry here, so the row that carries the
	// panel's smallest text was the one text nothing measured.
	//
	// **All of them stay at `--text-secondary`, and that is a constraint rather
	// than a preference.** The token set has two quieter tiers — `--text-muted` and
	// `--text-faint` — and neither clears 4.5:1 against `--bg-elevated` in this
	// app's light themes; `--text-muted` does not clear 3:1. So the tier below the
	// title is spelled in size, tracking and case, which every theme renders
	// identically and greyscale keeps.
	{
		what: 'a timeline row title',
		key: 'panel',
		selector: '.timeline-row__title',
		colour: 'var(--text-primary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a timeline row's prose",
		key: 'panel',
		selector: '.timeline-row__body',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a timeline row's capture marker",
		key: 'panel',
		selector: '.timeline-row__capture',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a timeline row's clock and offset",
		key: 'panel',
		selector: '.timeline-row__when',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a timeline row's kind",
		key: 'panel',
		selector: '.timeline-row__kind',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a timeline row's latency",
		key: 'panel',
		selector: '.timeline-row__latency',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a timeline row's cost line",
		key: 'panel',
		selector: '.timeline-row__meta',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "a timeline row's stdout",
		key: 'panel',
		// The one block in the feed with a wash of its own, and the reason it has
		// one: a recessed container is what says "this is the machine's words".
		selector: '.timeline-row__detail',
		colour: 'var(--text-primary)',
		washes: ['var(--bg-soft)'],
		floor: TEXT_FLOOR
	},
	// The three failures, and the chrome.
	{
		what: 'the staleness line',
		key: 'panel',
		selector: '.panel__stale',
		colour: 'var(--text-secondary)',
		// Its own wash, and the only place `--status-paused-soft` is painted.
		washes: ['var(--status-paused-soft)'],
		floor: TEXT_FLOOR
	},
	{
		what: 'the load failure headline',
		key: 'panel',
		selector: '.panel__load-error',
		colour: 'var(--text-primary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: 'the load failure detail',
		key: 'panel',
		selector: '.panel__load-error-detail',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: 'the drawer heading',
		key: 'drawer',
		selector: '.task-panel__title',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	},
	{
		what: "the header's description row",
		key: 'drawer',
		selector: '.task-panel__description',
		colour: 'var(--text-secondary)',
		washes: [null],
		floor: TEXT_FLOOR
	}
];

describe('every text the panel renders clears AA in every theme', () => {
	it('sweeps more than twenty themes, discovered from the stylesheet rather than listed', () => {
		// A floor on the sweep itself. Without it a parsing change that stopped
		// finding theme blocks would leave every assertion below passing over an
		// empty set — the shape of failure a source-reading test is most prone to.
		expect(THEMES.length).toBeGreaterThanOrEqual(23);
		expect(THEMES).toContain('jarvis');
		expect(THEMES).toContain('retro-16bit-light');
	});

	/**
	 * The table above names the colour each element must declare. Without this,
	 * a control could be pointed back at `--text-muted` and the sweep below would
	 * go on measuring the colour the table remembers — two mechanisms, one job,
	 * and the one that stopped working would be the one doing it.
	 */
	it('measures the colours the stylesheets actually declare', () => {
		const drifted = PAIRS.filter(
			(pair) => requireDeclaration(pair.key, pair.selector, 'color') !== pair.colour
		);

		expect(drifted.map((pair) => `${pair.selector} → ${styleOf(pair.key, pair.selector).get('color')}`)).toEqual([]);
	});

	it('clears the floor for every element, on every surface it renders on', () => {
		const failures: string[] = [];

		for (const theme of THEMES) {
			const tokens = tokensFor(theme);
			const base = resolveColour(SURFACE, tokens);
			if (!base) {
				failures.push(`${theme}: the panel's own surface does not resolve`);
				continue;
			}

			for (const pair of PAIRS) {
				for (const wash of pair.washes) {
					const tint = wash === null ? base : resolveColour(wash, tokens);
					if (!tint) {
						failures.push(`${theme} · ${pair.what}: ${wash} does not resolve`);
						continue;
					}
					const background = wash === null ? base : composite(tint, base);

					// The marker's colour is written against `--verdict-tone`, which the
					// band rules set per state. Bind it to the band whose wash is under
					// it, so the pair measured is the pair rendered — a marker is never
					// painted in one band's colour over another band's wash. With no
					// wash the state is `neutral`, which is what `.verdict` defaults to.
					const band = wash?.match(/^var\(--(status-\w+)-soft\)$/);
					const local = new Map(tokens);
					local.set(
						'--verdict-tone',
						band ? `var(--${band[1]})` : 'var(--text-secondary)'
					);

					const foreground = resolveColour(pair.colour, local);
					if (!foreground) {
						failures.push(`${theme} · ${pair.what}: ${pair.colour} does not resolve`);
						continue;
					}

					const ratio = contrastRatio(foreground, background);
					if (ratio < pair.floor) {
						failures.push(
							`${theme} · ${pair.what} on ${wash ?? 'the panel surface'}: ` +
								`${ratio.toFixed(2)}:1 (needs ${pair.floor}:1)`
						);
					}
				}
			}
		}

		// Every failure at once rather than the first: a token regression usually
		// takes a whole theme down, and one line at a time is a slow way to read
		// that.
		expect(failures).toEqual([]);
	});

	/**
	 * The one shared token this workstream changed, and the reason it had to be
	 * the token rather than a local workaround.
	 *
	 * A `*-soft` token is a translucent tint of its accent — that is what every
	 * consumer treats it as, a wash to put text on. `app.css`'s backfill block
	 * aliased this one to the accent at full strength, so the two themes that
	 * derive `--status-attention-soft` from it painted an opaque panel where a
	 * wash belonged. The task panel's verdict was not the first thing to be hurt
	 * by it: `Badge`'s attention row paints its label in `--status-attention` on
	 * a background of `--status-attention-soft`, which in those themes is the
	 * same colour, at 1:1.
	 */
	it('keeps the soft accent tokens translucent, which is what makes them safe to put text on', () => {
		const opaque = THEMES.filter((theme) => {
			const soft = resolveColour('var(--status-attention-soft)', tokensFor(theme));
			const solid = resolveColour('var(--status-attention)', tokensFor(theme));
			if (!soft || !solid) return false;
			// The failure shape: a "soft" token that is its own solid colour.
			return soft[3] >= 1 && soft.slice(0, 3).every((c, i) => Math.abs(c - solid[i]) < 1);
		});

		expect(opaque).toEqual([]);
	});
});

// ── A3.1 the house primary button, in every theme ─────────────────────────────

/**
 * **Why this sweep is here and not in a file of its own.**
 *
 * `native/Button.svelte` is not the panel's, and the block above is careful to
 * say so. But the panel renders nine of them, its controls *are* that component,
 * and the machinery for "resolve a token against one theme and measure it" only
 * exists here. The alternative was a second copy of the resolver, the theme
 * discovery and the contrast maths — two harnesses, and the one that stopped
 * being maintained would be the one still passing.
 *
 * What it protects, concretely: `--text-on-accent` was `#ffffff` at `:root` and
 * that value reached every theme that never overrode it. White is only ever
 * correct on a *dark* accent, and most of this app's accents are bright, so the
 * primary button failed AA in nine of twenty-three themes — `retro-16bit` paints
 * amber text on an amber accent and scored 1.83:1. Nothing caught it because
 * nothing measured the pair.
 */

/** Exactly as `.native-button--primary` declares them; asserted below. */
const BUTTON_FOREGROUND = 'var(--button-primary-color, var(--text-on-accent))';
const BUTTON_BACKGROUND = 'var(--button-primary-bg, var(--accent-primary))';

/**
 * **The one theme whose primary button no foreground can fix, left for its
 * owner.**
 *
 * Changing `--accent-primary` is a brand decision and not this sweep's to make,
 * so the theme is exempted rather than quietly recoloured. The exemption is not
 * taken on trust: the test below re-derives it, and fails if the background ever
 * becomes one that *does* admit a compliant foreground — which is what makes the
 * list shrink when the owner acts, instead of outliving the problem.
 */
const AWAITING_AN_OWNER_DECISION = ['arcane-terminal-light'];

describe('the house primary button clears AA in every theme', () => {
	/**
	 * The two expressions the sweep measures, read off the component rather than
	 * remembered. Point `.native-button--primary` at a different token and this
	 * fails here, instead of the sweep going on measuring a pair nothing renders.
	 */
	it('measures the colours Button.svelte actually declares', () => {
		expect(requireDeclaration('button', '.native-button--primary', 'color')).toBe(BUTTON_FOREGROUND);
		expect(requireDeclaration('button', '.native-button--primary', 'background')).toBe(BUTTON_BACKGROUND);
	});

	/**
	 * Why the floor is 4.5 and not the 3:1 large-text exemption: the largest of
	 * the three sizes is `0.875rem` at `font-weight: 600`. WCAG's exemption starts
	 * at 18.66px bold (1.1667rem), so no button in this component can ever reach
	 * it — and a later size that did would have to change this assertion first.
	 */
	it('is too small for the large-text exemption, so 4.5:1 is the floor', () => {
		const sizes = ['sm', 'md', 'lg'].map((size) =>
			rem(requireDeclaration('button', `.native-button--${size}`, 'font-size'))
		);
		expect(Math.max(...sizes)).toBeLessThan(1.1667);
		expect(requireDeclaration('button', '.native-button', 'font-weight')).toBe('600');
	});

	it('clears 4.5:1 in every theme, against every stop of its background', () => {
		const failures: string[] = [];

		for (const theme of THEMES) {
			if (AWAITING_AN_OWNER_DECISION.includes(theme)) continue;

			const tokens = tokensFor(theme);
			const foreground = resolveColour(BUTTON_FOREGROUND, tokens);
			const stops = backgroundStops(BUTTON_BACKGROUND, tokens);

			// An unresolvable foreground is the failure mode a missing token takes:
			// `color` becomes invalid at computed-value time and inherits whatever
			// the button sits inside. Never let that score as a pass.
			if (!foreground) {
				failures.push(`${theme}: the primary button's foreground does not resolve`);
				continue;
			}
			if (!stops.length) {
				failures.push(`${theme}: the primary button's background does not resolve`);
				continue;
			}

			for (const stop of stops) {
				const ratio = contrastRatio(foreground, stop);
				if (ratio < TEXT_FLOOR) {
					failures.push(
						`${theme}: primary button label ${ratio.toFixed(2)}:1 ` +
							`(needs ${TEXT_FLOOR}:1) on background stop ${stop.slice(0, 3).map(Math.round).join(',')}`
					);
				}
			}
		}

		expect(failures).toEqual([]);
	});

	/**
	 * The exemption, re-derived rather than believed. If neither near-black nor
	 * near-white clears the floor then the background genuinely has to move and
	 * no foreground edit would have helped; the moment that stops being true the
	 * theme owes a foreground again, and this fails until it is taken off the list.
	 */
	it('exempts only a theme whose background admits no compliant foreground at all', () => {
		const undeserved: string[] = [];

		for (const theme of AWAITING_AN_OWNER_DECISION) {
			expect(THEMES).toContain(theme);
			const stops = backgroundStops(BUTTON_BACKGROUND, tokensFor(theme));
			expect(stops.length).toBeGreaterThan(0);

			const best = (colour: Rgba) => Math.min(...stops.map((stop) => contrastRatio(colour, stop)));
			const black = best([0, 0, 0, 1]);
			const white = best([255, 255, 255, 1]);
			if (black >= TEXT_FLOOR || white >= TEXT_FLOOR) {
				undeserved.push(
					`${theme} no longer needs an owner decision: black reaches ${black.toFixed(2)}:1 ` +
						`and white ${white.toFixed(2)}:1 — give it a foreground and drop the exemption`
				);
			}
		}

		expect(undeserved).toEqual([]);
	});

	/**
	 * **A theme that forgets the token must fail, not inherit.**
	 *
	 * This is the half of the bug that a contrast sweep alone would keep letting
	 * through. Four themes never declared `--text-on-accent`, so they silently
	 * took `:root`'s value — and because `:root` matches the same `<html>` the
	 * `[data-theme]` rules do, at the same specificity, the token is *never*
	 * undefined and nothing looks broken. `retro-16bit` inherited white onto an
	 * amber accent that way. Requiring the declaration is what makes a new theme
	 * state its own answer rather than quietly adopting one that does not fit.
	 */
	it('makes every theme state its own --text-on-accent', () => {
		const silent = THEMES.filter(
			(theme) => theme !== '(pre-theme :root)' && !declaresLocally(theme, '--text-on-accent')
		);

		expect(silent).toEqual([]);
	});

	/**
	 * **Three names, one colour — asserted so they cannot drift apart again.**
	 *
	 * `--accent-contrast` and `--accent-on-primary` were already nothing but
	 * aliases of `--text-on-accent`: one declaration each, in the backfill block,
	 * and no theme has ever overridden either. Consolidating them therefore drops
	 * no theme's value. What it does not do on its own is stay consolidated, which
	 * is what this pins — sixteen consumers spread across the app read one of the
	 * two aliases rather than the token, and a per-theme override of just one of
	 * them would give the same surface two different answers.
	 */
	it('keeps --text-on-accent, --accent-contrast and --accent-on-primary one colour', () => {
		const split: string[] = [];

		for (const theme of THEMES) {
			const tokens = tokensFor(theme);
			const canonical = resolveColour('var(--text-on-accent)', tokens);
			if (!canonical) {
				split.push(`${theme}: --text-on-accent does not resolve`);
				continue;
			}
			for (const alias of ['--accent-contrast', '--accent-on-primary']) {
				const got = resolveColour(`var(${alias})`, tokens);
				if (!got || got.some((channel, i) => Math.abs(channel - canonical[i]) > 0.5)) {
					split.push(`${theme}: ${alias} has drifted from --text-on-accent`);
				}
			}
		}

		expect(split).toEqual([]);
	});

	/**
	 * **The name that had to come *out* of that family.**
	 *
	 * `--text-inverse` was aliased to `--text-on-accent` too, but its consumers
	 * paint it on `--bg-inverse`, which is `--text-primary` — a different
	 * background, so a different contract, and merging it in would have been a
	 * merge that lost. Aliased it failed in four themes, and in `cartoon` resolved
	 * to the exact colour it was painted on: `Tooltip` drew its text at 1:1.
	 */
	it('measures --text-inverse against the background it is actually painted on', () => {
		const failures: string[] = [];

		for (const theme of THEMES) {
			const tokens = tokensFor(theme);
			const foreground = resolveColour('var(--text-inverse)', tokens);
			const background = resolveColour('var(--bg-inverse)', tokens);
			if (!foreground || !background) {
				failures.push(`${theme}: --text-inverse or --bg-inverse does not resolve`);
				continue;
			}
			const ratio = contrastRatio(foreground, background);
			if (ratio < TEXT_FLOOR) {
				failures.push(`${theme}: --text-inverse on --bg-inverse is ${ratio.toFixed(2)}:1`);
			}
		}

		expect(failures).toEqual([]);
	});
});
