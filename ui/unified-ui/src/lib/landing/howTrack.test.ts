import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { HOW_CLAIMS, claimMotion } from './howTrack';

const howTrackSource = readFileSync(join(process.cwd(), 'src/lib/landing/HowTrack.svelte'), 'utf8');
const howCardSource = readFileSync(join(process.cwd(), 'src/lib/landing/HowCard.svelte'), 'utf8');
const fieldSource = readFileSync(join(process.cwd(), 'src/lib/landing/LandingField.svelte'), 'utf8');

describe('how track', () => {
	it('states seven things only Magican does, ending on ownership', () => {
		expect(HOW_CLAIMS.map((c) => c.id)).toEqual([
			'present',
			'hands',
			'learns',
			'relentless',
			'builds',
			'agents',
			'yours'
		]);
		// each station leads with an outcome and ends on an aside
		for (const claim of HOW_CLAIMS) {
			expect(claim.title.length).toBeGreaterThan(0);
			expect(claim.aside.length).toBeGreaterThan(0);
		}
	});

	it('pins the first station on arrival, then swipes later stations in', () => {
		const first = claimMotion(0, 0, 0);
		expect(first.opacity).toBe(1);
		expect(first.mediaX).toBe(0);
		expect(first.copyX).toBe(0);

		const incoming = claimMotion(1, 1, 0);
		expect(incoming.opacity).toBe(0);
		expect(incoming.mediaX).toBeCloseTo(-1);
		expect(incoming.copyX).toBeCloseTo(1);

		const settled = claimMotion(1, 1, 0.5);
		expect(settled.opacity).toBe(1);
		expect(settled.mediaX).toBe(0);
		expect(settled.copyX).toBe(0);
	});

	it('holds the last station instead of wiping the field empty', () => {
		const last = HOW_CLAIMS.length - 1;
		const end = claimMotion(last, last, 1, last);
		expect(end.opacity).toBe(1);
		expect(end.mediaX).toBe(0);
		expect(end.copyX).toBe(0);
	});

	it('hides stations that are not current', () => {
		expect(claimMotion(2, 0, 0.5).opacity).toBe(0);
	});
});

describe('how-track mobile layout', () => {
	it('covers dusk-town on viewports taller than 16:9 instead of letterboxing', () => {
		expect(fieldSource).toContain('max-aspect-ratio: 16 / 9');
		expect(fieldSource).toContain('object-fit: cover');
		expect(fieldSource).toContain('object-position: center');
	});

	it("keeps each card's 16:10 frame inside the stage on phones, either way up", () => {
		expect(howCardSource).toContain('aspect-ratio: 8 / 5');
		expect(howCardSource).toContain('(max-width: 820px)');
		expect(howCardSource).toContain('(max-height: 560px)');
		expect(howCardSource).toContain('80svh');
	});

	it('stacks copy above the screen on a phone and shortens the swipe', () => {
		expect(howTrackSource).toContain('--ht-slide: 10%');
		expect(howTrackSource).toContain("'copy'");
		expect(howTrackSource).toContain("'media'");
		expect(howTrackSource).toContain('var(--ht-slide)');
	});
});
