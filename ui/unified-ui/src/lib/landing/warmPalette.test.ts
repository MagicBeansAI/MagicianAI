import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { WARM_PALETTE, NIGHT_PALETTE, BRAND_GOLD, BRAND_GOLD_INK } from './warmPalette';

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

const route = readFileSync(join(process.cwd(), 'src/routes/+page.svelte'), 'utf8');

for (const [name, palette] of [['day', WARM_PALETTE], ['night', NIGHT_PALETTE]] as const) {
	const { ground, text, accent } = palette;
	describe(name + ' landing palette', () => {
		it('keeps all body and supporting text above AA on every ground', () => {
			for (const foreground of [text.primary, text.secondary, text.faint]) {
				for (const background of Object.values(ground)) {
					expect(contrastRatio(foreground, background)).toBeGreaterThanOrEqual(4.5);
				}
			}
		});
		it('keeps links and button labels readable', () => {
			for (const color of Object.values(accent)) {
				expect(contrastRatio(color, ground.base)).toBeGreaterThanOrEqual(4.5);
				expect(contrastRatio(color, ground.elevated)).toBeGreaterThanOrEqual(4.5);
				expect(contrastRatio(text.onAccent, color)).toBeGreaterThanOrEqual(4.5);
			}
		});
		it('matches the scoped CSS declarations', () => {
			const start = route.indexOf(name === 'day' ? '.lp-root {' : ".lp-root[data-appearance='night'] {");
			const block = route.slice(start, route.indexOf('}', start));
			const tokens = {
				'bg-base': ground.base, 'bg-card': ground.elevated, 'bg-soft': ground.soft,
				'landing-bg': ground.base, 'text-primary': text.primary,
				'text-secondary': text.secondary, 'text-faint': text.faint,
				'text-on-accent': text.onAccent, 'accent-primary': accent.primary,
				'accent-secondary': accent.secondary
			};
			for (const [token, value] of Object.entries(tokens)) expect(block).toContain('--' + token + ': ' + value);
		});
	});
}
it('uses dark ink on the signature gold', () => {
	expect(contrastRatio(BRAND_GOLD_INK, BRAND_GOLD)).toBeGreaterThanOrEqual(4.5);
});

it('keeps colored walkthrough labels readable in both appearances', () => {
	for (const [name, palette] of [['day', WARM_PALETTE], ['night', NIGHT_PALETTE]] as const) {
		const start = route.indexOf(name === 'day' ? '.lp-root {' : ".lp-root[data-appearance='night'] {");
		const block = route.slice(start, route.indexOf('}', start));
		for (const token of ['brand-signal-primary', 'brand-signal-secondary']) {
			const color = block.match(new RegExp('--' + token + ': (#[0-9a-f]{6})'))?.[1];
			expect(color).toBeDefined();
			for (const background of Object.values(palette.ground)) {
				expect(contrastRatio(color!, background)).toBeGreaterThanOrEqual(4.5);
			}
		}
	}
});
