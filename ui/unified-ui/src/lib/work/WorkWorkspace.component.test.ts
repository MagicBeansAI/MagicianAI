import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from '$app/stores';
import { get } from 'svelte/store';
import { goto } from '$app/navigation';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { taskStore } from '$lib/stores/taskStore';
import { clearAgents } from '$lib/stores/agentStore';
import { v2Events } from '$lib/realtime/v2-websocket';
import { installFetchMock, jsonResponse } from '../../test/browser';
import { backendTask, createTaskAttentionBackend } from '../../test/taskAttentionBackend';
import WorkWorkspace from './WorkWorkspace.svelte';

vi.mock('$app/stores', async () => ({page:(await import('svelte/store')).writable({url:new URL('http://localhost/work'),params:{}})}));
vi.mock('$app/navigation',()=>({goto:vi.fn(),replaceState:vi.fn()}));
const route=page as unknown as {set(value:{url:URL;params:Record<string,string>}):void};
beforeEach(()=>{
	vi.clearAllMocks(); scopeIdentityStore.reset(); taskStore.reset(); clearAgents(); v2Events.disconnect(); v2Events.clear();
	route.set({url:new URL('http://localhost/work'),params:{}});
	const backend=createTaskAttentionBackend([backendTask('redirect','running',{title:'Fix onboarding redirect',has_plan:true,plan_status:'approved'})]);
	installFetchMock([
		{match:call=>call.url.endsWith('/programs'),handle:()=>jsonResponse({programs:[]})},
		{match:call=>call.url.endsWith('/agents'),handle:()=>jsonResponse({agents:[{definition:{agent_id:'personal-assistant',name:'Assistant',kind:'personal',is_primary:true}}]})},
		...backend.routes()
	]);
});
afterEach(()=>{cleanup();taskStore.reset();clearAgents();v2Events.disconnect();v2Events.clear();scopeIdentityStore.reset();});

it('keeps simple task creation one click away without program or quality setup',async()=>{
	render(WorkWorkspace);
	expect(screen.getByRole('link',{name:'+ New work'})).toHaveAttribute('href','/work?compose=1');
	route.set({url:new URL('http://localhost/work?compose=1'),params:{}});
	await waitFor(()=>expect(screen.getByRole('combobox',{name:'Assign crew member'})).toHaveValue('personal-assistant'));
	await waitFor(()=>expect(screen.getByRole('button',{name:'Create Task'})).toBeEnabled());
	expect(screen.queryByLabelText('Quality target (0–1)')).not.toBeInTheDocument();
	expect(screen.queryByRole('combobox',{name:'Program'})).not.toBeInTheDocument();
	expect(screen.queryByRole('textbox',{name:'Custom cron'})).not.toBeInTheDocument();
	expect(screen.getByRole('heading',{name:'New work'})).toBeInTheDocument();
	expect(screen.queryByRole('button',{name:'Fix onboarding redirect'})).not.toBeInTheDocument();
	await fireEvent.click(screen.getByRole('button',{name:'← All work'}));
	await screen.findByRole('button',{name:'Fix onboarding redirect'});
	expect(screen.getByRole('link',{name:'+ New work'})).toBeInTheDocument();
});

it('clears a retained work item when opening New work after another Work view', async () => {
	route.set({ url: new URL('http://localhost/work?selected=redirect'), params: {} });
	render(WorkWorkspace);
	await screen.findByRole('dialog', { name: 'Task panel' });
	route.set({ url: new URL('http://localhost/work?view=needs-you'), params: {} });
	await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Task panel' })).not.toBeInTheDocument());
	route.set({ url: new URL('http://localhost/work?compose=1'), params: {} });
	await screen.findByRole('heading', { name: 'New work' });
	expect(screen.queryByRole('dialog', { name: 'Task panel' })).not.toBeInTheDocument();
	expect(get(taskStore).selectedTaskId).toBeNull();
});

it('removes existing task, execution and plan focus when consuming a New work link', async () => {
	route.set({ url: new URL('http://localhost/work?selected=redirect&execution_id=old-run&plan=open'), params: {} });
	render(WorkWorkspace);
	await screen.findByRole('dialog', { name: 'Task plan inspector' });
	vi.mocked(goto).mockClear();
	route.set({ url: new URL('http://localhost/work?selected=redirect&execution_id=old-run&execution=older-run&plan=open&compose=1'), params: {} });
	await screen.findByRole('heading', { name: 'New work' });
	expect(screen.queryByRole('dialog', { name: 'Task panel' })).not.toBeInTheDocument();
	expect(screen.queryByRole('dialog', { name: 'Task plan inspector' })).not.toBeInTheDocument();
	await waitFor(() => expect(goto).toHaveBeenCalledWith('/work', expect.objectContaining({ replaceState: true })));
	expect(get(taskStore).selectedTaskId).toBeNull();
});

