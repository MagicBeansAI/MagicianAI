import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { loadAgents } from '$lib/stores/agentStore';
import { scopeIdentityStore, setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import { installFetchMock, jsonResponse } from '../../test/browser';
import OperationsWorkspace from './OperationsWorkspace.svelte';

const focus = (name: string) => ({ name, description: `${name} routine`, priority: 'high' });
const harnessAgent = (agent_id: string, name: string, focus_areas: unknown[], harness: unknown = {}) => ({
	definition: { agent_id, name, kind: 'personal', harness, autonomous_config: { schedule: '0 9 * * *', focus_areas } }
});
let extraAgents: unknown[] = [];
const agents = { method: 'GET', match: '/api/magician/v2/agents', handle: () => jsonResponse({ agents: [
	// 'Weekly Replan' slugs like 'weekly replan': one server goal, one stage.
	harnessAgent('ceo', 'Atlas', [focus('accelerators'), focus('weekly replan'), focus('Weekly Replan')]),
	// A retired executive keeps its harness block but has no focus areas left.
	harnessAgent('cpo', 'Nova', []),
	harnessAgent('cro', 'Vance', [focus('prolific investors')]),
	// Focus areas on a non-harness agent are not team routines.
	{ definition: { agent_id: 'creative-mind', name: 'Muse', kind: 'personal', autonomous_config: { schedule: '0 9 * * *', focus_areas: [focus('general')] } } },
	...extraAgents
] }) };
vi.mock('$app/stores', async () => ({ page: (await import('svelte/store')).writable({url:new URL('http://localhost/work?view=operations'),params:{}}) }));

beforeEach(() => { extraAgents = []; installFetchMock([agents]); scopeIdentityStore.reset(); scopeIdentityStore.observe('owner', 'first'); });
afterEach(() => { cleanup(); scopeIdentityStore.reset(); });

it('offers no sequence when no harness agent has a focus area', async () => {
	installFetchMock([
		{ method: 'GET', match: '/api/magician/v2/agents', handle: () => jsonResponse({ agents: [harnessAgent('cpo', 'Nova', [])] }) },
		{ match: '/api/magician/v2/harness/runtime', handle: () => jsonResponse({ enabled: true, paused: false, effective_source: 'config' }) },
		{ match: '/api/magician/v2/harness/anomalies', handle: () => jsonResponse({ anomalies: [] }) }
	]);
	render(OperationsWorkspace);
	await waitFor(() => expect(screen.getByText(/No team routines are configured/)).toBeInTheDocument());
	expect(screen.getByRole('button', { name: 'Start team sequence' })).toBeDisabled();
});

function backend(first?: Promise<Response>) {
	const writes: Array<{ url: string; body: Record<string, string> }> = [];
	installFetchMock([
		agents,
		{ match: '/api/magician/v2/harness/runtime', handle: () => jsonResponse({ enabled: true, paused: false, effective_source: 'config' }) },
		{ match: '/api/magician/v2/harness/anomalies', handle: () => jsonResponse({ anomalies: [] }) },
		{ method: 'POST', match: /\/agents\/[^/]+\/trigger$/, handle: call => {
			writes.push({ url: call.url, body: JSON.parse(String(call.init?.body)) });
			return writes.length === 1 && first ? first : jsonResponse({ status: 'accepted' });
		} }
	]);
	return writes;
}

it("dispatches each harness agent's live focus areas, never a retired routine", async () => {
	const writes = backend(); render(OperationsWorkspace);
	await waitFor(() => expect(screen.getByRole('button', { name: 'Start team sequence' })).toBeEnabled());
	await fireEvent.click(screen.getByRole('button', { name: 'Start team sequence' }));
	await waitFor(() => expect(writes).toHaveLength(3));
	expect(writes.map(write => write.body.goal_id)).toEqual([
		'harness:ceo:accelerators', 'harness:ceo:weekly-replan', 'harness:cro:prolific-investors'
	]);
	expect(writes.every(write => write.body.trigger === 'manual.harness.company_loop')).toBe(true);
});

it('dispatches the stages shown at start even when the roster changes mid-sequence', async () => {
	let release!: (response: Response) => void;
	const first = new Promise<Response>(resolve => { release = resolve; });
	const writes = backend(first); render(OperationsWorkspace);
	await waitFor(() => expect(screen.getByRole('button', { name: 'Start team sequence' })).toBeEnabled());
	await fireEvent.click(screen.getByRole('button', { name: 'Start team sequence' }));
	await waitFor(() => expect(writes).toHaveLength(1));
	// A new harness agent sorts first; indexing the live list would re-send or skip a stage.
	extraAgents = [harnessAgent('aaa', 'Early', [focus('new routine')])];
	await loadAgents();
	release(jsonResponse({ status: 'accepted' }));
	await waitFor(() => expect(writes).toHaveLength(3));
	await new Promise(resolve => setTimeout(resolve, 30));
	expect(writes.map(write => write.body.goal_id)).toEqual([
		'harness:ceo:accelerators', 'harness:ceo:weekly-replan', 'harness:cro:prolific-investors'
	]);
});

it.each(['workspace', 'credential', 'unmount'])('stops later stages after %s changes during an in-flight dispatch', async change => {
	let release!: (response: Response) => void;
	const first = new Promise<Response>(resolve => { release = resolve; });
	const writes = backend(first); const view = render(OperationsWorkspace);
	await waitFor(() => expect(screen.getByRole('button', { name: 'Start team sequence' })).toBeEnabled());
	await fireEvent.click(screen.getByRole('button', { name: 'Start team sequence' }));
	await waitFor(() => expect(writes).toHaveLength(1));
	if (change === 'workspace') scopeIdentityStore.observe('other-owner', 'second');
	else if (change === 'credential') setCurrentScopeBearerToken('rotated-test-credential');
	else view.unmount();
	release(jsonResponse({ status: 'accepted', execution_id: 'old-scope-result' }));
	await new Promise(resolve => setTimeout(resolve, 30));
	expect(writes).toHaveLength(1);
	expect(screen.queryByText('Started execution old-scope-result')).not.toBeInTheDocument();
	if (change === 'credential') expect(screen.getByRole('button', { name: 'Start team sequence' })).toBeEnabled(); // A fresh scoped read completed; the old sequence still dispatched only once.
});
