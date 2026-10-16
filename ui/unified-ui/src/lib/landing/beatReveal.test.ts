// A beat's arrival math. The failure this guards against is the quiet one:
// a beat that never reaches local 1 on some viewport height stays half-faded
// forever, and nobody notices until a visitor on a short laptop reads a page
// of ghost text. Every case below pins an end of that range.
import { describe, expect, it } from 'vitest';

import { beatLocal } from './beatReveal';

describe('beatLocal', () => {
	it('is 0 while the beat is still below the fold', () => {
		expect(beatLocal(900, 900)).toBe(0);
		expect(beatLocal(2400, 900)).toBe(0);
	});

	it('reaches exactly 1 once the top edge has risen to the settle line', () => {
		expect(beatLocal(900 * 0.44, 900)).toBe(1);
	});

	it('stays 1 for everything above the settle line, including scrolled past', () => {
		expect(beatLocal(100, 900)).toBe(1);
		expect(beatLocal(0, 900)).toBe(1);
		expect(beatLocal(-4000, 900)).toBe(1);
	});

	it('rises monotonically across the arrival band', () => {
		let last = -1;
		for (let top = 900; top >= 0; top -= 5) {
			const v = beatLocal(top, 900);
			expect(v).toBeGreaterThanOrEqual(last);
			last = v;
		}
		expect(last).toBe(1);
	});

	it('runs the same shape on a short viewport as a tall one', () => {
		// half-way through the band is half-way through the reveal, whatever
		// the phone is — the stagger cannot compress to nothing on a small
		// screen and skip the arrival entirely.
		const band = (vh: number) => vh - vh * 0.44;
		expect(beatLocal(900 - band(900) / 2, 900)).toBeCloseTo(0.5, 10);
		expect(beatLocal(560 - band(560) / 2, 560)).toBeCloseTo(0.5, 10);
	});

	it('renders finished rather than NaN when there is no viewport to measure', () => {
		expect(beatLocal(0, 0)).toBe(1);
		expect(beatLocal(120, -10)).toBe(1);
	});

	it('clamps an absurd settle rather than inverting the band', () => {
		expect(beatLocal(400, 900, 1)).toBe(1);
		expect(beatLocal(400, 900, 4)).toBe(1);
	});
});
