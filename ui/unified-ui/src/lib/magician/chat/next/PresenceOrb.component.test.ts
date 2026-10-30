import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import PresenceOrb from './PresenceOrb.svelte';

afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

describe('PresenceOrb', () => {
	it.each(['idle', 'thinking', 'working', 'needs-you'] as const)('shares the %s animation with a distinct Dev identity', (state) => {
		render(PresenceOrb, { state, variant: 'dev', godToggleable: true });
		expect(screen.getByRole('status')).toHaveTextContent(state === 'needs-you' ? 'Dev needs you' : `Dev is ${state}`);
		expect(screen.getByRole('status')).toHaveAttribute('data-variant', 'dev');
		expect(screen.getByRole('status')).toHaveAttribute('data-state', state);
		expect(screen.getByRole('button', { name: /Open super god mode controls/ })).toHaveAttribute('aria-haspopup', 'menu');
	});
	it('delegates the Dev mode action to its owner without changing mode itself', async () => {
		const toggle = vi.fn();
		render(PresenceOrb, { props: { variant: 'dev', state: 'working', godMode: true, godToggleable: true }, events: { toggleGodMode: toggle } });
		await fireEvent.click(screen.getByRole('button', { name: /Super god mode is on. Turn it off/ }));
		expect(toggle).toHaveBeenCalledOnce();
		expect(screen.getByTestId('presence-orb')).toHaveAttribute('data-god', 'on');
	});
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