it('opens the mature task panel and keeps selection and plan links in Work',async()=>{
	render(WorkWorkspace);
	await fireEvent.click(await screen.findByRole('button',{name:'Fix onboarding redirect'}));
	await waitFor(()=>expect(goto).toHaveBeenCalledWith(expect.stringContaining('/work?'),expect.any(Object)));
	expect(vi.mocked(goto).mock.calls.some(call=>String(call[0]).startsWith('/tasks'))).toBe(false);
	const panel=await screen.findByRole('dialog',{name:'Task panel'});
	expect(panel.parentElement?.parentElement).toBe(document.body);
	expect(within(panel).getByRole('link',{name:'Quality review'})).toHaveAttribute('href',expect.stringContaining('/work?view=quality&task=redirect'));
	expect(within(panel).getByRole('link',{name:'Crew member'})).toHaveAttribute('href',expect.stringContaining('/work?view=crew&agent=personal-assistant'));
	expect(within(panel).getByRole('button',{name:'Open plan graph'})).toBeEnabled();
});

it('retains Programs in place when task and plan drawers open and close', async () => {
	const backend = createTaskAttentionBackend([
		backendTask('redirect', 'running', { title: 'Fix onboarding redirect', has_plan: true, plan_status: 'approved' })
	]);
	const { calls } = installFetchMock([
		{ match: call => call.url.endsWith('/programs'), handle: () => jsonResponse({ programs: [{ name: 'direction.md', title: 'Owner direction' }] }) },
		{ match: '/agents', handle: () => jsonResponse({ agents: [] }) },
		...backend.routes()
	]);
	render(WorkWorkspace);
	const program = await screen.findByRole('link', { name: /Owner direction/ });
	route.set({ url: new URL('http://localhost/work?selected=redirect'), params: {} });
	await screen.findByRole('dialog', { name: 'Task panel' });
	expect(program).toBeVisible();
	expect(screen.getByRole('link', { name: /Owner direction/, hidden: true })).toBe(program);
	route.set({ url: new URL('http://localhost/work?selected=redirect&plan=open'), params: {} });
	await screen.findByRole('dialog', { name: 'Task plan inspector' });
	expect(program).toBeVisible();
	await fireEvent.keyDown(window, { key: 'Escape' });
	await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Task plan inspector' })).not.toBeInTheDocument());
	await fireEvent.click(within(screen.getByRole('dialog', { name: 'Task panel' })).getByRole('button', { name: 'Close task panel' }));
	await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Task panel' })).not.toBeInTheDocument());
	route.set({ url: new URL('http://localhost/work'), params: {} });
	expect(screen.getByRole('link', { name: /Owner direction/ })).toBe(program);
	expect(calls.filter(call => call.url.endsWith('/programs'))).toHaveLength(1);
});

it('retargets the open plan inspector when navigation selects another task', async () => {
	const backend = createTaskAttentionBackend([
		backendTask('redirect', 'running', { title: 'Fix onboarding redirect', has_plan: true, plan_status: 'approved' }),
		backendTask('billing', 'pending', { title: 'Review billing plan', has_plan: true, plan_status: 'draft' })
	]);
	const { calls } = installFetchMock([
		{ match: '/programs', handle: () => jsonResponse({ programs: [] }) },
		{ match: '/agents', handle: () => jsonResponse({ agents: [] }) },
		...backend.routes()
	]);
	route.set({ url: new URL('http://localhost/work?selected=redirect&plan=open'), params: {} });
	render(WorkWorkspace);
	await screen.findByRole('heading', { name: 'Fix onboarding redirect' });
	route.set({ url: new URL('http://localhost/work?selected=billing&plan=open'), params: {} });
	await screen.findByRole('heading', { name: 'Review billing plan' });
	expect(screen.queryByRole('heading', { name: 'Fix onboarding redirect' })).not.toBeInTheDocument();
	await waitFor(() => expect(calls.some(call => call.url.includes('/tasks/billing/plan'))).toBe(true));
});

