import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { HOW_CLAIMS, claimMotion } from './howTrack';

const howTrackSource = readFileSync(join(process.cwd(), 'src/lib/landing/HowTrack.svelte'), 'utf8');
const howShotSource = readFileSync(join(process.cwd(), 'src/lib/landing/HowShot.svelte'), 'utf8');
const fieldSource = readFileSync(join(process.cwd(), 'src/lib/landing/LandingField.svelte'), 'utf8');

describe('how track', () => {
	it('states the how in five claims, ending on crew', () => {
		expect(HOW_CLAIMS).toHaveLength(5);
		expect(HOW_CLAIMS.map((c) => c.id)).toEqual([
			'superapp',
			'local',
			'security',
			'bounds',
			'crew'
		]);
		expect(HOW_CLAIMS[0].line).toBe(
			'Agents that can get work done on your Browser, Mac Apps and Mobile Apps'
		);
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

	it('scales the product chrome from its desktop size rather than reflowing it narrow', () => {
		expect(howShotSource).toContain('class="shot-scale"');
		expect(howShotSource).toContain('100cqi');
		expect(howShotSource).toContain('100cqb');
		expect(howShotSource).toContain('--shot-w: 30rem');
		expect(howShotSource).toContain('(max-height: 560px)');
		expect(howShotSource).not.toContain('22.5rem');
	});

	it('stacks copy above the screen on a phone and shortens the swipe', () => {
		expect(howTrackSource).toContain('--ht-slide: 10%');
		expect(howTrackSource).toContain("'copy'");
		expect(howTrackSource).toContain("'media'");
		expect(howTrackSource).toContain('var(--ht-slide)');
	});
});
