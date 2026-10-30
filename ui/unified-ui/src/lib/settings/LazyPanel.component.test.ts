import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import LazyPanel from './LazyPanel.svelte';
import PreferencesLinkCard from './PreferencesLinkCard.svelte';
import { createPanelScheduler } from './panelLoading';

const fetchView = vi.hoisted(() => vi.fn());
vi.mock('$lib/preferences/preferencesApi', () => ({ fetchPreferenceView: fetchView }));

afterEach(() => { cleanup(); scopeIdentityStore.reset(); fetchView.mockReset(); });

it('recreates a settings panel when workspace or account changes', async () => {
	scopeIdentityStore.observe('owner', 'first');
	fetchView.mockResolvedValueOnce({ preferences: [] })
		.mockResolvedValueOnce({ preferences: [{ kind: 'goal' }] })
		.mockResolvedValueOnce({ preferences: [{ kind: 'taste' }] });
	render(LazyPanel, { scheduler: createPanelScheduler('agents'), category: 'agents',
		load: async () => ({ default: PreferencesLinkCard }) });
	await screen.findByText('0 rules · 0 goals · 0 tastes');
	scopeIdentityStore.observe('owner', 'second');
	await screen.findByText('0 rules · 1 goal · 0 tastes');
	scopeIdentityStore.observe('other-owner', 'second');
	await screen.findByText('0 rules · 0 goals · 1 taste');
	expect(fetchView).toHaveBeenCalledTimes(3);
});

it('does not render a former workspace response after switching', async () => {
	scopeIdentityStore.observe('owner', 'first');
	let finishOld!: (value: unknown) => void;
	fetchView.mockReturnValueOnce(new Promise((resolve) => { finishOld = resolve; }))
		.mockResolvedValueOnce({ preferences: [] });
	render(LazyPanel, { scheduler: createPanelScheduler('agents'), category: 'agents',
		load: async () => ({ default: PreferencesLinkCard }) });
	await waitFor(() => expect(fetchView).toHaveBeenCalledTimes(1));
	scopeIdentityStore.observe('owner', 'second');
	await screen.findByText('0 rules · 0 goals · 0 tastes');
	finishOld({ preferences: [{ kind: 'goal' }] });
	await new Promise((resolve) => setTimeout(resolve, 0));
	expect(screen.queryByText('0 rules · 1 goal · 0 tastes')).toBeNull();
});
