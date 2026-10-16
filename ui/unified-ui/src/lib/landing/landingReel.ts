// WHICH prologue film the landing plays.
//
// Act 0 is generated footage (see PrologueReel.svelte), and there is now
// more than one cut of it: the photoreal film that shipped, and a
// miniature-diorama film shot to the same storyboard. They are not
// interchangeable by accident — a film brings its own match-cut geometry,
// its own caption timings, its own station labels and its own tube light —
// so a reel is a DIRECTORY plus the manifest inside it, and everything the
// page keys off the footage is read from that manifest at load. This file
// only answers "which directory".
//
// Choosing one is a VISITOR setting (owner call, 2026-08-04, reversing the
// authoring-only rule this file used to state). Three ways in, in strict
// order of precedence:
//   · `?reel=diorama` / `?reel=photoreal` — the deliberate override. It wins
//     over everything, so a link always shows the film it names and two tabs
//     still compare two cuts.
//   · the remembered choice — whatever the visitor last picked in the page
//     chrome's film selector, held in localStorage.
//   · DEFAULT_REEL below — the one line that decides what a first-time
//     visitor gets.
//
// The selector swaps the film LIVE, with no reload: `selectedReel` is the
// page's single answer to "which film", MovieTrack flows it to PrologueReel
// as `base`, and that component re-cuts to the new directory from scratch.

import { writable } from 'svelte/store';

export interface Reel {
	id: string;
	/** What the film is called in the page chrome. */
	name: string;
	/** Directory holding the frame sequence and its manifest.json. */
	base: string;
	/**
	 * The single frame reduced motion shows as a plain <img>. It cannot come
	 * from the manifest: under reduced motion the reel never mounts and no
	 * manifest is ever fetched — the still IS the whole of Act 0 there.
	 */
	still: string;
	stillAlt: string;
}

export const REELS: Record<string, Reel> = {
	photoreal: {
		id: 'photoreal',
		name: 'Photoreal',
		base: '/prologue/reel/',
		still: 'f390.webp',
		stillAlt:
			'A still from the prologue film: a hominid crouches over the first fire on a dark shore at dusk.'
	},
	diorama: {
		id: 'diorama',
		name: 'Diorama',
		base: '/prologue/reel-diorama/',
		// The same beat of the same storyboard as the photoreal still, but NOT
		// the same frame number: the diorama takes longer getting there, and
		// its flame is not lit until ~f415. f424 is where it stands up.
		still: 'f424.webp',
		stillAlt:
			'A still from the prologue film: the first fire, built and lit as a miniature tabletop diorama.'
	}
};

/**
 * THE SHIPPED DEFAULT — what a visitor who has never chosen, and who
 * followed no `?reel=` link, gets on arrival.
 */
export const DEFAULT_REEL = 'photoreal';

/** Where the visitor's chosen film is remembered between visits. */
const REEL_STORAGE_KEY = 'magican-landing-reel';

/**
 * THE PRECEDENCE, in one place. `?reel=<id>` is the deliberate override and
 * wins outright; then the remembered choice; then the shipped default. An id
 * naming no reel we have is ignored at every level, so neither a stale
 * localStorage value nor a visitor typing in the address bar can ask for a
 * film that does not exist.
 */
export function resolveReel(search: string, stored?: string | null): Reel {
	const want = new URLSearchParams(search).get('reel');
	return (want ? REELS[want] : null) ?? (stored ? REELS[stored] : null) ?? REELS[DEFAULT_REEL];
}

/**
 * The film the page is playing. Everything that shows footage reads this —
 * MovieTrack flows it to PrologueReel as `base`, and the reduced-motion
 * still comes out of the same record — so a write here swaps the film
 * everywhere at once.
 *
 * It starts on the shipped default rather than on the resolved answer: the
 * landing page is server-rendered, and resolution needs a URL and a
 * localStorage that only exist on the client. `initReelChoice()` settles it
 * on mount, which is also what keeps the server's markup and the first
 * client render identical.
 */
export const selectedReel = writable<Reel>(REELS[DEFAULT_REEL]);

let settled = false;

/** Settle the choice from URL + storage. Idempotent; client-only. */
export function initReelChoice(): void {
	if (settled || typeof window === 'undefined') return;
	settled = true;
	let stored: string | null = null;
	try {
		stored = window.localStorage.getItem(REEL_STORAGE_KEY);
	} catch {
		/* localStorage unavailable — the URL and the default still answer */
	}
	selectedReel.set(resolveReel(window.location.search, stored));
}

/** The visitor picked a film: play it, and remember it for next time. */
export function chooseReel(reel: Reel): void {
	selectedReel.set(reel);
	try {
		window.localStorage.setItem(REEL_STORAGE_KEY, reel.id);
	} catch {
		/* localStorage unavailable — the choice holds for this visit only */
	}
}

/** One whisper caption's frame window, in THIS reel's own frame numbers:
 *  fade up across a→b, hold, fade out across c→d. */
export interface ReelCaption {
	text: string;
	a: number;
	b: number;
	c: number;
	d: number;
}
