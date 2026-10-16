import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import LandingAskComposer from './LandingAskComposer.svelte';
import { LANDING_ASK_RUNS } from './landingAskRuns';

function setReducedMotion(matches: boolean): void {
	Object.defineProperty(window, 'matchMedia', {
		configurable: true,
		value: vi.fn().mockImplementation(() => ({
			matches,
			media: '(prefers-reduced-motion: reduce)',
			onchange: null,
			addEventListener: vi.fn(),
			removeEventListener: vi.fn(),
			addListener: vi.fn(),
			removeListener: vi.fn(),
			dispatchEvent: vi.fn()
		}))
	});
}

beforeEach(() => {
	vi.useFakeTimers();
	setReducedMotion(false);
});

afterEach(() => {
	cleanup();
	vi.clearAllTimers();
	vi.useRealTimers();
});

describe('LandingAskComposer', () => {
	it('types the replay in the real placeholder, yields to visitor text, and resumes after clear', async () => {
		const { container } = render(LandingAskComposer);
		const input = screen.getByLabelText('Tell Magican what you want done') as HTMLInputElement;

		expect(input.placeholder).toMatch(/^F.*▏$/);
		await vi.advanceTimersByTimeAsync(320);
		expect(input.placeholder.length).toBeGreaterThan(3);

		await fireEvent.input(input, { target: { value: 'Plan my Saturday' } });
		expect(input.value).toBe('Plan my Saturday');
		expect(input.placeholder).toBe('Tell Magican what you want done');
		expect(container.querySelector('.ask-stage')?.textContent).toContain('Your turn.');
		expect(vi.getTimerCount()).toBe(0);

		await fireEvent.input(input, { target: { value: '' } });
		expect(input.placeholder).toMatch(/^F.*▏$/);
		expect(vi.getTimerCount()).toBe(1);
	});

	it('turns starter tasks into editable real input and invokes the existing submit contract', async () => {
		const submitted = vi.fn();
		render(LandingAskComposer, { props: { onSubmit: submitted } });

		await fireEvent.click(screen.getByRole('button', { name: 'Chase a refund' }));
		const input = screen.getByLabelText('Tell Magican what you want done') as HTMLInputElement;
		expect(input.value).toBe(LANDING_ASK_RUNS[2].ask);

		await fireEvent.click(screen.getByRole('button', { name: 'Start delegating' }));
		expect(submitted).toHaveBeenCalledTimes(1);
	});

	it('renders one completed, readable example instead of scheduling motion when reduced motion is set', () => {
		setReducedMotion(true);
		const { container } = render(LandingAskComposer);
		const input = screen.getByLabelText('Tell Magican what you want done') as HTMLInputElement;
		const stage = container.querySelector('.ask-stage');

		expect(input.placeholder).toBe(LANDING_ASK_RUNS[0].ask);
		expect(stage?.getAttribute('data-phase')).toBe('result');
		expect(stage?.textContent).toContain(LANDING_ASK_RUNS[0].result.title);
		for (const line of LANDING_ASK_RUNS[0].plan) expect(stage?.textContent).toContain(line);
		expect(vi.getTimerCount()).toBe(0);
	});

	it('locks the request while the parent is handing it to chat', () => {
		const { container } = render(LandingAskComposer, {
			props: { value: 'Book the sensible option', taking: true }
		});

		expect(screen.getByLabelText('Tell Magican what you want done')).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Start delegating' })).toBeDisabled();
		expect(container.querySelector('.ask-stage')?.textContent).toContain('Handing this to Magican');
	});
});

describe('landing ask replay data', () => {
	it('keeps run ids unique and every beat attached to a real plan step', () => {
		expect(new Set(LANDING_ASK_RUNS.map((run) => run.id)).size).toBe(LANDING_ASK_RUNS.length);
		for (const run of LANDING_ASK_RUNS) {
			expect(run.plan).toHaveLength(3);
			expect(run.beats.length).toBeGreaterThanOrEqual(run.plan.length);
			for (const beat of run.beats) {
				expect(beat.step).toBeGreaterThanOrEqual(0);
				expect(beat.step).toBeLessThan(run.plan.length);
				expect(beat.ms).toBeGreaterThan(0);
			}
		}
	});
});
