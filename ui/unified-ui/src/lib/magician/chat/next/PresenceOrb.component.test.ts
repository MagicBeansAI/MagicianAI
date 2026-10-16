import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import PresenceOrb from './PresenceOrb.svelte';

afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

describe('PresenceOrb', () => {
	it.each([
		['idle', 'Magician is idle'],
		['thinking', 'Magician is thinking'],
		['working', 'Magician is working'],
		['needs-you', 'Magician needs you']
	] as const)('renders %s with its data-state and label', (state, label) => {
		render(PresenceOrb, { state });
		// The state is announced as the live region's text, not an aria-label.
		const orb = screen.getByRole('status');
		expect(orb).toHaveTextContent(label);
		expect(orb).not.toHaveAttribute('aria-label');
		expect(orb).toHaveAttribute('data-state', state);
		expect(orb).toHaveAttribute('data-testid', 'presence-orb');
	});

	it('updates data-state when the state changes', async () => {
		const { rerender } = render(PresenceOrb, { state: 'idle' });
		await rerender({ state: 'needs-you' });
		const orb = screen.getByRole('status');
		expect(orb).toHaveTextContent('Magician needs you');
		expect(orb).toHaveAttribute('data-state', 'needs-you');
	});

	it('opens the tooltip on hover and closes it on leave', async () => {
		const { container } = render(PresenceOrb, { state: 'idle' });
		const orb = screen.getByRole('status');
		const tip = container.querySelector('.cn-presence__tip');
		expect(tip).not.toHaveClass('cn-presence__tip--open');
		await fireEvent.mouseEnter(orb);
		expect(tip).toHaveClass('cn-presence__tip--open');
		await fireEvent.mouseLeave(orb);
		expect(tip).not.toHaveClass('cn-presence__tip--open');
	});

	it('announces a state change with the tooltip, then closes it', async () => {
		vi.useFakeTimers();
		const { container, rerender } = render(PresenceOrb, { state: 'idle' });
		const tip = () => container.querySelector('.cn-presence__tip');
		expect(tip()).not.toHaveClass('cn-presence__tip--open');
		await rerender({ state: 'working' });
		expect(tip()).toHaveClass('cn-presence__tip--open');
		expect(tip()).toHaveTextContent('Magician is working');
		await vi.advanceTimersByTimeAsync(2600);
		expect(tip()).not.toHaveClass('cn-presence__tip--open');
	});
});
