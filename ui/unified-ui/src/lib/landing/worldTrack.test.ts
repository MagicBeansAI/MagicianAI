import { describe, expect, it } from 'vitest';

import { sampleMotifTrail } from './motifFlow';
import { SI, STATIONS, buildThread, pAt, sampleThread, type Vec } from './worldTrack';

const POSITIONS: Vec[] = [
	{ x: 720, y: 450 },
	{ x: 720, y: 450 },
	{ x: 720, y: 680 }
];

describe('landing film motif path', () => {
	it('cuts the machine-era acts, leaving drown, tear and life', () => {
		// born (1977) and operate (1995) are gone — the film opens on drown.
		expect(STATIONS.map((s) => s.id)).toEqual(['drown', 'tear', 'life']);
		expect(SI.drown).toBe(0);
		expect(SI.tear).toBe(1);
		expect(SI.life).toBe(2);
		expect(STATIONS.reduce((sum, s) => sum + s.weight, 0)).toBeCloseTo(36.0, 5);
	});
	it('ghosts the hero motif into the drowning before blooming at the inversion', () => {
		const path = buildThread(POSITIONS, 1440, 900, 1, 1);

		// motifIn and pile are the same gesture now that born/operate no
		// longer carry a broad S in front of the drowning.
		expect(path.marks.motifIn).toBe(pAt(SI.drown, 0.03));
		expect(path.marks.motifIn).toBe(path.marks.pile);
		expect(path.marks.pile).toBeLessThan(path.marks.ignite);
		expect(path.marks.ignite).toBe(pAt(SI.tear, 0.32));
	});

	it('crosses the montage in one direction instead of orbiting it', () => {
		const path = buildThread(POSITIONS, 1440, 900, 1, 1);
		const lifeSamples = [0.05, 0.22, 0.42, 0.64, 0.84, 1].map((local) =>
			sampleThread(path.main, pAt(SI.life, local))
		);

		expect(path.marks.loopStart).toBeUndefined();
		expect(path.marks.loopEnd).toBeUndefined();
		expect(path.behind).toEqual([]);
		// The proof itself crosses once left→right. Its final control then returns
		// to bottom-centre solely to hand the same ribbon into Greeting.
		for (let i = 1; i < lifeSamples.length - 1; i++) {
			expect(lifeSamples[i].x).toBeGreaterThan(lifeSamples[i - 1].x);
		}
		expect(lifeSamples.at(-1)).toMatchObject({ x: 720, y: 1130 });
	});

	it('settles below the montage without crossing the closing headline', () => {
		const path = buildThread(POSITIONS, 1440, 900, 1, 1);
		const channel = [0.3, 0.42, 0.64, 0.84].map((local) =>
			sampleThread(path.main, pAt(SI.life, local))
		);

		for (const point of channel) {
			const offsetFromStation = point.y - POSITIONS[SI.life].y;
			expect(offsetFromStation).toBeGreaterThanOrEqual(0.09 * 900);
			expect(offsetFromStation).toBeLessThanOrEqual(0.25 * 900);
		}
		// After clearing the composition, it turns down to the shared canvas seam.
		expect(sampleThread(path.main, pAt(SI.life, 1))).toMatchObject({ x: 720, y: 1130 });
	});

	it('keeps the extended spline finite and strictly ordered', () => {
		const path = buildThread(POSITIONS, 1440, 900, 1, 1);

		for (let i = 0; i < path.main.length; i++) {
			expect(Number.isFinite(path.main[i].p)).toBe(true);
			expect(Number.isFinite(path.main[i].x)).toBe(true);
			expect(Number.isFinite(path.main[i].y)).toBe(true);
			if (i > 0) expect(path.main[i].p).toBeGreaterThan(path.main[i - 1].p);
		}

		for (const progress of [path.marks.motifIn, path.marks.pile, path.marks.ignite, 1]) {
			const point = sampleThread(path.main, progress);
			expect(Number.isFinite(point.x)).toBe(true);
			expect(Number.isFinite(point.y)).toBe(true);
		}
	});

	it('keeps the visible motif body a stable physical length across unequal chapter weights', () => {
		const trail = sampleMotifTrail(0, 1, 240, (p) => ({ x: p * 1000, y: 0 }));
		expect(trail).toHaveLength(65);
		expect(trail[trail.length - 1].x - trail[0].x).toBeCloseTo(240, 0);
		expect(trail[0].progress).toBeCloseTo(0.76, 2);
		expect(trail[trail.length - 1].progress).toBe(1);
	});

	it('returns finite anchored samples when the available path is shorter than the target body', () => {
		const trail = sampleMotifTrail(0.4, 0.5, 400, (p) => ({ x: p * 100, y: p * 50 }));
		expect(trail).toHaveLength(65);
		expect(trail[0].progress).toBeCloseTo(0.4);
		expect(trail[trail.length - 1].progress).toBeCloseTo(0.5);
		for (const point of trail) {
			expect(Number.isFinite(point.x)).toBe(true);
			expect(Number.isFinite(point.y)).toBe(true);
		}
	});
});
