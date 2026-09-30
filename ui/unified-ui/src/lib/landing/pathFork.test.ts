// The fork's own logic. Two things here are contracts rather than taste: the
// roads are a closed SET with a spine each, and the keyboard can reach every
// one of them without a mouse — the fork is the only door to a third of this
// page's copy, and since 2026-08-05 it is also the only door to the whole
// day-in-the-life, so an unreachable tab hides all of it.
import { describe, expect, it } from 'vitest';

import { PATHS, arrowTarget, otherPath, pathById, pathIndex } from './pathFork';

describe('PATHS', () => {
	it('offers exactly two roads, marked A and B', () => {
		expect(PATHS).toHaveLength(2);
		expect(PATHS.map((p) => p.mark)).toEqual(['A', 'B']);
	});

	it('does not carry the retired day road', () => {
		expect(PATHS.map((p) => p.id)).toEqual(['promise', 'lifecycle']);
	});

	it('gives every road a spine, a blurb and a sign-off back to the ask', () => {
		for (const path of PATHS) {
			expect(path.spine.length).toBeGreaterThan(0);
			expect(path.blurb.length).toBeGreaterThan(0);
			expect(path.close.line.length).toBeGreaterThan(0);
			expect(path.close.sub.length).toBeGreaterThan(0);
		}
	});

	it('keeps every spine in step with the three acts drawn on its card', () => {
		for (const path of PATHS) {
			expect(path.acts).toHaveLength(3);
			expect(path.acts.join(' · ')).toBe(path.spine);
		}
	});

	it('keeps every spine and label distinct — the fork is a choice, not a list', () => {
		expect(new Set(PATHS.map((p) => p.spine)).size).toBe(PATHS.length);
		expect(new Set(PATHS.map((p) => p.label)).size).toBe(PATHS.length);
		expect(new Set(PATHS.map((p) => p.id)).size).toBe(PATHS.length);
	});
});

describe('pathIndex / pathById', () => {
	it('treats "nothing chosen" as a real answer rather than a missing one', () => {
		expect(pathIndex(null)).toBe(-1);
		expect(pathById(null)).toBeNull();
	});

	it('round-trips a chosen road', () => {
		expect(pathById('promise')?.id).toBe('promise');
		expect(pathById('lifecycle')?.id).toBe('lifecycle');
		expect(pathIndex('lifecycle')).toBe(1);
	});
});

describe('otherPath', () => {
	// With three roads this is a rotation rather than "the other one". The
	// guarantee that has to survive is the one the offer depends on: it is
	// never the road you are already reading.
	it('rotates, and never names the road you are on', () => {
		expect(otherPath('promise').id).toBe('lifecycle');
		expect(otherPath('lifecycle').id).toBe('promise');
		for (const path of PATHS) expect(otherPath(path.id).id).not.toBe(path.id);
	});
});

describe('arrowTarget', () => {
	it('wraps in both directions across all three tabs', () => {
		expect(arrowTarget(0, 'ArrowRight', 3)).toBe(1);
		expect(arrowTarget(1, 'ArrowRight', 3)).toBe(2);
		expect(arrowTarget(2, 'ArrowRight', 3)).toBe(0);
		expect(arrowTarget(0, 'ArrowLeft', 3)).toBe(2);
		expect(arrowTarget(2, 'ArrowLeft', 3)).toBe(1);
	});

	it('treats the vertical arrows the same — the tabs stack on a phone', () => {
		expect(arrowTarget(0, 'ArrowDown', 3)).toBe(1);
		expect(arrowTarget(0, 'ArrowUp', 3)).toBe(2);
	});

	it('jumps to the ends on Home and End', () => {
		expect(arrowTarget(1, 'Home', 3)).toBe(0);
		expect(arrowTarget(0, 'End', 3)).toBe(2);
	});

	it('reports -1 for keys it does not own, so Enter and Tab stay native', () => {
		expect(arrowTarget(0, 'Enter', 3)).toBe(-1);
		expect(arrowTarget(0, ' ', 3)).toBe(-1);
		expect(arrowTarget(0, 'Tab', 3)).toBe(-1);
	});

	it('starts from the first tab when nothing has been chosen yet', () => {
		expect(arrowTarget(-1, 'ArrowRight', 3)).toBe(1);
		expect(arrowTarget(-1, 'ArrowLeft', 3)).toBe(2);
	});

	it('never indexes past an empty list', () => {
		expect(arrowTarget(0, 'ArrowRight', 0)).toBe(-1);
	});
});
