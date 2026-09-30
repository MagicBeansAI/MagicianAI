import { describe, expect, it } from 'vitest';

import {
	MOTIF_HANDOFF,
	greetingMotifPoint,
	motifBodyAlpha,
	motifHeadRadius,
	motifStrokeRecipe,
	motifTrailLengthPx,
	motifWaveOffset,
	sampleMotifTrail
} from './motifFlow';

describe('landing motif cross-section flow', () => {
	it('hands the movie bottom edge to the greeting top edge with one tangent', () => {
		expect(MOTIF_HANDOFF.movieExit.x).toBe(MOTIF_HANDOFF.greetingEntry.x);
		expect(MOTIF_HANDOFF.movieExit.y).toBe(1);
		expect(MOTIF_HANDOFF.greetingEntry.y).toBe(0);

		const movieTangent = {
			x: MOTIF_HANDOFF.movieExit.x - MOTIF_HANDOFF.moviePenultimate.x,
			y: MOTIF_HANDOFF.movieExit.y - MOTIF_HANDOFF.moviePenultimate.y
		};
		const greetingTangent = {
			x: MOTIF_HANDOFF.greetingControl.x - MOTIF_HANDOFF.greetingEntry.x,
			y: MOTIF_HANDOFF.greetingControl.y - MOTIF_HANDOFF.greetingEntry.y
		};
		expect(greetingTangent.x).toBeCloseTo(movieTangent.x, 8);
		expect(greetingTangent.y).toBeCloseTo(movieTangent.y, 8);
	});

	it('starts Greeting at that exact boundary and exits through bottom-centre', () => {
		expect(greetingMotifPoint(0, 1000, 500)).toEqual({ x: 500, y: 0 });
		expect(greetingMotifPoint(1, 1000, 500)).toEqual({ x: 500, y: 650 });
	});

	it('preserves the physical entry tangent when Greeting is shorter than the viewport', () => {
		const start = greetingMotifPoint(0, 1280, 320, 577);
		const next = greetingMotifPoint(0.0001, 1280, 320, 577);
		const greetingSlope = (next.y - start.y) / (next.x - start.x);
		const movieSlope = (0.18 * 577) / (-0.35 * 1280);
		expect(greetingSlope).toBeCloseTo(movieSlope, 3);
	});

	it('keeps the shared body and wave bounded and anchored', () => {
		expect(motifTrailLengthPx(320)).toBe(210);
		expect(motifTrailLengthPx(1200)).toBe(288);
		expect(motifTrailLengthPx(2400)).toBe(360);
		expect(motifBodyAlpha(0)).toBeCloseTo(0.18);
		expect(motifBodyAlpha(1)).toBeCloseTo(1);
		expect(motifWaveOffset(0, 100, 1)).toBeCloseTo(0);
		expect(motifWaveOffset(1, 100, 1)).toBeCloseTo(0);
		expect(motifHeadRadius(1)).toBe(4);
		expect(motifStrokeRecipe(true, 1).filament).toEqual({
			width: 2.1,
			alpha: 0.82,
			tint: 0.12
		});
	});

	it('samples the same physical body independent of a section timeline', () => {
		const trail = sampleMotifTrail(0, 1, 240, (progress) => ({ x: progress * 1000, y: 0 }));
		expect(trail).toHaveLength(65);
		expect(trail.at(-1)!.x - trail[0].x).toBeCloseTo(240, 0);
		expect(trail[0].progress).toBeCloseTo(0.76, 2);
		expect(trail.at(-1)!.progress).toBe(1);
	});
});
