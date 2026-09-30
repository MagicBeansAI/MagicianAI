// The fork after the film: three roads, none taken until the visitor says so.
//
// The film ends, the trust reveal lands, the creator speaks, the ask is
// offered — and only THEN does the page split. This module holds the part of
// that split which can be wrong quietly: which roads exist, and how a keyboard
// moves between them. The rendering lives in PathFork.svelte.
//
// Default is deliberately NOTHING chosen. A visitor who does not opt in never
// pays for the extra page: no panel is in the DOM until a tab is picked.
//
// The third road arrived on 2026-08-05 with the film's restructure. The
// day-in-the-life used to be the main scroll's second half; the main scroll is
// the ARGUMENT now, and the day is the PROOF you opt into — which is exactly
// what this section already was for the other two.

import { writable } from 'svelte/store';

/** The three roads. Order here is the order on screen and under the arrow keys. */
export const PATHS = [
	{
		id: 'promise',
		/** The mono index stamped on the card — the film's signage register. */
		mark: 'A',
		/** What the road is, in the fork's card. */
		label: 'The three promises',
		/** The road's three acts, drawn on the fork card as a small rail. */
		acts: ['Knows you', 'Acts for you', 'Belongs to you'],
		/**
		 * The same three as one line, for the accessible name and the sticky
		 * switcher. Written out rather than joined so the string is greppable;
		 * `pathFork.test.ts` fails if it ever drifts from `acts`.
		 */
		spine: 'Knows you · Acts for you · Belongs to you',
		blurb:
			'The three cards above, taken apart. What the memory actually stores, what the hands actually touch, and what the model is never allowed to see.',
		/** How the road signs off — and hands the visitor back to the ask. */
		close: {
			line: 'That is what it is.',
			sub: 'Three promises, nine mechanisms, no asterisks. The only thing left is to give it something.'
		}
	},
	{
		id: 'lifecycle',
		mark: 'B',
		label: 'One job, end to end',
		acts: ['Started', 'Moving', 'Closed'],
		spine: 'Started · Moving · Closed',
		blurb:
			'One piece of work from the second you hand it over to the receipt at the end — including the nights you were not watching, and the moment it stopped to ask.',
		close: {
			line: 'That is one job, closed.',
			sub: 'Handed over, worked through the night, questioned once, finished and accounted for. Now start one of your own.'
		}
	}

] as const;

export type PathId = (typeof PATHS)[number]['id'];

/** Null is a real answer: the fork sits closed until someone opens it. */
export type PathChoice = PathId | null;

/**
 * A road someone asked for from ELSEWHERE on the page.
 *
 * The fork sits below the ask, and a visitor who has just watched the film
 * has no reason to believe there is a day-in-the-life under it — the day used
 * to BE the film, and after the restructure it became a road nobody could see
 * from where they were standing. So the film and the ask both carry a link to
 * it, and this store is how they reach in: write an id, PathFork opens that
 * road and scrolls to it. Cleared back to null once honoured, so the same
 * link works twice.
 */
export const requestedPath = writable<PathChoice>(null);

/** Ask the fork to open a road. Safe from anywhere on the page. */
export function requestPath(id: PathId): void {
	requestedPath.set(id);
}

export function pathIndex(id: PathChoice): number {
	if (id === null) return -1;
	return PATHS.findIndex((p) => p.id === id);
}

export function pathById(id: PathChoice): (typeof PATHS)[number] | null {
	const n = pathIndex(id);
	return n < 0 ? null : PATHS[n];
}

/**
 * The NEXT road round — what the sticky head and the closing frame offer.
 *
 * With two roads this was "the road not taken" and the name said so. With
 * three it is a rotation, and the guarantee that survives is the one that
 * mattered: it is never the road you are on, so the offer is never a no-op.
 */
export function otherPath(id: PathId): (typeof PATHS)[number] {
	return PATHS[(pathIndex(id) + 1) % PATHS.length];
}

/**
 * Where a tablist keypress moves focus, or -1 for "not a navigation key".
 *
 * The APG roving-tabindex pattern: Left/Right wrap, Home/End jump to the
 * ends. Activation stays on Enter/Space, which native <button> already does —
 * so arrowing across the fork previews nothing and commits nothing, which is
 * what we want when committing costs the visitor a page of scrolling.
 */
export function arrowTarget(from: number, key: string, count: number): number {
	if (count <= 0) return -1;
	const at = from < 0 ? 0 : from % count;
	switch (key) {
		case 'ArrowRight':
		case 'ArrowDown':
			return (at + 1) % count;
		case 'ArrowLeft':
		case 'ArrowUp':
			return (at - 1 + count) % count;
		case 'Home':
			return 0;
		case 'End':
			return count - 1;
		default:
			return -1;
	}
}
