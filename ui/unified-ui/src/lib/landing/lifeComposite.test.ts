// The homography is the one piece of the montage that is pure math, and it is
// also the piece whose failure is hardest to SEE — a device placed by a subtly
// wrong matrix looks like a device that is merely badly art-directed. So it is
// tested against cases whose answers are known independently of the solver.
import { describe, expect, it } from 'vitest';

import {
	gradeColour,
	gradeStrength,
	quadBounds,
	quadCentre,
	quadToMatrix3d,
	type Quad
} from './lifeComposite';

/** Pull the 16 numbers back out of a `matrix3d(...)` string. */
function nums(m: string): number[] {
	const inner = m.slice(m.indexOf('(') + 1, m.lastIndexOf(')'));
	return inner.split(',').map((s) => Number(s.trim()));
}

/** Apply a CSS matrix3d to a point the way the compositor would. */
function apply(m: string, x: number, y: number): [number, number] {
	const v = nums(m);
	// Column-major: column 0 is v[0..3], column 1 v[4..7], column 3 v[12..15].
	const X = v[0] * x + v[4] * y + v[12];
	const Y = v[1] * x + v[5] * y + v[13];
	const W = v[3] * x + v[7] * y + v[15];
	return [X / W, Y / W];
}

describe('quadToMatrix3d', () => {
	it('is the identity when the quad IS the rectangle', () => {
		const q: Quad = [
			[0, 0],
			[100, 0],
			[100, 50],
			[0, 50]
		];
		const m = quadToMatrix3d(100, 50, q);
		for (const [x, y] of [
			[0, 0],
			[100, 0],
			[100, 50],
			[0, 50],
			[37, 21]
		]) {
			const [X, Y] = apply(m, x, y);
			expect(X).toBeCloseTo(x, 6);
			expect(Y).toBeCloseTo(y, 6);
		}
	});

	it('lands all four corners exactly, under real perspective', () => {
		// A far edge SHORTER than the near one — the case a rotate/scale
		// composition cannot express, and the reason this is projective.
		const q: Quad = [
			[220, 140],
			[430, 176],
			[452, 320],
			[196, 300]
		];
		const m = quadToMatrix3d(640, 400, q);
		const src = [
			[0, 0],
			[640, 0],
			[640, 400],
			[0, 400]
		];
		src.forEach(([x, y], i) => {
			const [X, Y] = apply(m, x, y);
			expect(X).toBeCloseTo(q[i][0], 4);
			expect(Y).toBeCloseTo(q[i][1], 4);
		});
	});

	it('keeps straight lines straight — a projective map has no curvature', () => {
		const q: Quad = [
			[10, 20],
			[300, 60],
			[280, 240],
			[30, 200]
		];
		const m = quadToMatrix3d(200, 100, q);
		// Three collinear source points must stay collinear once mapped.
		const a = apply(m, 0, 50);
		const b = apply(m, 100, 50);
		const c = apply(m, 200, 50);
		const cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
		expect(Math.abs(cross)).toBeLessThan(1e-6);
	});

	it('refuses a degenerate quad rather than emitting NaNs', () => {
		const collapsed: Quad = [
			[0, 0],
			[0, 0],
			[0, 0],
			[0, 0]
		];
		expect(quadToMatrix3d(100, 100, collapsed)).toBe('none');
	});
});

describe('quad geometry', () => {
	it('bounds the quad, not the source rect', () => {
		const q: Quad = [
			[20, 10],
			[120, 30],
			[110, 90],
			[10, 70]
		];
		expect(quadBounds(q)).toEqual({ x: 10, y: 10, w: 110, h: 80 });
	});

	it('centres on the average corner — where the glow spills from', () => {
		const q: Quad = [
			[0, 0],
			[100, 0],
			[100, 100],
			[0, 100]
		];
		expect(quadCentre(q)).toEqual([50, 50]);
	});
});

describe('white balance', () => {
	it('leaves a neutral room alone', () => {
		// Grey light: the grade is white (multiply by 1) and asks for no
		// strength, so a neutral scene pays nothing for the machinery.
		expect(gradeColour([180, 180, 180])).toBe('rgb(255, 255, 255)');
		expect(gradeStrength([180, 180, 180])).toBe(0);
	});

	it('pulls blue down in a golden room and leaves red untouched', () => {
		const warm: [number, number, number] = [232, 198, 150];
		expect(gradeColour(warm)).toBe('rgb(255, 218, 165)');
		// The COLOUR carries the cast at full accuracy; the STRENGTH is what
		// is deliberately held back, so this only asks that a warm room grades
		// at all. The ceiling is asserted below.
		expect(gradeStrength(warm)).toBeGreaterThan(0);
	});

	it('barely grades at all — the screen is a source, not a surface', () => {
		// A display is tinted by the camera's white balance, not lit by the
		// room, so the grade is a hint that the two things were photographed
		// together rather than a match. The owner's reference holds a neutral,
		// bright screen against full daylight and reads entirely correctly.
		// At the first ceiling (0.62) our UI looked like a photograph OF a
		// screen instead of a screen.
		expect(gradeStrength([255, 40, 0])).toBeLessThanOrEqual(0.22);
		expect(gradeStrength([0, 0, 0])).toBeLessThanOrEqual(0.22);
		expect(gradeStrength([232, 198, 150])).toBeLessThanOrEqual(0.22);
	});
});
