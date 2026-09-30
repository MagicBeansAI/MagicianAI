import { AURORA, mixStops, type AuroraStop } from './auroraPalette';

export interface ThemeThreadPalette {
	primary: AuroraStop;
	secondary: AuroraStop;
	glowPrimary: AuroraStop;
	glowSecondary: AuroraStop;
	ink: AuroraStop;
	light: AuroraStop;
}

const BLACK: AuroraStop = { r: 0, g: 0, b: 0 };
const WHITE: AuroraStop = { r: 1, g: 1, b: 1 };

/**
 * Raise a theme color's emitted-light peak without changing its hue. Mixing a
 * dark accent with white turns it pastel; proportional channel scaling keeps
 * rust rust, forest green green, and monochrome neutral while making each
 * capable of producing a visible halo. Near-black colors become neutral light
 * because black itself cannot emit a colored glow.
 */
export function luminousThemeColor(color: AuroraStop, peak = 0.9): AuroraStop {
	const currentPeak = Math.max(color.r, color.g, color.b);
	if (currentPeak < 0.015) {
		return { r: peak, g: peak, b: peak };
	}
	const scale = Math.max(1, peak / currentPeak);
	return {
		r: Math.min(1, color.r * scale),
		g: Math.min(1, color.g * scale),
		b: Math.min(1, color.b * scale)
	};
}

function byte(value: string): number | null {
	const parsed = Number.parseInt(value, 16);
	return Number.isFinite(parsed) ? parsed / 255 : null;
}

/**
 * Convert the computed value of a theme color token into the normalized sRGB
 * shape used by the movie canvas. Current app themes publish hex values, while
 * rgb()/rgba() support keeps the boundary safe for future theme additions.
 */
export function parseThemeColor(value: string, fallback: AuroraStop): AuroraStop {
	const raw = value.trim();
	const shortHex = raw.match(/^#([\da-f])([\da-f])([\da-f])(?:[\da-f])?$/i);
	if (shortHex) {
		const r = byte(`${shortHex[1]}${shortHex[1]}`);
		const g = byte(`${shortHex[2]}${shortHex[2]}`);
		const b = byte(`${shortHex[3]}${shortHex[3]}`);
		if (r !== null && g !== null && b !== null) return { r, g, b };
	}

	const longHex = raw.match(/^#([\da-f]{2})([\da-f]{2})([\da-f]{2})(?:[\da-f]{2})?$/i);
	if (longHex) {
		const r = byte(longHex[1]);
		const g = byte(longHex[2]);
		const b = byte(longHex[3]);
		if (r !== null && g !== null && b !== null) return { r, g, b };
	}

	const functional = raw.match(/^rgba?\(\s*([\d.]+)%?[\s,]+([\d.]+)%?[\s,]+([\d.]+)%?/i);
	if (functional) {
		const percent = raw.slice(0, raw.indexOf(')')).includes('%');
		const scale = percent ? 100 : 255;
		const values = functional.slice(1, 4).map((part) => Number.parseFloat(part) / scale);
		if (values.every(Number.isFinite)) {
			return {
				r: Math.min(1, Math.max(0, values[0])),
				g: Math.min(1, Math.max(0, values[1])),
				b: Math.min(1, Math.max(0, values[2]))
			};
		}
	}

	return { ...fallback };
}

/**
 * Keep the living thread legible on both paper and deep grounds while making
 * its identity the selected theme's own primary/secondary pair.
 */
export function themeThreadPalette(primaryValue: string, secondaryValue: string): ThemeThreadPalette {
	const fallbackPrimary = AURORA.violetSurge.halo;
	const fallbackSecondary = AURORA.violetSurge.stops[1];
	const primary = parseThemeColor(primaryValue, fallbackPrimary);
	const secondary = parseThemeColor(secondaryValue, fallbackSecondary);
	return {
		primary,
		secondary,
		glowPrimary: luminousThemeColor(primary, 1),
		glowSecondary: luminousThemeColor(secondary, 0.96),
		ink: mixStops(primary, BLACK, 0.58),
		light: mixStops(primary, WHITE, 0.8)
	};
}
