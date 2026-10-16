/**
 * Which office palette the app theme is asking for.
 *
 * The app ships twenty-two `[data-theme]` blocks and none of them declares
 * `color-scheme`, so there is no attribute to switch on and a hand-written list
 * of dark theme names would be wrong the moment a twenty-third lands. Instead
 * this MEASURES: it resolves the app's own background token on a live element
 * and reads its luminance. That is correct for every theme that exists and for
 * every theme that will exist, including user-authored ones.
 *
 * Kept out of the component so it can be reasoned about (and, if it ever grows,
 * tested) on its own.
 */

export type OfficeTone = 'light' | 'dark';

/** Rec. 709 relative luminance of an already-resolved CSS colour, or null when
 * the string is a form we cannot read (named colours, `color()`, gradients). */
export function luminanceOf(colour: string): number | null {
	const value = colour.trim();
	if (!value) return null;
	let r: number;
	let g: number;
	let b: number;
	const hex = value.match(/^#([0-9a-f]{3,8})$/i);
	if (hex) {
		const digits = hex[1];
		const full =
			digits.length === 3 || digits.length === 4
				? digits
						.slice(0, 3)
						.split('')
						.map((c) => c + c)
						.join('')
				: digits.slice(0, 6);
		if (full.length !== 6) return null;
		r = parseInt(full.slice(0, 2), 16);
		g = parseInt(full.slice(2, 4), 16);
		b = parseInt(full.slice(4, 6), 16);
	} else {
		const parts = value.match(/^rgba?\(([^)]+)\)$/i);
		if (!parts) return null;
		const nums = parts[1]
			.split(/[\s,/]+/)
			.filter(Boolean)
			.map((n) => (n.endsWith('%') ? (parseFloat(n) * 255) / 100 : parseFloat(n)));
		if (nums.length < 3 || nums.some((n) => Number.isNaN(n))) return null;
		[r, g, b] = nums;
	}
	const channel = (c: number): number => {
		const s = c / 255;
		return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
	};
	return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

/**
 * Resolve the tone the floor should paint in.
 *
 * `el` must be attached to the document — the whole point is to read the
 * cascade as it actually resolves for this subtree, not a token's declared
 * text. Falls back to light, which is what an unresolvable theme most likely
 * is and which is the palette the reference is drawn in.
 */
export function resolveOfficeTone(el: Element | null | undefined): OfficeTone {
	if (!el || typeof getComputedStyle !== 'function') return 'light';
	const style = getComputedStyle(el);
	const candidates = [
		style.getPropertyValue('--bg-base'),
		style.getPropertyValue('--bg-surface'),
		style.getPropertyValue('--bg-card'),
		style.backgroundColor
	];
	for (const candidate of candidates) {
		const lum = luminanceOf(candidate);
		if (lum != null) return lum < 0.22 ? 'dark' : 'light';
	}
	return 'light';
}
