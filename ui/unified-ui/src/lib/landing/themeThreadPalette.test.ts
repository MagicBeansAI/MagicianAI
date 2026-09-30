import { describe, expect, it } from 'vitest';

import {
	luminousThemeColor,
	parseThemeColor,
	themeThreadPalette
} from './themeThreadPalette';

describe('parseThemeColor', () => {
	const fallback = { r: 0.1, g: 0.2, b: 0.3 };

	it('accepts the hex formats used by current app themes', () => {
		expect(parseThemeColor('#ff6b6b', fallback)).toEqual({ r: 1, g: 107 / 255, b: 107 / 255 });
		expect(parseThemeColor('#0af', fallback)).toEqual({ r: 0, g: 170 / 255, b: 1 });
		expect(parseThemeColor('#007a66ff', fallback)).toEqual({ r: 0, g: 122 / 255, b: 102 / 255 });
	});

	it('accepts rgb values and safely falls back for unresolved tokens', () => {
		expect(parseThemeColor('rgb(255, 84, 153)', fallback)).toEqual({
			r: 1,
			g: 84 / 255,
			b: 153 / 255
		});
		expect(parseThemeColor('var(--missing)', fallback)).toEqual(fallback);
		expect(parseThemeColor('', fallback)).toEqual(fallback);
	});
});

describe('luminousThemeColor', () => {
	it('raises a dark theme color without washing out its channel proportions', () => {
		const glow = luminousThemeColor({ r: 160 / 255, g: 64 / 255, b: 32 / 255 }, 0.94);

		expect(glow.r).toBeCloseTo(0.94);
		expect(glow.g / glow.r).toBeCloseTo(64 / 160);
		expect(glow.b / glow.r).toBeCloseTo(32 / 160);
	});

	it('keeps an already luminous color unchanged and gives black a neutral glow', () => {
		const bright = { r: 1, g: 0.6, b: 0.2 };
		expect(luminousThemeColor(bright, 0.9)).toEqual(bright);
		expect(luminousThemeColor({ r: 0, g: 0, b: 0 }, 0.9)).toEqual({
			r: 0.9,
			g: 0.9,
			b: 0.9
		});
	});
});

describe('themeThreadPalette', () => {
	it('uses the selected theme pair and derives contrast cores from its primary', () => {
		const palette = themeThreadPalette('#00d4aa', '#9d4edd');

		expect(palette.primary).toEqual({ r: 0, g: 212 / 255, b: 170 / 255 });
		expect(palette.secondary).toEqual({ r: 157 / 255, g: 78 / 255, b: 221 / 255 });
		expect(Math.max(palette.glowPrimary.r, palette.glowPrimary.g, palette.glowPrimary.b)).toBeCloseTo(
			1
		);
		expect(
			Math.max(palette.glowSecondary.r, palette.glowSecondary.g, palette.glowSecondary.b)
		).toBeCloseTo(0.96);
		expect(palette.ink.r).toBe(0);
		expect(palette.ink.g).toBeLessThan(palette.primary.g);
		expect(palette.light.r).toBeGreaterThan(palette.primary.r);
		expect(palette.light.g).toBeGreaterThan(palette.primary.g);
	});

});