it('closes the plan inspector when browser navigation removes its URL state', async () => {
	route.set({ url: new URL('http://localhost/work?selected=redirect&plan=open'), params: {} });
	render(WorkWorkspace);
	await screen.findByRole('dialog', { name: 'Task plan inspector' });
	route.set({ url: new URL('http://localhost/work?selected=redirect'), params: {} });
	await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Task plan inspector' })).not.toBeInTheDocument());
	await screen.findByRole('dialog', { name: 'Task panel' });
});

it('opens and closes the nested inspector while Work navigation is pending', async () => {
	route.set({ url: new URL('http://localhost/work?selected=redirect'), params: {} });
	render(WorkWorkspace);
	const panel = await screen.findByRole('dialog', { name: 'Task panel' });
	await fireEvent.click(within(panel).getByRole('button', { name: 'Open plan graph' }));
	const inspector = await screen.findByRole('dialog', { name: 'Task plan inspector' });
	expect(inspector.parentElement?.parentElement).toBe(document.body);
	await fireEvent.keyDown(window, { key: 'Escape' });
	await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Task plan inspector' })).not.toBeInTheDocument());
	expect(panel).toBeInTheDocument();
});

it('offers quality improvement from the task overflow menu inside Work',async()=>{
	route.set({url:new URL('http://localhost/work?filter=running&tag=launch'),params:{}});
	render(WorkWorkspace);
	const title=await screen.findByRole('button',{name:'Fix onboarding redirect'});
	const card=title.closest('article');
	expect(card).not.toBeNull();
	await fireEvent.click(within(card!).getByTitle('More actions'));
	const action=screen.getByRole('menuitem',{name:'Improve quality'});
	const target=new URL(action.getAttribute('href')!, 'http://localhost');
	expect(target.pathname).toBe('/work');
	expect(target.searchParams.get('view')).toBe('quality');
	expect(target.searchParams.get('task')).toBe('redirect');
	expect(target.searchParams.get('from')).toBe('/work?filter=running&tag=launch');
});

it('resolves a cold task link in the first authenticated workspace',async()=>{
	route.set({url:new URL('http://localhost/work?selected=redirect'),params:{}});
	render(WorkWorkspace);
	await screen.findByRole('button',{name:'Fix onboarding redirect'});
	scopeIdentityStore.observe('signed-in-owner','signed-in-workspace');
	await screen.findByRole('dialog',{name:'Task panel'});
	await waitFor(()=>expect(get(taskStore).selectedTaskId).toBe('redirect'));
	expect(vi.mocked(goto).mock.calls.some(call=>String(call[0]) === '/work')).toBe(false);
});

it('drops the old task selection when switching an already resolved workspace',async()=>{
	scopeIdentityStore.observe('first-owner','first-workspace');
	route.set({url:new URL('http://localhost/work?selected=redirect'),params:{}});
	render(WorkWorkspace);
	await screen.findByRole('dialog',{name:'Task panel'});
	scopeIdentityStore.observe('second-owner','second-workspace');
	await waitFor(()=>expect(goto).toHaveBeenCalledWith('/work',expect.objectContaining({replaceState:true})));
	expect(screen.queryByRole('dialog',{name:'Task panel'})).not.toBeInTheDocument();
	expect(get(taskStore).selectedTaskId).toBeNull();
});

it('dismisses specialist navigation with Escape and restores the trigger focus',async()=>{
	render(WorkWorkspace);
	const more=screen.getByRole('button',{name:'More ▾'});
	await fireEvent.click(more);
	expect(screen.getByRole('button',{name:'Quality and review'})).toBeVisible();
	await fireEvent.keyDown(window,{key:'Escape'});
	expect(screen.queryByRole('button',{name:'Quality and review'})).not.toBeInTheDocument();
	expect(more).toHaveFocus();
});

