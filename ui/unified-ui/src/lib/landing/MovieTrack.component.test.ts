// The movie under reduced motion: the house rule says the track collapses
// and EVERY station renders stacked as a readable document. This is the one
// rendering the suite can honestly assert (jsdom has no scroll geometry,
// no sticky, no canvas) — and it is also the rendering that matters most,
// because it is the accessibility contract.
//
// 2026-08-16, THE MACHINE-ERA CUT. `born` (1977) and `operate` (1995) — the
// history-of-computing arc — are gone: the owner's call was that "the whole
// orchestra of the machines is not useful anymore". Both motion modes now
// begin directly on the drowning; the CRT terminal session, its physical
// counter-scaled font, and the six-window 1995 carousel are all dead code
// and this suite pins their absence rather than their content.
//
// 2026-08-11: the generated photoreal and diorama prologues remain archived
// in static/prologue, but the root film no longer mounts either one.
//
// 2026-08-05, THE SECOND CUT. The film used to carry two grievances — a
// LABOUR thesis (tools history, 1977, 1995) and then, mid-film, an
// OWNERSHIP one ("your data became their assets") — so the reveal answered
// the first while the payoff line answered the second. This suite pins the
// single-grievance cut:
//
//   · THREE stations. The four generated-prologue stations, the redundant
//     “still doing it all” recap, the two machine-era acts, and the
//     day-in-the-life (7:30 → 22:30) are outside the root film.
//   · The drowning, where WORK multiplies and nothing is stolen. The data
//     argument is deleted from the spine, not relocated: the dossier, the
//     advertisers, the consent sheet and their copy must all be gone.
//   · A reveal whose payoff is TOOL → STAFF. "Yours again" answered a
//     grievance the film no longer makes and must not survive anywhere.
//   · A closing montage station whose words stand alone while its footage
//     has not landed.
//
// The freedom payoff ("Love what you do." → "Do what you love.") is NOT in
// here. It is a scrubbed section after the track; Greeting.component.test.ts
// owns it.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import MovieTrack from './MovieTrack.svelte';
import { motionEnabled } from '$lib/motion';

const movieSource = readFileSync(join(process.cwd(), 'src/lib/landing/MovieTrack.svelte'), 'utf8');

afterEach(() => {
	cleanup();
	motionEnabled.set(true);
});

