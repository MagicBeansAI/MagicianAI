import { describe, expect, it } from 'vitest';
import { CYCLE } from './lifeCycle';
import { buildLifePortraitLottie } from './lifePortrait';

describe('life portrait lottie', () => {
	const doc = buildLifePortraitLottie();

	it('is a tall vector portrait, not a 16:9 plate', () => {
		expect(doc.w).toBe(300);
		expect(doc.h).toBe(400);
		expect(JSON.stringify(doc).length).toBeLessThan(80_000);
	});

	it('has still silhouette and frame plus one layer per cycle entry', () => {
		const names = (doc.layers as { nm?: string }[]).map((l) => l.nm);
		expect(names).toContain('silhouette');
		expect(names).toContain('frame');
		for (const label of CYCLE) {
			expect(names).toContain(label);
		}
		expect(doc.layers.length).toBeGreaterThanOrEqual(2 + CYCLE.length);
	});

	it('has no raster assets', () => {
		const assets = (doc.assets ?? []) as { p?: string; u?: string }[];
		expect(assets.every((a) => !a.p && !a.u)).toBe(true);
	});
});
