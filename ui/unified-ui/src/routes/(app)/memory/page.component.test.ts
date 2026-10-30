import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { page } from '$app/stores';
import { installFetchMock, jsonResponse, type MockFetchHandler } from '../../../test/browser';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import Page from './+page.svelte';

vi.mock('$app/stores', async () => ({
	page: (await import('svelte/store')).writable({ url: new URL('http://localhost/memory'), params: {} })
}));

const agentDefinitions = [
	{ definition: { agent_id: 'company-assistant', name: 'Aide' } },
	{ definition: { agent_id: 'personal-assistant', name: 'Personal Assistant' } }
];

function routes(search: MockFetchHandler, agents = agentDefinitions) {
	return installFetchMock([
		{ method: 'POST', match: '/memory/search', handle: search },
		{ method: 'GET', match: '/agents?limit=', handle: () => jsonResponse({ agents }) },
		{ method: 'GET', match: /\/agents\/[^/]+\/memory$/, handle: () => jsonResponse({ tiers: [], total_count: 0 }) },
		{ method: 'GET', match: '/memory/pending-tasks', handle: () => jsonResponse({ pending: [], already_synthesized: [] }) },
		{ method: 'GET', match: '/memory/effect-review', handle: () => jsonResponse({}, { status: 404 }) },
		{ method: 'GET', match: '/taste-proposals', handle: () => jsonResponse({ enabled: false, proposals: [] }) }
	]);
}

function hit(value: string) {
	return { status: 'ok', backend: 'lancedb_hybrid', count: 1, matches: [
		{ kind: 'tier_field', scope: 'agent', tier: 'insights', semantic_memory_type: 'insight', key: value, value, score: 1.2, score_backend: 'lancedb_hybrid' }
	] };
}

async function startSearch() {
	await waitFor(() => expect(screen.getByRole('combobox', { name: 'Search as agent' })).not.toBeDisabled());
	await fireEvent.input(screen.getByRole('searchbox'), { target: { value: 'browser connection' } });
	await fireEvent.click(screen.getByRole('button', { name: /^Search$/ }));
}

beforeEach(() => {
	scopeIdentityStore.reset();
	(page as unknown as { set(value: unknown): void }).set({ url: new URL('http://localhost/memory'), params: {} });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe('Memory search', () => {
	it('defaults to chat memory, uses one server search and labels elapsed time as total', async () => {
		const { calls } = routes(() => jsonResponse(hit('Browser connection finding')));
		render(Page);
		await startSearch();
		expect(screen.getByRole('combobox', { name: 'Search as agent' })).toHaveValue('personal-assistant');
		expect(await screen.findByText('Browser connection finding', { selector: '.memory-hit-value' })).toBeInTheDocument();
		const searches = calls.filter(call => call.url.endsWith('/memory/search'));
		expect(searches).toHaveLength(1);
		expect(JSON.parse(searches[0].init?.body as string)).toMatchObject({ agent_id: 'personal-assistant', query: 'browser connection' });
		expect(screen.getByText(/\d+ ms total/)).toHaveAttribute('title', expect.stringContaining('Total request time'));
		// Selecting the default never fans out to every agent's memory.
		expect(calls.filter(call => /\/agents\/[^/]+\/memory$/.test(call.url))).toHaveLength(1);
	});

	it('honours an explicit agent link and falls back when Personal Assistant is absent', async () => {
		(page as unknown as { set(value: unknown): void }).set({ url: new URL('http://localhost/memory?agent_id=company-assistant'), params: {} });
		routes(() => jsonResponse({ status: 'ok', count: 0, matches: [] }), [agentDefinitions[0]]);
		render(Page);
		await startSearch();
		expect(screen.getByRole('combobox', { name: 'Search as agent' })).toHaveValue('company-assistant');
		expect(await screen.findByText(/No matching memory available to Aide/)).toBeInTheDocument();
	});

	it('cancels an old agent search and ignores its late response after the new search finishes', async () => {
		let finishOld!: (response: Response) => void;
		let oldSignal: AbortSignal | undefined;
		routes(call => {
			const body = JSON.parse(call.init?.body as string);
			if (body.agent_id === 'personal-assistant') {
				oldSignal = call.init?.signal as AbortSignal;
				return new Promise(resolve => { finishOld = resolve; });
			}
			return jsonResponse(hit('Company finding'));
		});
		render(Page);
		await startSearch();
		await fireEvent.change(screen.getByRole('combobox', { name: 'Search as agent' }), { target: { value: 'company-assistant' } });
		expect(oldSignal?.aborted).toBe(true);
		await fireEvent.click(screen.getByRole('button', { name: /^Search$/ }));
		expect(await screen.findByText('Company finding', { selector: '.memory-hit-value' })).toBeInTheDocument();
		finishOld(jsonResponse(hit('Old personal finding')));
		await waitFor(() => expect(screen.getByRole('button', { name: /^Search$/ })).not.toBeDisabled());
		expect(screen.queryByText('Old personal finding')).not.toBeInTheDocument();
		expect(screen.getByText('Company finding', { selector: '.memory-hit-value' })).toBeInTheDocument();
	});

	it('clears search content and rejects a late response after an account scope change', async () => {
		let finishOld!: (response: Response) => void;
		routes(() => new Promise(resolve => { finishOld = resolve; }));
		render(Page);
		await startSearch();
		scopeIdentityStore.observe('different-owner', 'default');
		await waitFor(() => expect(screen.getByRole('searchbox')).toHaveValue(''));
		finishOld(jsonResponse(hit('Previous owner finding')));
		await waitFor(() => expect(screen.queryByRole('button', { name: 'Searching…' })).not.toBeInTheDocument());
		expect(screen.queryByText('Previous owner finding')).not.toBeInTheDocument();
	});

	it('surfaces partial source failures instead of presenting an unqualified empty success', async () => {
		routes(() => jsonResponse({ status: 'partial', source_read_failed: true, reason: 'source_read_failed', count: 0, matches: [] }));
		render(Page);
		await startSearch();
		expect(await screen.findByText('Some memory sources could not be read. Results may be incomplete.')).toBeInTheDocument();
		expect(screen.getByText(/source_read_failed/)).toBeInTheDocument();
	});
});