describe('MovieTrack under reduced motion', () => {
	it('drops the 1977 terminal and its counter-scaled physical font entirely', () => {
		// born is cut: the CRT terminal session, its dedicated custom property
		// and every rule that read it must all be gone, not merely unreachable.
		expect(movieSource).not.toContain("st.style.setProperty('--crt-font-size'");
		expect(movieSource).not.toContain('--crt-font-size');
		expect(movieSource).not.toContain('.mt-crt-line');
		expect(movieSource).not.toContain('.mt-crt-cursor');
		expect(movieSource).not.toContain('.mt-crt.mt-inscreen');
		expect(movieSource).not.toContain('@keyframes mt-crt-flicker');
	});

	it('keeps the aurora motif continuous before it blooms at the inversion', () => {
		expect(movieSource).toContain('const motifIn = m.motifIn ?? m.ignite');
		expect(movieSource).toContain('const revealBloom = clamp01((rt.p - m.ignite) / 0.02)');
		expect(movieSource).not.toContain('const born = clamp01((rt.p - m.ignite) / 0.02)');
	});

	it('renders the payoff motif as a flowing ribbon rather than an orb with a short tail', () => {
		expect(movieSource).toContain('sampleMotifTrail(');
		expect(movieSource).toContain('const targetLength = motifTrailLengthPx(rt.cw)');
		expect(movieSource).toContain('const offset = motifWaveOffset(g, rt.t, revealBloom)');
		expect(movieSource).toContain('const hr = visible * motifHeadRadius(revealBloom)');
		expect(movieSource).not.toContain('head.x + Math.cos(a) * 9');
	});

	it('keeps both pile-up columns just outside the live laptop silhouette', () => {
		expect(movieSource).toContain('const PILE_MACHINE_GUTTER_PX = 8');
		expect(movieSource).toContain("st.style.setProperty('--pile-clearance'");
		expect(movieSource).toContain(
			'--rest-x: calc(var(--rx) + var(--pile-clearance, 0px))'
		);
		expect(movieSource).toContain(
			'--rest-x: calc(var(--rx) - var(--pile-clearance, 0px))'
		);
	});

	it('keeps optional film effects and the payoff composite out of the cold-load waterfall', () => {
		// Three.js belongs to the below-the-fold film, never the opening bundle.
		expect(movieSource).not.toContain("import MoteField from './MoteField.svelte'");
		expect(movieSource).toContain("moteFieldLoad = import('./MoteField.svelte')");
		expect(movieSource).toContain('if (rt.visible) ensureMoteField()');

		// The late montage is mounted only near its station, so its manifest and
		// 1.6 MB laptop photograph cannot compete with the first paint.
		expect(movieSource).toContain('let lifeMounted = false');
		expect(movieSource).toContain('if (u > -0.12)');
		expect(movieSource).toContain('{#if !reduced && lifeMounted}');
	});

	it('renders all three stations stacked and visible, beginning at the drowning', () => {
		motionEnabled.set(false);
		const { container } = render(MovieTrack);

		const scenes = container.querySelectorAll('.mt-scene');
		expect(scenes).toHaveLength(3);
		for (const scene of scenes) {
			expect(scene.classList.contains('on')).toBe(true);
			expect(scene.classList.contains('static')).toBe(true);
		}

		const track = container.querySelector('#movie-track');
		expect(track?.classList.contains('static')).toBe(true);
		expect(track?.getAttribute('style') ?? '').not.toContain('svh');

		// The drowning is the first root-film scene now that born (1977) and
		// operate (1995) — the history-of-computing arc — are cut. No opening
		// reel, fallback still, illustrated prehistory station, or either
		// machine-era act may re-enter the document path.
		expect(scenes[0]?.classList.contains('mt-s-drown')).toBe(true);
		expect(scenes[1]?.classList.contains('mt-s-tear')).toBe(true);
		expect(scenes[2]?.classList.contains('mt-s-life')).toBe(true);
		expect(screen.queryByRole('region', { name: 'The big bang' })).toBeNull();
		expect(screen.queryByRole('region', { name: 'Evolution — from fish to humans' })).toBeNull();
		expect(screen.queryByRole('region', { name: 'Fire — the first tool' })).toBeNull();
		expect(screen.queryByRole('region', { name: 'The stone blade' })).toBeNull();

		const doc = container.textContent?.replace(/\s+/g, ' ') ?? '';

		// The declaration is GONE as a beat of its own — no such region, and
		// its standalone wording must not survive anywhere.
		expect(screen.queryByRole('region', { name: 'The declaration' })).toBeNull();
		expect(doc).not.toContain('Your computer, yours again.');

		expect(doc).not.toContain('life learns to walk');
		expect(doc).not.toContain('the first tool');
		expect(doc).not.toContain('ours — every tool since');

		// THE MACHINE-ERA ACTS ARE CUT. born's terminal session (the
		// obedience-versus-help thesis, typed as READY. / > HELP /
		// ?SYNTAX ERROR) and operate's six-window carousel of the 1995
		// working day must not survive anywhere in the document, as text,
		// class or region.
		expect(screen.queryByRole('region', { name: '1977 — It did what it was told' })).toBeNull();
		expect(screen.queryByRole('region', { name: '1995 — You did it all' })).toBeNull();
		expect(container.querySelector('.mt-s-born')).toBeNull();
		expect(container.querySelector('.mt-s-operate')).toBeNull();
		expect(container.querySelector('.mt-crt')).toBeNull();
		expect(container.querySelector('.mt-owin')).toBeNull();
		expect(container.querySelector('.mt-oldcursor')).toBeNull();
		expect(container.querySelector('.mt-exo-cursor')).toBeNull();
		// The machine element itself only mounts with motion; reduced motion
		// renders the static document instead (see the `{#if !reduced}` gate).
		expect(container.querySelector('.mt-machine')).toBeNull();
		expect(doc).not.toContain('READY.');
		expect(doc).not.toContain('SYNTAX ERROR');
		expect(doc).not.toContain('You did it all');
		expect(doc).not.toContain('You did the work');
		expect(doc).not.toContain('You showcased');
		expect(doc).not.toContain('You indulged');
		expect(doc).not.toContain(
			'You did the work. You showcased. You indulged. Every step was yours.'
		);
		for (const title of [
			'Yahoo! Mail — Inbox (47)',
			'Expense claim',
			'Fares_final_v7.xls',
			'QBR_deck_v11.ppt',
			'Kyoto fares — search',
			'cartly.shop — checkout'
		]) {
			expect(doc).not.toContain(title);
		}
		expect(doc).not.toContain('how we lost');

		// “You became the glue” already closes the labour case. There is no
		// second summary station or hidden ladder before the inversion.
		expect(doc).not.toContain('And somehow, you are still doing it all');
		expect(container.querySelector('.mt-s-still')).toBeNull();
		expect(container.querySelector('.mt-rungs')).toBeNull();
		expect(container.querySelector('.mt-ask')).toBeNull();
		expect(doc).not.toContain('The most powerful tool we have ever built');

		// THE DROWNING — work multiplying, and NOBODY DOING IT TO YOU. The
		// arrivals land beside the person, the backlog stacks, and the one
		// gesture in the beat is a deferral rather than a consent.
		expect(doc).toContain('You became the glue between every app');
		// The window, and the strip that is the argument: every tab is a
		// piece of work someone opened and did not close. TWENTY-EIGHT of
		// them, and the number is the point — at fourteen the strip stayed
		// readable, and a readable tab strip is not the thing being described.
		expect(container.querySelectorAll('.mt-tab')).toHaveLength(28);
		expect(doc).toContain('Inbox (47)');
		expect(doc).toContain('Reset password');
		expect(doc).toContain('Tickets assigned (9)');
		// The counters only ever go up; the document shows where they end.
		// They end HIGH, and they accelerate to get there: a backlog that
		// climbs linearly to 74 reads as a progress bar that is nearly done.
		expect(doc).toContain('214');
		expect(doc).toContain('96');
		expect(doc).toContain('63');
		expect(doc).toContain('WhatsApp unread');
		expect(doc).toContain('Slack mentions');
		expect(doc).toContain('Payments due');
		expect(doc).toContain('Calendar conflicts');
		expect(doc).toContain('Approvals pending');
		// The counters are NOT in the window: a number inside browser chrome
		// reads as that app's own badge and gets skipped. They are loose on
		// the page, arriving from the right while the leavings arrive from
		// the left — the person closed in on from both sides.
		expect(container.querySelectorAll('.mt-badge-c')).toHaveLength(8);
		// NINE SURFACES, not four. Four reads as "a few apps"; the felt
		// experience being argued for is that there is no bottom to it. The
		// document has no scroll to switch them, so it stacks every one, and
		// each is drawn to its own SHAPE with none of its branding.
		expect(doc).toContain('Q3 fare approvals'); // inbox
		expect(doc).toContain('Action required: verify');
		expect(doc).toContain('2 required fields left'); // form
		expect(doc).toContain('Insurance renewal'); // payments
		expect(doc).toContain('Blocked on you'); // kanban
		expect(doc).toContain('#REF!'); // spreadsheet
		expect(doc).toContain('and 1,204 others liked this'); // photo feed
		// The network is a FEED with a profile card and a news rail, not a list
		// of notifications — three columns is what makes it recognisable, so
		// that is what this asserts rather than any one row's wording.
		expect(doc).toContain('Start a post…'); // network composer
		expect(doc).toContain('Profile views'); // network profile card
		expect(doc).toContain('Like · Comment · Repost · Send'); // network feed
		expect(doc).toContain('Untitled design'); // design tool
		// The arrivals carry a count that climbs as they approach, so the
		// document — where every arrival has landed — shows the final one.
		expect(doc).toContain('+28 tabs you meant to read');
		expect(doc).toContain('+61 messages unanswered');
		expect(doc).toContain('+9 forms half-filled');
		expect(doc).toContain('+7 logins to redo');
		expect(doc).toContain('+23 receipts to file');
		expect(doc).toContain('+88 notifications you swiped away');
		expect(container.querySelectorAll('.mt-exo-act')).toHaveLength(10);
		// The two edge stacks were removed: they were atmosphere, and the
		// window plus the counters make the same point with real numbers.
		expect(container.querySelector('.mt-mass')).toBeNull();
		expect(doc).not.toContain('still open');
		// THE BACKLOG IS GONE, and this pins it staying gone. It read
		// "17 tabs you meant to read" under a "still waiting on you" header
		// while the arrivals column beside it said "+28 tabs you meant to
		// read" — the same words with a different number, which is a
		// contradiction rather than emphasis. The arrivals column took the
		// job over when it grew to ten chips with climbing counts.
		expect(doc).not.toContain('still waiting on you');
		expect(doc).not.toContain('4 replies you owe');
		expect(doc).not.toContain('the follow-up you promised');
		expect(container.querySelector('.mt-exo-learned')).toBeNull();
		// The window is a MAC window: three dots is the whole cost of saying
		// so, and the strip must never squeeze them as tabs multiply.
		expect(container.querySelectorAll('.mt-s-drown .mt-light')).toHaveLength(3);
		expect(doc).toContain('Finish setting this up?');
		expect(doc).toContain('Later');
		expect(doc).toContain('The work multiplied. You didn’t.');

		// The ownership grievance is DELETED, not moved. Every one of these
		// is a line the old exodus said, and a film that still says any of
		// them is arguing two cases again.
		for (const gone of [
			'Your data became their assets',
			'Your computer stopped being yours',
			'now they know',
			'what you read',
			'what you watch',
			'who you know',
			'big tech',
			'the advertisers',
			'Allow tracking?',
			'Allow all',
			'scrolled the feed',
			'made a purchase'
		]) {
			expect(doc).not.toContain(gone);
		}

		// THE INVERSION — the hero already introduced Magican. This station pays
		// the history directly: the computer stops waiting and starts working.
		expect(doc).toContain('Your computer');
		expect(doc).toContain('stopped waiting.');
		expect(doc).toContain('Now it works for you.');
		expect(doc).not.toContain('Built only for you.');
		expect(doc).not.toContain('is not a tool you operate');
		expect(doc).toContain('on your Mac');
		expect(doc).toContain('on your iPhone');
		// The ownership payoff and the retired description must both be gone.
		expect(doc).not.toContain('makes your computer');
		expect(doc).not.toContain('Yours again');
		expect(doc).not.toContain('It operates your computer. For you.');
		expect(doc).not.toContain('Your computer. Your phone. Yours again.');

		// The invitation now points at the montage, not at a day of stations.
		expect(doc).toContain('So you can:');
		expect(doc).not.toContain('One day with it:');

		// THE MONTAGE — the closing line, standing alone. Under reduced
		// motion no reel ever mounts, so the words are the whole beat and
		// the frame must not be in the tree at all.
		expect(doc).toContain('Go. It’s handled.');
		expect(container.querySelector('.mt-lifeframe')).toBeNull();
		expect(container.querySelector('.mt-life')?.classList.contains('mt-life-bare')).toBe(true);

		// THE DAY LEFT THE FILM. Every one of these is a station that used
		// to be in the main scroll and is now road C of the fork; the film
		// carrying any of them would mean the move was half-done.
		for (const gone of [
			'It already knows you',
			'Delegate from anywhere',
			'It has hands',
			'It sits in the meeting',
			'It remembers your goals',
			'Messy in, finished out',
			'Say it out loud',
			'Even from the lock screen',
			'It teaches the way a person would',
			'Even apps it’s never met',
			'It works while you sleep',
			'@brainstorm',
			'“hey presto”',
			'The Pythagorean Theorem',
			'Connect Zepto',
			'Run receipt'
		]) {
			expect(doc).not.toContain(gone);
		}

		// No scrub chrome in document mode: the rail, skip and thread canvas
		// belong to the ride. The orbiting memory fragment is gone outright —
		// it was picked up at a station the film no longer has.
		expect(container.querySelector('.mt-rail')).toBeNull();
		expect(container.querySelector('.mt-skip')).toBeNull();
		expect(container.querySelector('.mt-frag')).toBeNull();

		// No canvas of any kind in document mode — neither the WebGL mote
		// field nor the montage. This is also what keeps jsdom canvas-free.
		expect(container.querySelector('canvas')).toBeNull();

		// Removing the prologue means reduced motion must not quietly fetch a
		// fallback frame either.
		expect(container.querySelector('img.mt-reel-still')).toBeNull();
		expect(container.querySelector('.mt-reelcaps')).toBeNull();
		expect(container.querySelectorAll('.mt-reelcap')).toHaveLength(0);
	});

	it('renders the thread-map rail with one real dot per station when motion is on', () => {
		motionEnabled.set(true);
		const { container } = render(MovieTrack);
		expect(container.querySelectorAll('.mt-dot')).toHaveLength(3);
		expect(container.querySelector('.mt-skip')).toBeTruthy();
		expect(container.querySelector('.mt-map-base')).toBeTruthy();
		expect(container.querySelector('.mt-map-done')).toBeTruthy();
		expect(container.querySelector('.mt-map-base')?.getAttribute('d')).not.toContain('NaN');
		for (const dot of container.querySelectorAll<HTMLElement>('.mt-dot')) {
			expect(dot.getAttribute('style')).not.toContain('NaN');
		}
		// The thread renders across TWO canvases — the main layer above the
		// world and an under-canvas beneath the stations for the segments
		// that dive behind station objects.
		expect(container.querySelectorAll('canvas.mt-thread')).toHaveLength(2);
		expect(container.querySelector('canvas.pr')).toBeNull();
		expect(container.querySelector('img.mt-reel-still')).toBeNull();
	});

	it('keeps the montage frame out of the tree until its footage resolves', () => {
		motionEnabled.set(true);
		const { container } = render(MovieTrack);
		// The montage is far below the fold. Neither its frame nor the canvas
		// that asks for the manifest mounts during the opening render; the film
		// activates it near the station instead.
		const frame = container.querySelector('.mt-lifeframe');
		expect(frame).toBeNull();
		expect(container.querySelector('canvas.lr')).toBeNull();
		// And the words are composed as the bare ending, not as a caption
		// waiting under a rectangle.
		expect(container.querySelector('.mt-life')?.classList.contains('mt-life-bare')).toBe(true);
	});
});
