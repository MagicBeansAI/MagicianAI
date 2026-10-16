import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from '$app/stores';
import { goto } from '$app/navigation';
import { clearAgents } from '$lib/stores/agentStore';
import { scopeIdentityStore, setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import { installFetchMock, jsonResponse, type MockFetchCall } from '../../test/browser';
import CrewDefinitionWorkspace from '$lib/magician/crew/CrewDefinitionWorkspace.svelte';
import CrewRulesWorkspace from '$lib/magician/crew/CrewRulesWorkspace.svelte';
import CrewMemberWorkspace from '$lib/magician/crew/CrewMemberWorkspace.svelte';

vi.mock('$app/stores', async () => ({ page: (await import('svelte/store')).writable({ url: new URL('http://localhost/work'), params: {} }) }));
vi.mock('$app/navigation', () => ({ goto: vi.fn(), replaceState: vi.fn() }));
const route = page as unknown as { set(value: { url: URL; params: Record<string, string> }): void };
const definition = { agent_id: 'forge', name: 'Forge', persona: 'Build useful tools.', kind: 'Worker', trust_level: 'local', custom_extension: { preserve: 'future value' } };
const record = { definition, version: 1, etag: '"version-one"' };

beforeEach(() => {
	vi.clearAllMocks(); scopeIdentityStore.reset(); clearAgents();
	route.set({ url: new URL('http://localhost/work/crew/forge/edit?from=%2Fwork%3Fview%3Dcrew%26agent%3Dforge%26tab%3Dconfig'), params: { id: 'forge' } });
});
afterEach(() => { cleanup(); clearAgents(); scopeIdentityStore.reset(); setCurrentScopeBearerToken(null); });

function editorBackend(save: (call: MockFetchCall) => Response | Promise<Response>) {
	return installFetchMock([
		{ method: 'GET', match: '/api/magician/v2/agents/forge', handle: () => jsonResponse(record, { headers: { etag: record.etag } }) },
		{ method: 'GET', match: '/api/magician/v2/agents', handle: () => jsonResponse({ agents: [record], system_agents: [], total_count: 1 }) },
		{ method: 'GET', match: '/api/magician/v2/skills', handle: () => jsonResponse({ skills: [] }) },
		{ method: 'PUT', match: '/api/magician/v2/agents/forge', handle: save }
	]);
}

it('reloads the member detail when credentials change before the visible workspace', async () => {
	let currentRecord = record;
	const { calls } = installFetchMock([
		{ method: 'GET', match: call => new URL(call.url, 'http://localhost').pathname === '/api/magician/v2/agents/forge', handle: () => jsonResponse(currentRecord) },
		{ method: 'GET', match: '/api/', handle: () => jsonResponse({ error: 'No fixture for this optional projection' }, { status: 404 }) }
	]);
	route.set({ url: new URL('http://localhost/work?view=crew&agent=forge'), params: {} });
	render(CrewMemberWorkspace, { workContext: true });
	await screen.findAllByRole('heading', { name: 'Forge' });
	const previousReads = calls.filter(call => new URL(call.url, 'http://localhost').pathname === '/api/magician/v2/agents/forge').length;
	currentRecord = { ...record, definition: { ...definition, name: 'Current Forge' } };
	setCurrentScopeBearerToken('changed-member-credential');
	await screen.findAllByRole('heading', { name: 'Current Forge' });
	expect(calls.filter(call => new URL(call.url, 'http://localhost').pathname === '/api/magician/v2/agents/forge').length).toBeGreaterThan(previousReads);
});

it('saves the complete guided definition with its opened ETag and returns to the member tab', async () => {
	let request: MockFetchCall | undefined;
	editorBackend(call => {
		request = call;
		return jsonResponse({ definition: JSON.parse(String(call.init?.body)), version: 2, etag: '"version-two"' });
	});
	render(CrewDefinitionWorkspace, { agentId: 'forge' });
	const name = await screen.findByRole('textbox', { name: /^Name/ });
	await waitFor(() => expect(name).toHaveValue('Forge'));
	await fireEvent.input(name, { target: { value: 'Forge improved' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Save agent' }));
	await waitFor(() => expect(goto).toHaveBeenCalledWith('/work?view=crew&agent=forge&tab=config'));
	expect(new Headers(request?.init?.headers).get('If-Match')).toBe(record.etag);
	expect(JSON.parse(String(request?.init?.body))).toMatchObject({ name: 'Forge improved', agent_id: 'forge', custom_extension: definition.custom_extension });
});

it('keeps a conflicting definition edit open with the attempted value and no success navigation', async () => {
	editorBackend(() => jsonResponse({ error: 'Definition changed. Reload before saving.' }, { status: 412 }));
	render(CrewDefinitionWorkspace, { agentId: 'forge' });
	const name = await screen.findByRole('textbox', { name: /^Name/ });
	await waitFor(() => expect(name).toHaveValue('Forge'));
	await fireEvent.input(name, { target: { value: 'Unsaved name' } });
	await fireEvent.click(screen.getByRole('button', { name: 'Save agent' }));
	await screen.findByText(/Definition changed\. Reload before saving/);
	expect(name).toHaveValue('Unsaved name');
	expect(goto).not.toHaveBeenCalled();
	expect(screen.getByRole('button', { name: 'Save agent' })).toBeEnabled();
});

it('returns from creation to the original task inspector without creating a member', async () => {
	const backend = editorBackend(() => jsonResponse(record));
	route.set({ url: new URL('http://localhost/work/crew/new?from=%2Fwork%3Fselected%3Dtask-one%26execution%3Drun-two%26plan%3Dopen'), params: {} });
	render(CrewDefinitionWorkspace);
	await fireEvent.click(screen.getByRole('button', { name: 'Back to Work' }));
	expect(goto).toHaveBeenCalledWith('/work?selected=task-one&execution=run-two&plan=open');
	expect(backend.calls.some(call => call.method === 'POST' || call.method === 'PUT')).toBe(false);
});

it('discards an older rules response after the authenticated workspace changes', async () => {
	let finishOld: (response: Response) => void = () => {};
	let calls = 0;
	installFetchMock([{ method: 'GET', match: /\/agents(?:\?|$)/, handle: () => jsonResponse({ agents: [], system_agents: [], total_count: 0 }) }, { method: 'GET', match: '/api/magician/v2/agents/forge', handle: () => {
		calls++;
		return calls === 1 ? new Promise<Response>(resolve => { finishOld = resolve; }) : jsonResponse({ ...record, definition: { ...definition, persona: 'Current workspace persona' } });
	} }]);
	route.set({ url: new URL('http://localhost/work/crew/forge/rules'), params: { id: 'forge' } });
	render(CrewRulesWorkspace);
	await waitFor(() => expect(calls).toBe(1));
	scopeIdentityStore.observe('another-owner', 'another-workspace');
	await waitFor(() => expect(calls).toBe(2));
	await waitFor(() => expect((screen.getByRole('textbox', { name: 'Persona YAML' }) as HTMLTextAreaElement).value).toContain('Current workspace persona'));
	finishOld(jsonResponse({ ...record, definition: { ...definition, persona: 'Old workspace private persona' } }));
	await new Promise(resolve => setTimeout(resolve, 0));
	expect((screen.getByRole('textbox', { name: 'Persona YAML' }) as HTMLTextAreaElement).value).toContain('Current workspace persona');
});


it('refuses to save a definition opened before credentials changed', async () => {
 const fetch = editorBackend(() => jsonResponse(record));
 render(CrewDefinitionWorkspace, {agentId:'forge'});
 await waitFor(()=>expect(screen.getByRole('textbox',{name:'Name'})).toHaveValue('Forge'));
 setCurrentScopeBearerToken('replacement-workspace-credential');
 await fireEvent.click(screen.getByRole('button',{name:'Save agent'}));
 await screen.findByText('The connection changed. Reload this editor before saving.');
 expect(fetch.calls.some(call=>call.method === 'PUT')).toBe(false);
 expect(goto).not.toHaveBeenCalled();
});

it('keeps nested harness, autonomous and focus fields during a guided name edit', async () => {
 const original = {...definition,kind:'Personal',harness:{program_section:'Engineering',custom_harness:{retained:true}},autonomous_config:{schedule:'0 9 * * *',max_tasks_per_cycle:3,max_steps_per_plan:10,custom_limit:17,focus_areas:[{name:'Launch',description:'Verify launch',priority:'high',program:'launch.md',custom_focus:{retained:true}}]}};
 let saved: Record<string,unknown> | undefined;
 installFetchMock([
  {method:'GET',match:'/api/magician/v2/agents/forge',handle:()=>jsonResponse({...record,definition:original})},
  {method:'GET',match:'/api/magician/v2/agents',handle:()=>jsonResponse({agents:[]})},
  {method:'GET',match:'/api/magician/v2/skills',handle:()=>jsonResponse({skills:[]})},
  {method:'PUT',match:'/api/magician/v2/agents/forge',handle:call=>{saved=JSON.parse(String(call.init?.body));return jsonResponse({...record,definition:saved});}}
 ]);
 render(CrewDefinitionWorkspace,{agentId:'forge'});
 const name = await screen.findByRole('textbox',{name:/^Name/});
 await waitFor(()=>expect(name).toHaveValue('Forge'));
 await fireEvent.input(name,{target:{value:'Forge updated'}});
 await fireEvent.click(screen.getByRole('button',{name:'Save agent'}));
 await waitFor(()=>expect(saved).toBeDefined());
 expect(saved).toMatchObject({name:'Forge updated',harness:original.harness,autonomous_config:{custom_limit:17,focus_areas:[{name:'Launch',custom_focus:{retained:true}}]}});
});

it('submits a future u64 field unchanged while editing a guided name', async () => {
 const raw = '{"definition":{"agent_id":"forge","name":"Forge","kind":"Worker","trust_level":"local","persona":"Build","future_limit":18446744073709551615},"version":1,"etag":"v1"}';
 let body = '';
 installFetchMock([
  {method:'GET',match:'/api/magician/v2/agents/forge',handle:()=>new Response(raw,{headers:{'Content-Type':'application/json'}})},
  {method:'GET',match:'/api/magician/v2/agents',handle:()=>jsonResponse({agents:[]})},
  {method:'GET',match:'/api/magician/v2/skills',handle:()=>jsonResponse({skills:[]})},
  {method:'PUT',match:'/api/magician/v2/agents/forge',handle:call=>{body=String(call.init?.body);return new Response(raw,{headers:{'Content-Type':'application/json'}});}}
 ]);
 render(CrewDefinitionWorkspace,{agentId:'forge'});
 const name = await screen.findByRole('textbox',{name:/^Name/}); await waitFor(()=>expect(name).toHaveValue('Forge'));
 await fireEvent.input(name,{target:{value:'Forge renamed'}});
 await fireEvent.click(screen.getByRole('button',{name:'Save agent'}));
 await waitFor(()=>expect(body).toContain('"future_limit":18446744073709551615'));
 expect(body).toContain('"name":"Forge renamed"');
});
