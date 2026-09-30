// Contrast is a gate, not a preference (phase 3, 2026-08-17). Every
// text-on-ground pair the landing actually paints from `warmPalette.ts` is
// checked against WCAG AA here: 4.5:1 for normal text, 3:1 for large text
// (>=24px, or >=18.66px bold) — see each `it()` below for which usage in
// `src/lib/landing/*.svelte` / `src/routes/+page.svelte` motivated the pair
// and its threshold.
//
// The relative-luminance/contrast-ratio math mirrors the WCAG formula
// already implemented in
// `src/lib/magician/tasks/taskPanelPresentation.test.ts`
// (`relativeLuminance`/`contrastRatio`) — reproduced locally rather than
// imported because that module resolves colors out of live theme CSS for a
// different surface entirely; the arithmetic itself is standard and small
// enough that a second WCAG helper here is the honest choice, not a
// duplicated one.
import { describe, expect, it } from 'vitest';

import { WARM_PALETTE } from './warmPalette';

const AA_NORMAL = 4.5;
const AA_LARGE = 3.0;

function hexToRgb(hex: string): [number, number, number] {
	const clean = hex.replace('#', '');
	return [
		Number.parseInt(clean.slice(0, 2), 16),
		Number.parseInt(clean.slice(2, 4), 16),
		Number.parseInt(clean.slice(4, 6), 16)
	];
}

function relativeLuminance([r, g, b]: [number, number, number]): number {
	const linear = (value: number) => {
		const channel = value / 255;
		return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
	};
	return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

function contrastRatio(fgHex: string, bgHex: string): number {
	const a = relativeLuminance(hexToRgb(fgHex));
	const b = relativeLuminance(hexToRgb(bgHex));
	return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

const { ground, text, accent } = WARM_PALETTE;

describe('warm palette — normal text clears AA 4.5:1', () => {
	// Body copy on the page ground (LandingHero, DayTrack, MovieTrack,
	// TrustReveal, WhatItTakes, RoadModal all set `color: var(--text-primary)`
	// directly on the landing ground).
	it('text.primary on every ground', () => {
		for (const g of [ground.base, ground.elevated, ground.soft] as const) {
			expect(contrastRatio(text.primary, g)).toBeGreaterThanOrEqual(AA_NORMAL);
		}
	});

	// LandingAskComposer / ReelSwitcher body copy; DayTrack's .mt-mini-act
	// chip sits on --bg-soft specifically.
	it('text.secondary on base and soft', () => {
		expect(contrastRatio(text.secondary, ground.base)).toBeGreaterThanOrEqual(AA_NORMAL);
		expect(contrastRatio(text.secondary, ground.soft)).toBeGreaterThanOrEqual(AA_NORMAL);
	});

	// --accent-primary used as small link/label text: +page.svelte's
	// marketing-cta span/a and footer-mark, WhatItTakes' badge,
	// ReelSwitcher's active tab.
	it('accent.primary as link/label text on base and elevated', () => {
		expect(contrastRatio(accent.primary, ground.base)).toBeGreaterThanOrEqual(AA_NORMAL);
		expect(contrastRatio(accent.primary, ground.elevated)).toBeGreaterThanOrEqual(AA_NORMAL);
	});

	// --accent-secondary used the same way: +page.svelte's marketing-cta
	// hover state.
	it('accent.secondary as link/label text on base and elevated', () => {
		expect(contrastRatio(accent.secondary, ground.base)).toBeGreaterThanOrEqual(AA_NORMAL);
		expect(contrastRatio(accent.secondary, ground.elevated)).toBeGreaterThanOrEqual(AA_NORMAL);
	});

	// --text-on-accent on the two accents it actually sits on: CTA buttons
	// (LandingHero, PathFork, PathBeat, PathPromise, DayTrack) blend between
	// --accent-primary and --accent-secondary via themeThreadPalette, and
	// button text at these sizes (~13-16px) is not WCAG "large text".
	it('text.onAccent on both accents', () => {
		expect(contrastRatio(text.onAccent, accent.primary)).toBeGreaterThanOrEqual(AA_NORMAL);
		expect(contrastRatio(text.onAccent, accent.secondary)).toBeGreaterThanOrEqual(AA_NORMAL);
	});
});

describe('warm palette — text.faint was retuned for the landing', () => {
	// --text-faint is only ever used small in this codebase today:
	// LandingAskComposer's 14.5px placeholder and its 0.66rem/0.63rem mono
	// labels (stage-head, .is-done, .stage-file) — none of that clears the
	// WCAG "large text" carve-out (>=24px, or >=18.66px bold), so 4.5:1 is
	// the real bar. `longhand` ships `#97836b`, which measures 3.04:1 on this
	// ground and does not clear it. The landing pins a darker tone from the
	// same hue ramp (`#695948`) rather than inheriting the failure,
	// scoped to `.lp-root` so the shared app theme is untouched — retuning
	// that is a separate, owner-level call.
	it('text.faint clears AA on every ground it is used against', () => {
		expect(contrastRatio(text.faint, ground.base)).toBeGreaterThanOrEqual(AA_NORMAL);
		expect(contrastRatio(text.faint, ground.elevated)).toBeGreaterThanOrEqual(AA_NORMAL);
		// --bg-soft is the darkest ground the composer puts faint text on
		// (its stage rows), so it is the binding constraint, not base.
		expect(contrastRatio(text.faint, ground.soft)).toBeGreaterThanOrEqual(AA_NORMAL);
	});

	it('stays visibly fainter than text.secondary rather than collapsing into it', () => {
		expect(contrastRatio(text.faint, ground.base)).toBeLessThan(
			contrastRatio(text.secondary, ground.base)
		);
	});
});
