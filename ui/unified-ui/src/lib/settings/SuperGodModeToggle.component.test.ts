import { fireEvent, render, screen } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Writable } from 'svelte/store';
import type { SuperGodModeState } from './godMode';

const calls = vi.hoisted(() => ({ confirm: vi.fn(), save: vi.fn(), pollNow: vi.fn() }));
vi.mock('$lib/stores/confirmationStore', () => ({ requestConfirmation: calls.confirm }));
vi.mock('$lib/settings/godMode', async () => {
	const { writable } = await import('svelte/store');
	return { superGodModePoll: { value: writable(null), pollNow: calls.pollNow }, saveSuperGodMode: calls.save, describeSuperGodModeEnd: () => '1 hour left' };
});

import { superGodModePoll } from './godMode';
import SuperGodModeToggle from './SuperGodModeToggle.svelte';
const state = superGodModePoll.value as Writable<SuperGodModeState | null>;

beforeEach(() => {
	vi.clearAllMocks();
	state.set({ active: false, enabled: false, until_ms: null, remaining_secs: null });
	calls.confirm.mockResolvedValue(false);
	calls.save.mockResolvedValue(undefined);
});

describe('Dev presence owns the existing super god approval flow', () => {
	it('offers a labelled composer control beside the activity dot and restores its own focus', async () => {
		render(SuperGodModeToggle, { orb: true, labelled: true });
		const trigger = screen.getByRole('button', { name: 'Super god mode' });
		expect(trigger).toHaveTextContent('Super God');
		await fireEvent.click(trigger);
		expect(screen.getByRole('menuitem', { name: 'Super god mode until turned off' })).toBeInTheDocument();
		await fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
		expect(trigger).toHaveFocus();
		expect(calls.save).not.toHaveBeenCalled();
	});

	it('can enable the until-turned-off duration only after confirmation', async () => {
		calls.confirm.mockResolvedValue(true);
		render(SuperGodModeToggle, { orb: true, labelled: true });
		await fireEvent.click(screen.getByRole('button', { name: 'Super god mode' }));
		await fireEvent.click(screen.getByRole('menuitem', { name: 'Super god mode until turned off' }));
		expect(calls.confirm).toHaveBeenCalledWith(expect.objectContaining({ destructive: true }));
		expect(calls.save).toHaveBeenCalledWith('forever');
	});
	it('opens duration choices and restores focus with Escape without changing mode', async () => {
		render(SuperGodModeToggle, { orb: true, presenceState: 'working' });
		const trigger = screen.getByRole('button', { name: /Dev is working.*Open super god mode controls/ });
		await fireEvent.click(trigger);
		expect(screen.getByRole('menuitem', { name: 'Super god mode for 1 hour' })).toHaveFocus();
		await fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
		expect(screen.queryByRole('menu')).not.toBeInTheDocument();
		expect(trigger).toHaveFocus();
		expect(calls.save).not.toHaveBeenCalled();
	});

	it('requires confirmation before enabling super god mode from the Dev dot', async () => {
		render(SuperGodModeToggle, { orb: true });
		await fireEvent.click(screen.getByRole('button', { name: /Open super god mode controls/ }));
		await fireEvent.click(screen.getByRole('menuitem', { name: 'Super god mode for 1 hour' }));
		expect(calls.confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Turn on super god mode for 1 hour?', destructive: true }));
		expect(calls.save).not.toHaveBeenCalled();
		calls.confirm.mockResolvedValue(true);
		await fireEvent.click(screen.getByRole('button', { name: /Open super god mode controls/ }));
		await fireEvent.click(screen.getByRole('menuitem', { name: 'Super god mode for 1 hour' }));
		expect(calls.save).toHaveBeenCalledWith('1h');
	});

	it('shows the enabled halo and can turn super god mode off', async () => {
		state.set({ active: true, enabled: true, until_ms: null, remaining_secs: null });
		render(SuperGodModeToggle, { orb: true, presenceState: 'thinking' });
		expect(screen.getByTestId('presence-orb')).toHaveAttribute('data-god', 'on');
		await fireEvent.click(screen.getByRole('button', { name: /Super god mode is on. Turn it off/ }));
		expect(calls.save).toHaveBeenCalledWith(null);
		expect(calls.confirm).not.toHaveBeenCalled();
	});
});
