// "Sunday afternoon" — the landing's one pinned warm theme (phase 3, 2026-08-17).
//
// SOURCE OF TRUTH for the literal values `.lp-root` declares in
// `src/routes/+page.svelte` to pin the landing to its own palette
// regardless of the visitor's stored app theme (see the comment on
// `.lp-root` there for why this is done with CSS custom-property
// redeclaration rather than writing `data-theme`). This file does not
// generate that CSS block — keep the two in sync by hand; `warmPalette.test.ts`
// is what turns a forgotten sync into a failing number instead of a slowly
// wrong page.
//
// REPAINTED (second pass, same day): the accent pair below used to be a
// literal copy of `[data-theme="longhand"]` in `src/app.css` — burnt rust
// `#a04020` and sage `#4a6a3a`. Reported live as reading earthy/artisanal
// rather than clean; ink-blue and deep teal replace them here. The two files
// are no longer required to match — `longhand` is untouched and still a real
// choice in ThemeSwitcher for signed-in visitors; this is only ever what the
// marketing route pins for itself. The ground and text tones are unchanged
// (the cream paper background was kept as-is by request), so only
// `WARM_ACCENT` moved.
//
// `warmPalette.test.ts` gates every text-on-ground pair drawn from this
// object at WCAG AA. Contrast is a gate here, not a preference: if a pair
// fails, the fix is to change the token (an owner decision), never to relax
// the test.

export const WARM_GROUND = {
	/** --bg-base / --landing-bg */
	base: '#f3ead6',
	/** --bg-elevated / --bg-card / --landing-chip-bg / --landing-input-surface */
	elevated: '#faf3e0',
	/** --bg-soft */
	soft: '#e3d3ac'
} as const;

export const WARM_TEXT = {
	/** --text-primary / --text-ink */
	primary: '#1a1612',
	/** --text-secondary */
	secondary: '#3a2f24',
	/** --text-faint — used only at small sizes today (placeholders, mono
	 *  labels), so 4.5:1 is the real bar. longhand ships `#97836b` at
	 *  3.04:1; darkened here to the lightest tone on the same hue ramp that
	 *  clears AA on ALL three grounds it renders against, scoped to the landing
	 *  theme. */
	faint: '#695948',
	/** --text-on-accent */
	onAccent: '#faf3e0'
} as const;

export const WARM_ACCENT = {
	/** --accent-primary — ink blue, the one strong accent */
	primary: '#28437a',
	/** --accent-secondary — deep teal */
	secondary: '#12665a'
} as const;

export const WARM_PALETTE = {
	ground: WARM_GROUND,
	text: WARM_TEXT,
	accent: WARM_ACCENT
} as const;
