import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { tick } from 'svelte';

import HowShot from './HowShot.svelte';
import { HOW_SHOT_HOLD_MS } from './howShot';
import { motionEnabled } from '$lib/motion';

class ImmediateIntersectionObserver {
	constructor(private readonly callback: IntersectionObserverCallback) {}
	observe(): void {
		this.callback(
			[{ isIntersecting: true } as IntersectionObserverEntry],
			this as unknown as IntersectionObserver
		);
	}
	unobserve(): void {}
	disconnect(): void {}
	takeRecords(): IntersectionObserverEntry[] {
		return [];
	}
	readonly root = null;
	readonly rootMargin = '';
	readonly thresholds = [0];
}

afterEach(() => {
	cleanup();
	motionEnabled.set(true);
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

beforeEach(() => {
	vi.stubGlobal('IntersectionObserver', ImmediateIntersectionObserver);
});

async function advanceShot(): Promise<void> {
	await tick();
	vi.advanceTimersByTime(HOW_SHOT_HOLD_MS);
	await tick();
}

describe('HowShot', () => {
	it('wraps the chrome window so mobile can scale the desktop layout', () => {
		const { container } = render(HowShot, { props: { kind: 'superapp', active: false } });

		expect(container.querySelector('.shot-scale .shot')).toBeTruthy();
	});

	it('opens the superapp on Today with For you and Worth a look', () => {
		render(HowShot, { props: { kind: 'superapp', active: true } });

		expect(screen.getByText('For you')).toBeInTheDocument();
		expect(screen.getByText('Worth a look')).toBeInTheDocument();
		expect(screen.getByText('Reply to Priya')).toBeInTheDocument();
		expect(screen.getByText('Reply')).toBeInTheDocument();
		expect(screen.getByText('Approve')).toBeInTheDocument();
		expect(screen.getByText('Review')).toBeInTheDocument();
		expect(document.querySelector('[data-frame="today"]')).toBeTruthy();
		expect(document.querySelector('[data-pip="today"]')).toBeTruthy();
		expect(document.querySelector('[data-pip="chat"]')).toBeTruthy();
		expect(document.querySelector('[data-pip="tasks"]')).toBeTruthy();
	});

	it('cycles the superapp to Chat, then Tasks', async () => {
		vi.useFakeTimers();
		render(HowShot, { props: { kind: 'superapp', active: true } });
		await tick();

		await advanceShot();
		expect(document.querySelector('[data-frame="chat"]')).toBeTruthy();
		expect(screen.getByText('Message or describe a task')).toBeInTheDocument();
		expect(screen.getByText('Do')).toBeInTheDocument();

		await advanceShot();
		expect(document.querySelector('[data-frame="tasks"]')).toBeTruthy();
		expect(screen.getByText('Watch Kyoto fare')).toBeInTheDocument();
	});

	it('does not cycle when reduced motion is on', async () => {
		vi.useFakeTimers();
		motionEnabled.set(false);
		render(HowShot, { props: { kind: 'superapp', active: true } });
		await tick();
		await advanceShot();

		expect(document.querySelector('[data-frame="today"]')).toBeTruthy();
		expect(screen.getByText('For you')).toBeInTheDocument();
	});

	it('shows tiered memory divided by user, agent, and task episode', () => {
		render(HowShot, { props: { kind: 'local', active: false } });

		expect(screen.getByText('User')).toBeInTheDocument();
		expect(screen.getByText('Agent · Travel')).toBeInTheDocument();
		expect(screen.getByText('Task episode')).toBeInTheDocument();
	});

	it('cycles machine memory onto on-device synthesis', async () => {
		vi.useFakeTimers();
		render(HowShot, { props: { kind: 'local', active: true } });
		await tick();
		await advanceShot();

		expect(document.querySelector('[data-frame="synthesize"]')).toBeTruthy();
		expect(screen.getByText('synthesized on this Mac')).toBeInTheDocument();
	});

	it('cycles enterprise-grade from the password vault to sealed cards', async () => {
		vi.useFakeTimers();
		render(HowShot, { props: { kind: 'security', active: true } });
		expect(screen.getByText('password')).toBeInTheDocument();
		expect(screen.getByText('OS keychain')).toBeInTheDocument();

		await tick();
		await advanceShot();
		expect(document.querySelector('[data-frame="card"]')).toBeTruthy();
		expect(screen.getByText('Visa · sealed')).toBeInTheDocument();
		expect(screen.getByText('•••• •••• •••• 4242')).toBeInTheDocument();
	});

	it('shows resource authority ceilings instead of scrambled toggles', () => {
		render(HowShot, { props: { kind: 'bounds', active: false } });

		expect(document.querySelector('[data-chrome="Resource Authority"]')).toBeTruthy();
		expect(screen.getByText('USD')).toBeInTheDocument();
		expect(screen.getByText('Tokens')).toBeInTheDocument();
		expect(screen.getByText('$152 / $400 · monthly')).toBeInTheDocument();
		expect(screen.queryByText('show every plan before it runs')).not.toBeInTheDocument();
	});

	it('shows a crew roster that can work on its own', () => {
		render(HowShot, { props: { kind: 'crew', active: false } });

		expect(screen.getByText('Travel')).toBeInTheDocument();
		expect(screen.getByText('Mail')).toBeInTheDocument();
		expect(screen.getByText('Research')).toBeInTheDocument();
		expect(screen.getByText('Home')).toBeInTheDocument();
	});
});
