// The scrubber's pure math — the part of the movie that can be wrong
// quietly. A boundary off by one shows a scene a frame early on some
// viewport somewhere; these tests are where that is a red line instead.
import { describe, expect, it } from 'vitest';

import {
	boundsFromWeights,
	clamp,
	resolveScene,
	trackProgress,
	viewportProgress
} from './scrub';

describe('trackProgress', () => {
	it('is 0 with the track top at the viewport top and 1 at full travel', () => {
		expect(trackProgress(0, 1800, 800)).toBe(0);
		expect(trackProgress(-1000, 1800, 800)).toBe(1);
	});

	it('clamps beyond both ends rather than extrapolating', () => {
		expect(trackProgress(200, 1800, 800)).toBe(0);
		expect(trackProgress(-5000, 1800, 800)).toBe(1);
	});

	it('degrades a track shorter than the viewport to 0, never NaN', () => {
		expect(trackProgress(-10, 500, 800)).toBe(0);
		expect(trackProgress(-10, 800, 800)).toBe(0);
	});
});

describe('viewportProgress', () => {
	it('scrubs a compact section from viewport entry through viewport exit', () => {
		expect(viewportProgress(800, 400, 800)).toBe(0);
		expect(viewportProgress(200, 400, 800)).toBe(0.5);
		expect(viewportProgress(-400, 400, 800)).toBe(1);
	});

	it('clamps before entry and after exit', () => {
		expect(viewportProgress(1200, 400, 800)).toBe(0);
		expect(viewportProgress(-900, 400, 800)).toBe(1);
	});

	it('degrades missing layout geometry to zero progress', () => {
		expect(viewportProgress(0, 0, 800)).toBe(0);
		expect(viewportProgress(0, 400, 0)).toBe(0);
	});
});

describe('boundsFromWeights', () => {
	it('starts at 0, ends at exactly 1, and is strictly monotonic', () => {
		const bounds = boundsFromWeights([2, 1.5, 2, 1.5, 1, 1.5, 1.5, 1, 2, 1, 1.5, 1.5]);
		expect(bounds[0]).toBe(0);
		expect(bounds[bounds.length - 1]).toBe(1);
		for (let i = 1; i < bounds.length; i++) {
			expect(bounds[i]).toBeGreaterThan(bounds[i - 1]);
		}
	});

	it('produces one more bound than weights', () => {
		expect(boundsFromWeights([1, 1, 1])).toHaveLength(4);
	});
});

describe('resolveScene', () => {
	const bounds = boundsFromWeights([1, 1, 2]);

	it('assigns a boundary to the scene it OPENS', () => {
		expect(resolveScene(bounds, 0)).toEqual({ scene: 0, local: 0 });
		expect(resolveScene(bounds, bounds[1]).scene).toBe(1);
		expect(resolveScene(bounds, bounds[1]).local).toBe(0);
	});

	it('keeps p = 1 inside the last scene at local 1 rather than indexing past it', () => {
		expect(resolveScene(bounds, 1)).toEqual({ scene: 2, local: 1 });
	});

	it('reports local as within-scene progress', () => {
		const midFirst = resolveScene(bounds, bounds[1] / 2);
		expect(midFirst.scene).toBe(0);
		expect(midFirst.local).toBeCloseTo(0.5, 10);
		const midLast = resolveScene(bounds, (bounds[2] + 1) / 2);
		expect(midLast.scene).toBe(2);
		expect(midLast.local).toBeCloseTo(0.5, 10);
	});

	it('is monotonic: scene never decreases as p increases', () => {
		let lastScene = -1;
		for (let p = 0; p <= 1.0001; p += 0.001) {
			const { scene } = resolveScene(bounds, Math.min(1, p));
			expect(scene).toBeGreaterThanOrEqual(lastScene);
			lastScene = scene;
		}
	});

	it('clamps out-of-range progress instead of throwing', () => {
		expect(resolveScene(bounds, -0.5)).toEqual({ scene: 0, local: 0 });
		expect(resolveScene(bounds, 1.5)).toEqual({ scene: 2, local: 1 });
	});

	it('degrades empty bounds to one scene whose local is p', () => {
		expect(resolveScene([], 0.4)).toEqual({ scene: 0, local: 0.4 });
	});
});

describe('clamp', () => {
	it('holds both ends', () => {
		expect(clamp(-1, 0, 1)).toBe(0);
		expect(clamp(2, 0, 1)).toBe(1);
		expect(clamp(0.5, 0, 1)).toBe(0.5);
	});
});
