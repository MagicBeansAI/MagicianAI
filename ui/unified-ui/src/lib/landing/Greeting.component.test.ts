// The freedom payoff's contract, and it is mostly about what a visitor is
// TOLD rather than what they are shown.
//
// The beat is a SCROLL-SCRUBBED swap: "Love what you do." holds, the thread
// crosses, and the first and last words trade places to leave "Do what you
// love." Two things about that can be wrong quietly:
//
//   1. THE FOUR WORDS. The whole argument is that both phrases use the same
//      ones — "Love what you do" is what you tell someone who cannot change
//      their situation, "Do what you love" is what someone says who can, and
//      Magican is the thing that reorders them. A fifth word, a subtitle or an
//      explanatory line destroys it, so this suite pins the vocabulary.
//   2. THE ACCESSIBLE SENTENCE. A screen reader cannot perceive a swap and
//      must never be handed a phrase that is about to be contradicted. There
//      is exactly one sentence in the tree, it is the RESOLVED one, and
//      everything that animates is hidden from the tree entirely.
//
// Reduced motion gets the resolved line directly: no hold, no swap, no
// thread, and no canvas — and no track height either, since there is nothing
// to scrub through.
//
// jsdom has no scroll geometry, so the scrub always reports progress 0 here.
// That is the right thing to pin anyway: the beat must begin unresolved even
// now that it arrives after the product proof rather than opening the page.
import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import Greeting from './Greeting.svelte';
import { motionEnabled } from '$lib/motion';

afterEach(() => {
	cleanup();
	motionEnabled.set(true);
});

const WORDS = ['Love', 'love.', 'what', 'you', 'do.', 'Do'];
const greetingSource = readFileSync(join(process.cwd(), 'src/lib/landing/Greeting.svelte'), 'utf8');

describe('Greeting', () => {
	it('names the resolved sentence once, and hides everything that moves', () => {
		const { container } = render(Greeting);

		// One heading, and it is the sentence the beat ENDS on.
		const heading = screen.getByRole('heading', { level: 2 });
		expect(heading.textContent).toBe('Do what you love.');
		expect(screen.getByRole('region', { name: 'Do what you love' })).toBeTruthy();

		// The animated line is not in the tree at all: it spends most of its
		// life saying the opposite of the heading.
		const line = container.querySelector('.gr-line');
		expect(line?.getAttribute('aria-hidden')).toBe('true');
		expect(container.querySelector('.gr-canvas')?.getAttribute('aria-hidden')).toBe('true');
		expect(container.querySelector('.gr-cue')).toBeNull();
	});

	it('uses the same four words in both orders, and adds none', () => {
		const { container } = render(Greeting);
		const line = container.querySelector('.gr-line');
		const words = [...(line?.querySelectorAll('.gr-w, .gr-fixed') ?? [])].map(
			(el) => el.textContent ?? ''
		);
		expect(words.sort()).toEqual([...WORDS].sort());
	});

	it('pairs each cell with the two words that stand in ITS position', () => {
		const { container } = render(Greeting);
		// Cell one hosts "Love" and then "Do"; cell four hosts "do." and then
		// "love." Pairing them the other way (cell one holding Love → love.)
		// moves the CELLS instead of the words, and the finished line then
		// has a hole on one side and touching words on the other, because
		// the two cells are different widths.
		const first = container.querySelector('.gr-slot-1');
		const last = container.querySelector('.gr-slot-4');
		expect(first?.querySelector('.gr-go')?.textContent).toBe('Love');
		expect(first?.querySelector('.gr-come')?.textContent).toBe('Do');
		expect(last?.querySelector('.gr-go')?.textContent).toBe('do.');
		expect(last?.querySelector('.gr-come')?.textContent).toBe('love.');
	});

	it('starts on the unresolved phrase, so there is something to resolve', () => {
		const { container } = render(Greeting);
		expect(container.querySelector('.gr-line')?.getAttribute('data-phase')).toBe('in');
	});

	it('continues MovieTrack motif geometry instead of restarting a second comet', () => {
		expect(greetingSource).toContain('return greetingMotifPoint(u, rt.w, rt.h, rt.vh)');
		expect(greetingSource).toContain('motifTrailLengthPx(rt.w)');
		expect(greetingSource).toContain('motifWaveOffset(g, rt.t, 1)');
		expect(greetingSource).toContain('motifHeadRadius(1)');
		expect(greetingSource).toContain('motifStrokeRecipe(rt.onLight, 1)');
		expect(greetingSource).toContain('sampleMotifTrail(0, u, targetLength, pathAt)');
		expect(greetingSource).toContain('observer = new IntersectionObserver');
		expect(greetingSource).not.toContain('const hr = 52');
		expect(greetingSource).not.toContain('pass(glowCompanion, 20');
	});

	it('scrubs while its compact section passes through the viewport', () => {
		const { container } = render(Greeting);
		// The track remains the progress owner, but there is no tall sticky
		// stage or scroll-cue ceremony around this short payoff.
		const track = container.querySelector<HTMLElement>('.gr-track');
		expect(track).toBeTruthy();
		expect(track?.classList.contains('gr-still')).toBe(false);
		// The scrubbed values ride the TRACK, never the section: `measure()`
		// writes --travel and --arc onto the section as inline properties,
		// and Svelte rewrites the whole style attribute of anything it puts a
		// style directive on. Both on one element and every scroll frame
		// wipes the measurements — which is exactly what happened, and the
		// words flew a third of the way to each other.
		const style = track?.getAttribute('style') ?? '';
		expect(style).toContain('--sw:');
		expect(style).not.toContain('--cue:');
		expect(container.querySelector('.gr')?.getAttribute('style') ?? '').not.toContain('--sw');
		expect(container.querySelector('.gr-cue')).toBeNull();
		// At progress zero nothing has happened yet.
		expect(track?.style.getPropertyValue('--sw')).toBe('0.0000');
		expect(track?.style.getPropertyValue('--cue')).toBe('');
	});

	it('gives reduced motion no track to scroll through', () => {
		motionEnabled.set(false);
		const { container } = render(Greeting);
		expect(container.querySelector('.gr-track')?.classList.contains('gr-still')).toBe(true);
		expect(container.querySelector('.gr-track')?.getAttribute('style') ?? '').toBe('');
	});

	it('under reduced motion states the resolved line and nothing that waits', () => {
		motionEnabled.set(false);
		const { container } = render(Greeting);

		// One line, already right, with no cells to swap and no thread.
		const line = container.querySelector('.gr-line');
		expect(line?.textContent?.trim()).toBe('Do what you love.');
		expect(line?.classList.contains('gr-live')).toBe(false);
		expect(container.querySelector('.gr-slot')).toBeNull();
		expect(container.querySelector('canvas')).toBeNull();
		// And no scroll cue timed to an exit that never happens.
		expect(container.querySelector('.gr-cue')).toBeNull();
		// The heading is unchanged: it was always the resolved sentence.
		expect(screen.getByRole('heading', { level: 2 }).textContent).toBe('Do what you love.');
	});
});
