// The October Bright Sidekick landing palette. CSS declarations are scoped
// to .lp-root in routes/+page.svelte; keep those literals and these values
// together. Appearance changes never modify the shared app theme.
export const WARM_GROUND = {
	base: '#faf8f5',
	elevated: '#ffffff',
	soft: '#f0ebf6'
} as const;

export const WARM_TEXT = {
	primary: '#1a1612',
	secondary: '#49444f',
	faint: '#66606d',
	onAccent: '#faf8f5'
} as const;

export const WARM_ACCENT = {
	primary: '#1a1612',
	secondary: '#61506f'
} as const;

export const WARM_PALETTE = {
	ground: WARM_GROUND,
	text: WARM_TEXT,
	accent: WARM_ACCENT
} as const;

export const NIGHT_PALETTE = {
	ground: { base: '#191b22', elevated: '#242731', soft: '#30313c' },
	text: { primary: '#faf8f5', secondary: '#dedbe5', faint: '#c0b8c8', onAccent: '#1a1612' },
	accent: { primary: '#ffc93c', secondary: '#eadcff' }
} as const;

export const BRAND_GOLD = '#ffc93c';
export const BRAND_GOLD_INK = '#1a1612';