it('scopes the full task panel and quick composer to the thread named by the Work link',async()=>{
 const backend=createTaskAttentionBackend([
  backendTask('launch-task','running',{title:'Launch thread task',ui_thread_id:'launch'}),
  backendTask('other-task','running',{title:'Another thread task',ui_thread_id:'other'})
 ]);
 installFetchMock([
  {match:'/ui-threads',handle:()=>jsonResponse({thread:{id:'launch',name:'Launch planning'},threads:[]})},
  {match:'/programs',handle:()=>jsonResponse({programs:[]})},
  {match:'/agents',handle:()=>jsonResponse({agents:[{definition:{agent_id:'personal-assistant',name:'Assistant',kind:'personal',is_primary:true}}]})},
  ...backend.routes()
 ]);
 route.set({url:new URL('http://localhost/work?thread=launch'),params:{}});
 render(WorkWorkspace);
 await screen.findByRole('button',{name:'Launch thread task'});
 expect(screen.queryByRole('button',{name:'Another thread task'})).not.toBeInTheDocument();
 expect(screen.getByRole('link',{name:'Back to chat'})).toHaveAttribute('href','/t/launch');
 expect(screen.getByRole('link',{name:'+ New work'})).toHaveAttribute('href','/work?compose=1&thread=launch');
 await fireEvent.click(screen.getByRole('button',{name:'Launch thread task'}));
 const panel=await screen.findByRole('dialog',{name:'Task panel'});
 expect(within(panel).getByRole('button',{name:/^Run(?: ·|$)/})).toBeInTheDocument();
 route.set({url:new URL('http://localhost/work?thread=launch&compose=1'),params:{}});
 await waitFor(()=>expect(screen.getByRole('button',{name:'Create Task'})).toBeEnabled());
 const thread=screen.getByRole('combobox',{name:'Thread'});
 expect(thread).toHaveValue('launch');
 expect(thread).toBeDisabled();
 await fireEvent.click(screen.getByRole('button',{name:'Clear'}));
 expect(thread).toHaveValue('launch');
});

it('keeps thread creation unavailable when the thread cannot be read',async()=>{
 installFetchMock([
  {match:'/ui-threads',handle:()=>jsonResponse({error:'not found'},{status:404})},
  {match:'/programs',handle:()=>jsonResponse({programs:[]})},
  {match:'/agents',handle:()=>jsonResponse({agents:[{definition:{agent_id:'personal-assistant',name:'Assistant',kind:'personal',is_primary:true}}]})},
  ...createTaskAttentionBackend([]).routes()
 ]);
 route.set({url:new URL('http://localhost/work?thread=missing&compose=1'),params:{}});
 render(WorkWorkspace);
 await screen.findByText('This thread could not be loaded in the current workspace.');
 await waitFor(()=>expect(screen.getByRole('button',{name:'Create Task'})).toBeDisabled());
 expect(screen.getByRole('button',{name:'Retry thread'})).toBeEnabled();
});

it('creates ordinary work in the linked thread using the real task admission request',async()=>{
 const backend=createTaskAttentionBackend([]);
 const submitted: Record<string,unknown>[]=[];
 installFetchMock([
  {method:'POST',match:call=>new URL(call.url,'http://localhost').pathname==='/api/magician/v3/tasks',handle:call=>{
   const body=JSON.parse(String(call.init?.body)); submitted.push(body);
   const created=backendTask('thread-created','pending',{title:body.title,description:body.description,ui_thread_id:body.ui_thread_id,has_plan:false,plan_status:null});
   backend.tasks.push(created); return jsonResponse({task:created}, {status:201});
  }},
  {match:'/ui-threads',handle:()=>jsonResponse({thread:{id:'launch',name:'Launch'},threads:[]})},
  {match:'/programs',handle:()=>jsonResponse({programs:[]})},
  {match:'/agents',handle:()=>jsonResponse({agents:[{definition:{agent_id:'personal-assistant',name:'Assistant',kind:'personal',is_primary:true}}]})},
  ...backend.routes()
 ]);
 route.set({url:new URL('http://localhost/work?thread=launch&compose=1'),params:{}});
 render(WorkWorkspace);
 await waitFor(()=>expect(screen.getByRole('button',{name:'Create Task'})).toBeEnabled());
 await fireEvent.input(screen.getByRole('textbox',{name:'Title'}),{target:{value:'Draft launch checklist'}});
 const description=screen.getByRole('textbox',{name:''});
 description.textContent='Prepare the checklist and explain each step.';
 await fireEvent.input(description);
 await fireEvent.click(screen.getByRole('button',{name:'Create Task'}));
 await waitFor(()=>expect(submitted).toHaveLength(1));
 expect(submitted[0]).toMatchObject({title:'Draft launch checklist',ui_thread_id:'launch',agent_id:'personal-assistant'});
 expect(submitted[0]).not.toHaveProperty('program');
 await screen.findByRole('button',{name:'Draft launch checklist'});
});
