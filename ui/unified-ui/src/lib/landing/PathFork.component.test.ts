// The fork's rendered contract.
//
// Three things must hold or the section is worse than not shipping it:
//
// 1. It is CLOSED by default. Each road is ~nine screens of scrolling; a
//    visitor who does not opt in must not pay for any of them, so no panel is
//    in the DOM until a tab is pressed.
// 2. Exactly one road is mounted at a time, and another is always one press
//    away — from the head of the road and again at its end.
// 3. Every road ends by handing the ask back. The page's one conversion point
//    sits ABOVE this section, so a road that does not return the visitor to it
//    strands anyone who scrolled past the CTA to get here.
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import PathFork from './PathFork.svelte';
import { STATIONS } from './dayTrack';
import { PATHS } from './pathFork';
import { motionEnabled } from '$lib/motion';

afterEach(() => {
	cleanup();
	motionEnabled.set(true);
});

const tabs = (): HTMLElement[] => screen.getAllByRole('tab');
const take = (index: number): Promise<unknown> => fireEvent.click(tabs()[index]);
const act = (name: string): HTMLElement | null => screen.queryByRole('heading', { name });

// SKIPPED, and not because the assertions below are wrong — they were right
// about a component that no longer exists in the shape they describe.
//
// The 2026-08-17/18 landing rebuild took the fork off the page entirely and
// moved the act titles out of the road and onto `LandingChrome`, which titles
// each act in turn over the pinned section. A standalone road therefore builds
// nine stations where this file expects twelve, and `act('Knows you')` finds
// nothing: the heading is real, it is simply no longer the road's to render.
//
// Making these pass would mean restoring act signs to a component nothing
// mounts. Deleting them would mean deleting dead code on a branch a second
// session is actively working, which is its own hazard. So they are parked,
// with the reason, until the retirement sweep takes PathFork, PathDay,
// PathLifecycle, PathActSign and eleven others in one pass.
describe.skip('PathFork (retired — awaiting the deletion sweep)', () => {
	it('offers every road and opens none', () => {
		render(PathFork);

		expect(tabs()).toHaveLength(PATHS.length);
		expect(tabs()).toHaveLength(3);
		for (const path of PATHS) {
			expect(screen.getByRole('tab', { name: new RegExp(path.label, 'i') })).toHaveAttribute(
				'aria-selected',
				'false'
			);
		}
		expect(screen.queryByRole('dialog')).toBeNull();
	});

	it('leaves the first tab as the one keyboard stop until a road is chosen', () => {
		render(PathFork);

		expect(tabs()[0]).toHaveAttribute('tabindex', '0');
		expect(tabs()[1]).toHaveAttribute('tabindex', '-1');
		expect(tabs()[2]).toHaveAttribute('tabindex', '-1');
	});

	it('reveals the chosen road, labelled by the tab that opened it', async () => {
		render(PathFork);
		await take(0);

		// THE ROAD OPENS AS A MODAL. Inline, a pinned scrubbed track fought the
		// page's own scroll and the fork's sticky switcher, and a visitor deep
		// in a road had to climb back out; a dialog closes back to exactly
		// where they were. It names itself after the road rather than
		// pointing at the tab, because a dialog is not a tab panel.
		const panel = await screen.findByRole('dialog');
		expect(panel).toHaveAttribute('aria-modal', 'true');
		expect(panel.getAttribute('aria-label') ?? '').toContain('The three promises');
		expect(tabs()[0]).toHaveAttribute('aria-selected', 'true');
		expect(tabs()[1]).toHaveAttribute('aria-selected', 'false');
		expect(tabs()[2]).toHaveAttribute('aria-selected', 'false');
	});

	it('mounts only the chosen road, so the page never carries two at once', async () => {
		render(PathFork);

		await take(0);
		expect(act('Knows you')).toBeInTheDocument();
		expect(act('Started')).toBeNull();
		expect(act('Morning')).toBeNull();

		await take(1);
		expect(act('Started')).toBeInTheDocument();
		expect(act('Knows you')).toBeNull();
		expect(act('Morning')).toBeNull();

		await take(2);
		expect(
			screen.getByRole('region', { name: '7:30 — It already knows you' })
		).toBeInTheDocument();
		expect(act('Started')).toBeNull();
		expect(act('Knows you')).toBeNull();
	});

	it('carries EVERY road not taken at the head of the road you are on', async () => {
		// Not "a" road — every one of them. The switcher used to offer a single
		// alternative, which reads as a toggle when there are two roads and
		// hides a road behind a road when there are three: whichever you were
		// not on and were not being offered was simply unreachable without
		// closing the modal and starting again.
		render(PathFork);
		await take(0);

		expect(screen.getByRole('button', { name: /One job, end to end/i })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /A day with it/i })).toBeInTheDocument();
	});

	it('ends every road within one press of the ask above it', async () => {
		render(PathFork);

		for (const index of [0, 1, 2]) {
			await take(index);
			expect(screen.getByRole('button', { name: /Give it something/i })).toBeInTheDocument();
		}
	});

	it('renders three acts and nine beats on the promises road', async () => {
		render(PathFork);
		await take(0);

		const panel = await screen.findByRole('dialog');
		// 3 act signs + 9 beats — the road is the pillar row expanded, not restated
		expect(panel.querySelectorAll('[data-beat]')).toHaveLength(12);
		expect(act('Knows you')).toBeInTheDocument();
		expect(act('Acts for you')).toBeInTheDocument();
		expect(act('Belongs to you')).toBeInTheDocument();
	});

	it('renders three acts and nine beats on the lifecycle road', async () => {
		render(PathFork);
		await take(1);

		const panel = await screen.findByRole('dialog');
		expect(panel.querySelectorAll('[data-beat]')).toHaveLength(12);
		expect(act('Started')).toBeInTheDocument();
		expect(act('Moving')).toBeInTheDocument();
		expect(act('Closed')).toBeInTheDocument();
	});

	// Road C is the day-in-the-life, which used to be the film's own second
	// half. It moved here whole: the same eleven scrubbed stations, the same
	// numbers and the same closing receipt — only the placement changed. If
	// the film no longer carries it and this road does not either, the day is
	// simply gone.
	it('carries the day the film gave up, beat for beat', async () => {
		motionEnabled.set(false);
		render(PathFork);
		await take(2);

		const panel = await screen.findByRole('dialog');
		const road = within(panel);
		for (const station of STATIONS) {
			expect(
				road.getByRole('region', { name: `${station.clock} — ${station.title}` })
			).toBeInTheDocument();
		}
		expect(road.getByRole('heading', { name: 'It already knows you' })).toBeInTheDocument();
		expect(road.getByRole('heading', { name: 'Messy in, finished out' })).toBeInTheDocument();
		expect(road.getByRole('heading', { name: 'It works while you sleep' })).toBeInTheDocument();

		const doc = panel.textContent?.replace(/\s+/g, ' ') ?? '';
		expect(doc).toContain('It already knows you');
		expect(doc).toContain('the Kyoto trip we discussed');
		expect(doc).toContain('₹1,840 of your ₹2,500 cap');
		expect(doc).toContain('Migration ships Friday');
		expect(doc).toContain('Eating healthy — 12 days in');
		expect(doc).toContain('Pilot a weekend cohort in March');
		expect(doc).toContain('“Chase the deposit refund”');
		expect(doc).toContain('Connect Zepto');
		expect(doc).toContain('4 of 6 · ₹280');
		expect(doc).toContain('Kyoto fares dropped again — rebook drafted');
		expect(doc).toContain('₹212.40');
		expect(screen.getByRole('heading', { name: /That is one day\./ })).toBeInTheDocument();
	});

	it('lands the lifecycle road on completion, not on momentum', async () => {
		render(PathFork);
		await take(1);

		// The receipt is the road's whole point: the summed cost and the step
		// count, not a "still working" flourish.
		// Both figures now COUNT to their value rather than being printed, so
		// the digits live in a pseudo-element and the accessible copy sits in a
		// sibling span — `getByText` has no single element to match against.
		// Asserting on the row's own text is the honest version of the same
		// contract: the receipt states the summed cost and the step count.
		const receipt = document.body.textContent?.replace(/\s+/g, ' ') ?? '';
		expect(receipt).toContain('$0.01832625');
		expect(receipt).toContain('14 steps · 2 retries');
		expect(screen.getByRole('heading', { name: /That is one job, closed\./ })).toBeInTheDocument();
	});

	it('under reduced motion the road is a finished document with no scroll dependence', async () => {
		motionEnabled.set(false);
		render(PathFork);
		await take(0);

		const panel = await screen.findByRole('dialog');
		const beats = panel.querySelectorAll<HTMLElement>('[data-beat]');
		expect(beats).toHaveLength(12);
		for (const beat of beats) {
			// Rendered still, and never written to — the reveal action does not
			// attach at all, so there is nothing for a scroll to drive.
			expect(beat.classList.contains('still')).toBe(true);
			expect(beat.style.getPropertyValue('--local')).toBe('');
		}
		// The switcher unpins too: a sticky bar over a document that does not
		// scroll-animate is just a bar covering the top of it.
		expect(panel.querySelector('.pf-switch')?.classList.contains('still')).toBe(true);
	});
});
