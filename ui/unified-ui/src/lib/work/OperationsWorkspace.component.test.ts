import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { scopeIdentityStore, setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import { installFetchMock, jsonResponse } from '../../test/browser';
import OperationsWorkspace from './OperationsWorkspace.svelte';

const agents = { method: 'GET', match: '/api/magician/v2/agents', handle: () => jsonResponse({ agents: [] }) };
vi.mock('$app/stores', async () => ({ page: (await import('svelte/store')).writable({url:new URL('http://localhost/work?view=operations'),params:{}}) }));

beforeEach(() => { installFetchMock([agents]); scopeIdentityStore.reset(); scopeIdentityStore.observe('owner', 'first'); });
afterEach(() => { cleanup(); scopeIdentityStore.reset(); });

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

it('dispatches the five existing goals in their reviewed order', async () => {
	const writes = backend(); render(OperationsWorkspace);
	await waitFor(() => expect(screen.getByRole('button', { name: 'Run Company Loop' })).toBeEnabled());
	await fireEvent.click(screen.getByRole('button', { name: 'Run Company Loop' }));
	await waitFor(() => expect(writes).toHaveLength(5));
	expect(writes.map(write => write.body.goal_id)).toEqual([
		'harness:cmo:content-calendar', 'harness:cro:weekly-gtm-review', 'harness:cpo:backlog-grooming',
		'harness:cto:morning-engineering-standup', 'harness:ceo:morning-briefing'
	]);
	expect(writes.every(write => write.body.trigger === 'manual.harness.company_loop')).toBe(true);
});

it.each(['workspace', 'credential', 'unmount'])('stops later stages after %s changes during an in-flight dispatch', async change => {
	let release!: (response: Response) => void;
	const first = new Promise<Response>(resolve => { release = resolve; });
	const writes = backend(first); const view = render(OperationsWorkspace);
	await waitFor(() => expect(screen.getByRole('button', { name: 'Run Company Loop' })).toBeEnabled());
	await fireEvent.click(screen.getByRole('button', { name: 'Run Company Loop' }));
	await waitFor(() => expect(writes).toHaveLength(1));
	if (change === 'workspace') scopeIdentityStore.observe('other-owner', 'second');
	else if (change === 'credential') setCurrentScopeBearerToken('rotated-test-credential');
	else view.unmount();
	release(jsonResponse({ status: 'accepted', execution_id: 'old-scope-result' }));
	await new Promise(resolve => setTimeout(resolve, 30));
	expect(writes).toHaveLength(1);
	expect(screen.queryByText('Started execution old-scope-result')).not.toBeInTheDocument();
	if (change === 'credential') expect(screen.getByRole('button', { name: 'Run Company Loop' })).toBeEnabled(); // A fresh scoped read completed; the old sequence still dispatched only once.
});
