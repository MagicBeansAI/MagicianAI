import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from '$app/stores';
import { scopeIdentityStore, setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import { requestConfirmation } from '$lib/stores/confirmationStore';
import { installFetchMock as installRawFetchMock, jsonResponse } from '../../test/browser';
import ProgramsWorkspace from './ProgramsWorkspace.svelte';

function installFetchMock(handlers: Parameters<typeof installRawFetchMock>[0]) {
 return installRawFetchMock([...handlers,{match:call => new URL(call.url,'http://localhost').pathname.endsWith('/agents'),handle:()=>jsonResponse({agents:[]})}]);
}

vi.mock('$app/stores', async () => ({ page: (await import('svelte/store')).writable({ url: new URL('http://localhost/work?view=programs'), params: {} }) }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$lib/stores/confirmationStore', () => ({ requestConfirmation: vi.fn() }));
const route = page as unknown as { set(value: { url: URL; params: Record<string,string> }): void };
const document = { name: 'direction.md', title: 'Direction', content: '# Direction\n\nOwner text stays intact.', missions_section: null, history: [{ snapshot: 'direction-previous.md', modified: '2026-10-04T10:00:00Z' }] };

beforeEach(() => { vi.clearAllMocks(); scopeIdentityStore.observe('owner', 'work'); route.set({url:new URL('http://localhost/work?view=programs'),params:{}}); });
afterEach(() => { cleanup(); scopeIdentityStore.reset(); });

it('opens the full owner document and history with specialist progress links', async () => {
	installFetchMock([{ match: call => call.url.endsWith('/programs'), handle: () => jsonResponse({programs:[document]}) }, { match:'/programs/direction.md', handle:() => jsonResponse(document) }]);
	render(ProgramsWorkspace);
	expect(await screen.findByRole('link', {name:/Direction/})).toHaveAttribute('href','/work?view=programs&program=direction.md');
	route.set({url:new URL('http://localhost/work?view=programs&program=direction.md'),params:{}});
	await screen.findByText('Owner text stays intact.');
	expect(screen.getByRole('link',{name:'Crew and program progress →'})).toHaveAttribute('href','/work?view=crew');
	await fireEvent.click(screen.getByText('Document history · 1'));
	expect(screen.getByRole('button',{name:'Restore version'})).toBeEnabled();
});

it('discards an old workspace response even when its transport ignores cancellation', async () => {
	let release!: (response:Response) => void;
	let requests = 0;
	installFetchMock([{match:'/programs',handle:() => ++requests === 1 ? new Promise<Response>(resolve => {release=resolve;}) : jsonResponse({programs:[{name:'new.md',title:'New workspace direction'}]})}]);
	render(ProgramsWorkspace);
	await waitFor(() => expect(requests).toBe(1));
	scopeIdentityStore.observe('other-owner','other-work');
	await screen.findByRole('link',{name:/New workspace direction/});
	release(jsonResponse({programs:[{name:'private.md',title:'Previous private direction'}]}));
	await Promise.resolve();
	expect(screen.queryByText('Previous private direction')).not.toBeInTheDocument();
});

it('does not restore a document after scope changes while confirmation is pending', async () => {
	route.set({url:new URL('http://localhost/work?view=programs&program=direction.md'),params:{}});
	const fetch = installFetchMock([{match:call => call.url.endsWith('/programs'),handle:()=>jsonResponse({programs:[document]})},{match:'/programs/direction.md',handle:()=>jsonResponse(document)}]);
	let confirm!: (accepted:boolean) => void;
	vi.mocked(requestConfirmation).mockImplementation(() => new Promise(resolve=>{confirm=resolve;}));
	render(ProgramsWorkspace);
	await screen.findByText('Owner text stays intact.');
	await fireEvent.click(screen.getByText('Document history · 1'));
	await fireEvent.click(screen.getByRole('button',{name:'Restore version'}));
	await waitFor(()=>expect(requestConfirmation).toHaveBeenCalledOnce());
	scopeIdentityStore.observe('other-owner','other-work');
	await waitFor(()=>expect(fetch.calls.filter(call => call.url.endsWith('/programs')).length).toBe(2));
	confirm(true);
	await Promise.resolve();
	expect(fetch.calls.some(call=>call.url.endsWith('/revert'))).toBe(false);
});

it('shows a program API failure with a working retry instead of an empty success', async () => {
	let requests=0;
	installFetchMock([{match:'/programs',handle:()=> ++requests===1?jsonResponse({error:'Program provider unavailable'},{status:503}):jsonResponse({programs:[document]})}]);
	render(ProgramsWorkspace);
	await screen.findByRole('alert');
	expect(screen.getByRole('alert')).toHaveTextContent('Program provider unavailable');
	await fireEvent.click(screen.getByRole('button',{name:'Retry'}));
	await screen.findByRole('link',{name:/Direction/});
});

it('does not restore under replacement credentials while the visible scope is unchanged', async () => {
	route.set({url:new URL('http://localhost/work?view=programs&program=direction.md'),params:{}});
	const fetch = installFetchMock([
		{match:call => call.url.endsWith('/programs'),handle:()=>jsonResponse({programs:[document]})},
		{method:'POST',match:'/programs/direction.md/revert',handle:()=>jsonResponse({restored:true})},
		{method:'GET',match:'/programs/direction.md',handle:()=>jsonResponse(document)}
	]);
	let confirm!: (accepted:boolean) => void;
	vi.mocked(requestConfirmation).mockImplementation(() => new Promise(resolve=>{confirm=resolve;}));
	render(ProgramsWorkspace);
	await screen.findByText('Owner text stays intact.');
	await fireEvent.click(screen.getByText('Document history · 1'));
	await fireEvent.click(screen.getByRole('button',{name:'Restore version'}));
	await waitFor(()=>expect(requestConfirmation).toHaveBeenCalledOnce());
	setCurrentScopeBearerToken('replacement-credential');
	confirm(true);
	await new Promise(resolve => setTimeout(resolve, 0));
	expect(fetch.calls.some(call => call.method === 'POST')).toBe(false);
});

it('does not start a restore from a document loaded under replaced credentials', async () => {
	route.set({url:new URL('http://localhost/work?view=programs&program=direction.md'),params:{}});
	const fetch = installFetchMock([
		{match:call => call.url.endsWith('/programs'),handle:()=>jsonResponse({programs:[document]})},
		{method:'POST',match:'/revert',handle:()=>jsonResponse({restored:true})},
		{match:'/programs/direction.md',handle:()=>jsonResponse(document)}
	]);
	vi.mocked(requestConfirmation).mockResolvedValue(true);
	render(ProgramsWorkspace);
	await screen.findByText('Owner text stays intact.');
	await fireEvent.click(screen.getByText('Document history · 1'));
	setCurrentScopeBearerToken('replacement-credential');
	await fireEvent.click(screen.getByRole('button',{name:'Restore version'}));
	expect(requestConfirmation).not.toHaveBeenCalled();
	expect(fetch.calls.some(call => call.method === 'POST')).toBe(false);
});

it.each(['catalog', 'detail'])('discards a pending program %s read after credentials change', async kind => {
	if (kind === 'detail') route.set({url:new URL('http://localhost/work?view=programs&program=direction.md'),params:{}});
	let release!: () => void;
	const pending = new Promise<void>(resolve => { release = resolve; });
	let pendingReads = 0;
	const fresh = {...document,title:'Fresh direction',content:'# Fresh direction\n\nReplacement session direction.'};
	installFetchMock([
		{match:call => call.url.endsWith('/programs'),handle:async()=> {
			if (kind === 'catalog') { if (++pendingReads === 1) {await pending; return jsonResponse({programs:[document]});} return jsonResponse({programs:[fresh]}); }
			return jsonResponse({programs:[]});
		}},
		{match:'/programs/direction.md',handle:async()=> { if (++pendingReads === 1) {await pending; return jsonResponse(document);} return jsonResponse(fresh); }}
	]);
	render(ProgramsWorkspace);
	await waitFor(()=>expect(pendingReads).toBeGreaterThan(0));
	setCurrentScopeBearerToken('replacement-credential');
	release();
	await new Promise(resolve => setTimeout(resolve, 0));
	expect(screen.queryByRole('link',{name:/Direction/})).not.toBeInTheDocument();
	expect(screen.queryByText('Owner text stays intact.')).not.toBeInTheDocument();
	if (kind === 'catalog') await screen.findByRole('link',{name:/Fresh direction/});
	else await screen.findByText('Replacement session direction.');
});

it('keeps another selected program free of a late restore failure', async () => {
	route.set({url:new URL('http://localhost/work?view=programs&program=direction.md'),params:{}});
	const other = {...document,name:'other.md',title:'Other direction',content:'# Other\n\nCurrent direction stays visible.'};
	let release!: (response:Response) => void;
	installFetchMock([
		{match:call => call.url.endsWith('/programs'),handle:()=>jsonResponse({programs:[document,other]})},
		{method:'POST',match:'/revert',handle:()=>new Promise<Response>(resolve => {release=resolve;})},
		{match:'/programs/direction.md',handle:()=>jsonResponse(document)},
		{match:'/programs/other.md',handle:()=>jsonResponse(other)}
	]);
	vi.mocked(requestConfirmation).mockResolvedValue(true);
	render(ProgramsWorkspace);
	await screen.findByText('Owner text stays intact.');
	await fireEvent.click(screen.getByText('Document history · 1'));
	await fireEvent.click(screen.getByRole('button',{name:'Restore version'}));
	await waitFor(()=>expect(release).toBeDefined());
	route.set({url:new URL('http://localhost/work?view=programs&program=other.md'),params:{}});
	await screen.findByText('Current direction stays visible.');
	release(jsonResponse({error:'Previous program restore failed'},{status:503}));
	await new Promise(resolve => setTimeout(resolve, 0));
	expect(screen.queryByText('Previous program restore failed')).not.toBeInTheDocument();
});

it('still restores the confirmed version in the original scope', async () => {
	route.set({url:new URL('http://localhost/work?view=programs&program=direction.md'),params:{}});
	const fetch = installFetchMock([
		{match:call => call.url.endsWith('/programs'),handle:()=>jsonResponse({programs:[document]})},
		{method:'POST',match:'/revert',handle:()=>jsonResponse({restored:true})},
		{match:'/programs/direction.md',handle:()=>jsonResponse(document)}
	]);
	vi.mocked(requestConfirmation).mockResolvedValue(true);
	render(ProgramsWorkspace);
	await screen.findByText('Owner text stays intact.');
	await fireEvent.click(screen.getByText('Document history · 1'));
	await fireEvent.click(screen.getByRole('button',{name:'Restore version'}));
	await waitFor(()=>expect(fetch.calls.filter(call => call.method === 'POST')).toHaveLength(1));
	expect(JSON.parse(String(fetch.calls.find(call => call.method === 'POST')?.init?.body))).toEqual({snapshot:'direction-previous.md'});
});

it('connects an actual program member, metrics and provenance without mixing another document’s state', async () => {
 route.set({url:new URL('http://localhost/work?view=programs&program=direction.md'),params:{}});
 const owner = {definition:{agent_id:'owner',name:'Direction owner',kind:'Personal',harness:{program_section:'Operations'},autonomous_config:{focus_areas:[{name:'Delivery',program:'direction.md'}]}},version:8};
 const state = {program:{relative_path:'direction.md'},current_phase:'Deliver',current_step:'Verify release',updated_at:'2026-10-06T10:00:00Z',update_reason:'Verified milestone',open_loops:['Owner approval'],update_history:[{reason:'Release evidence',task_id:'evidence/1',execution_id:'older-run'}]};
 installFetchMock([
  {match:call => call.url.endsWith('/programs'),handle:()=>jsonResponse({programs:[document]})},
  {match:'/programs/direction.md',handle:()=>jsonResponse(document)},
  {match:call => new URL(call.url,'http://localhost').pathname.endsWith('/agents'),handle:()=>jsonResponse({agents:[owner],total_count:1})},
  {match:'/agents/owner/harness-overview',handle:()=>jsonResponse({context:{program:{relative_path:'direction.md'}},evaluation:{active_program_state:state,program_metrics:{documents:[{relative_path:'direction.md',metrics:[{metric_id:'Deliveries',current:3,comparator:'>=',target:2,met:true}]}]}}})}
 ]);
 render(ProgramsWorkspace);
 await screen.findByText('Verify release');
 expect(screen.getByText('Responsible member · definition v8')).toBeInTheDocument();
 expect(screen.getByRole('link',{name:'Direction owner →'})).toHaveAttribute('href',expect.stringContaining('agent=owner'));
 await fireEvent.click(screen.getByText('Recent progress evidence'));
 const evidence = new URL(screen.getByRole('link',{name:'Task and execution →'}).getAttribute('href')!,'http://localhost');
 expect(evidence.searchParams.get('selected')).toBe('evidence/1'); expect(evidence.searchParams.get('execution_id')).toBe('older-run');
 expect(evidence.searchParams.get('from')).toBe('/work?view=programs&program=direction.md');
 await fireEvent.click(screen.getByText('Evaluation metrics · 1'));
 expect(screen.getByText('Deliveries')).toBeInTheDocument();
});
