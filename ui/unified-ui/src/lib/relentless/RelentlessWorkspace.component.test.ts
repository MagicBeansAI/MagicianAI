import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, afterEach, expect, it, describe, vi } from 'vitest';
import Page from './RelentlessWorkspace.svelte';
import { scopeIdentityStore, setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import { request } from './api';
import { findReferences, readReference } from './references';
import { runGroup, runLabel, matchesSearch } from './workspace';
import { agentList, refreshAgents, type AgentSummary } from '$lib/stores/agentStore';
import type { Writable } from 'svelte/store';

vi.mock('$lib/stores/agentStore', async () => {
  const {writable,get}=await vi.importActual<typeof import('svelte/store')>('svelte/store');
  const agentList=writable<AgentSummary[]>([]);
  // `primaryAgent`: the owning-task ask dock reads the Attention store, whose realtime client imports wake-word preferences.
  return {agentList,primaryAgent:writable(null),refreshAgents:vi.fn(async()=>get(agentList))};
});
import { page } from '$app/stores';

vi.mock('$app/stores', async () => ({ page: (await import('svelte/store')).writable({ url: new URL('http://localhost/work?view=quality'), params: {} }) }));
vi.mock('./api', async () => ({ ...(await vi.importActual('./api')), request: vi.fn() }));
vi.mock('./references', () => ({findReferences:vi.fn(),readReference:vi.fn()}));
const profiles = {
  profiles: [{name:'actor',model:'Draft model',family:'actor',maximum_request_microusd:1}, {name:'critic',model:'Review model',family:'critic',maximum_request_microusd:1}],
  actor_default:'actor',critic_default:'critic'
};
const preview = {contract:{goal:'Owner goal'},contract_hash:'reviewed-hash',maximum_attempt_microusd:100,budget_can_admit_attempt:true};

// Existing review fixtures are served through a synthetic paged server. The UI
// receives only the requested page; detailed objects remain behind ID routes.
function mockApi(handler:(path:string,body?:unknown,signal?:AbortSignal)=>Promise<any>) {
  vi.mocked(request).mockImplementation(async(path,body,signal)=>{
    const url=new URL(path,'http://fixture'),p=url.pathname,q=url.searchParams;
    const limit=Number(q.get('limit')??20),offset=Number(q.get('offset')??0);
    const paged=(rows:any[])=>({pagination:{total:rows.length,limit,offset,has_more:offset+limit<rows.length},rows:rows.slice(offset,offset+limit)});
    if(p==='/catalog') {
      const runs=(await handler('/runs',undefined,signal)).runs??[];
      const proposals=(await handler('/proposals',undefined,signal)).proposals??[];
      const counts:Record<string,number>={brief:proposals.length};for(const run of runs){const group=runGroup(run);counts[group]=(counts[group]??0)+1;}
      const view=q.get('view'),status=q.get('status')??'all',search=q.get('q')??'',agent=q.get('agent_id');
      const items:any[]=[];
      if(view!=='runs'&&['all','brief'].includes(status))for(const item of proposals){
        if((!agent||agent===item.origin.agent_id)&&matchesSearch(search,item.proposal.goal,item.origin.agent_id,item.origin.task_id,item.origin.request_id))items.push({kind:'proposal',item:{goal:item.proposal.goal,origin:item.origin,reference_count:Object.keys(item.proposal.sources).length,maximum_cost_microusd:item.proposal.budget.max_cost_microusd}});
      }
      for(const run of runs){const group=runGroup(run),label=runLabel(run);
        if((view==='runs'||['review','attention'].includes(group))&&(status==='all'||status===group)&&matchesSearch(search,run.contract.goal,run.id,run.admission.task_id,label))items.push({kind:'run',item:{id:run.id,goal:run.contract.goal,task_id:run.admission.task_id,started_at:run.started_at,status:run.status,group,label,attempt_count:run.attempts.length,max_attempts:run.contract.budget.max_attempts,spend:run.spend}});
      }
      const page=paged(items);return {items:page.rows,pagination:page.pagination,counts,agents:[...new Set(proposals.map((p:any)=>p.origin.agent_id))]};
    }
    if(p.startsWith('/proposals/'))return (await handler('/proposals',undefined,signal)).proposals.find((p:any)=>p.origin.request_id===decodeURIComponent(url.pathname.split('/').at(-1)!));
    if(p==='/campaigns') {
      const all=await handler('/campaigns',body,signal),page=paged((all.campaigns??[]).filter((v:any)=>matchesSearch(q.get('q')??'',v.state.campaign.id,v.state.campaign.spec.name)));
      return {...all,campaigns:page.rows.map((v:any)=>({...v,capture_count:Object.keys(v.state.captures).length,state:{...v.state,captures:{}}})),pagination:page.pagination};
    }
    if(/^\/campaigns\/[^/]+\/captures$/.test(p)&&!body){
      const all=await handler('/campaigns',undefined,signal),view=all.campaigns.find((v:any)=>v.state.campaign.id===p.split('/')[2]);
      const page=paged(Object.values(view.state.captures).filter((c:any)=>q.get('status')==='all'||c.status===q.get('status')));
      return {captures:page.rows,pagination:page.pagination};
    }
    const result=await handler(path,body,signal);
    if(/^\/runs\/[^/]+$/.test(p)&&!body&&!result.id)return (await handler('/runs',undefined,signal)).runs.find((r:any)=>r.id===p.split('/')[2]);
    return result;
  });
}

beforeEach(() => {
  (agentList as Writable<AgentSummary[]>).set([{agent_id:'engineer',name:'Fixture owner',is_primary:true,status:'idle',updated_at:0},{agent_id:'another-owner',status:'idle',updated_at:0}]);
  vi.clearAllMocks(); scopeIdentityStore.observe('owner','work');
  (page as unknown as { set(value: { url: URL; params: Record<string, string> }): void }).set({ url: new URL('http://localhost/work?view=quality'), params: {} });
  mockApi(async path => path === '/profiles' ? profiles : path === '/preview' ? preview : path === '/proposals' ? {proposals:[]} : {runs:[]});
});
afterEach(() => {cleanup();setCurrentScopeBearerToken(null);scopeIdentityStore.reset();});

describe('owner budget continuation',()=>{
  const paused=(limits:{cost:boolean;time:boolean;attempts:boolean})=>({
    id:'budget-run',status:'budget_exhausted',reason:'Allowance reached',attention_id:null,
    generation:7,contract_hash:'frozen-hash',started_at:'2026-10-01T00:00:00Z',deadline_at:'2026-10-02T00:00:00Z',source_epoch:0,
    admission:{task_id:'budget-task'},contract:{goal:'Large owner work',budget:{max_cost_microusd:100000000,max_attempts:25},criteria:[]},
    attempts:[],decisions:[],retained:{champion:null,best_draft:null},spend:{settled_microusd:100000000,reserved_microusd:0},routing:null,owner_approved:null,handoff:null,
    exhausted_limits:limits,budget_continuation:{available:true,reason:null},budget_resets:[],
  });
  for(const hit of ['cost','time','attempts'] as const){
    it(`approves only the reached ${hit} allowance with exact owner CAS`,async()=>{
      const run=paused({cost:hit==='cost',time:hit==='time',attempts:hit==='attempts'});
      mockApi(async(path,body)=>path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:path==='/runs'?{runs:[run]}:path===`/runs/${run.id}/budget-resets`?{...run,status:'admitted'}:run);
      render(Page);
      await fireEvent.click(await screen.findByRole('button',{name:/Large owner work/}));
      expect(await screen.findByRole('heading',{name:'Continue this work?'})).toBeInTheDocument();
      expect(screen.queryByLabelText('New cost allowance (USD)')!==null).toBe(hit==='cost');
      expect(screen.queryByLabelText('New time allowance (hours)')!==null).toBe(hit==='time');
      expect(screen.queryByLabelText('New attempt allowance')!==null).toBe(hit==='attempts');
      await fireEvent.click(screen.getByRole('button',{name:'Approve continuation'}));
      await waitFor(()=>expect(request).toHaveBeenCalledWith('/runs/budget-run/budget-resets',expect.objectContaining({
        reset_id:expect.any(String),expected_generation:7,expected_contract_hash:'frozen-hash',
        reset:hit==='cost'?{cost_microusd:100000000}:hit==='time'?{wall_clock_secs:86400}:{attempts:25},
      }),expect.any(AbortSignal)));
    });
  }
  it('does not offer continuation for archived or superseded work',async()=>{
    const run={...paused({cost:true,time:false,attempts:false}),budget_continuation:{available:false,reason:'Work archived'}};
    mockApi(async path=>path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:path==='/runs'?{runs:[run]}:run);
    render(Page);await fireEvent.click(screen.getByRole('button',{name:/Runs/}));
    await fireEvent.click(await screen.findByRole('button',{name:/Large owner work/}));
    expect(await screen.findByText('Work archived')).toBeInTheDocument();
    expect(screen.queryByRole('button',{name:'Approve continuation'})).not.toBeInTheDocument();
  });
});

async function prepare() {
  render(Page);
  await fireEvent.click(screen.getByRole('button',{name:'New draft'}));
  await fireEvent.click(screen.getByText('Models and task ownership'));
  await waitFor(() => expect(screen.getByRole('option',{name:'Draft model · actor'})).toBeInTheDocument());
  await fireEvent.input(screen.getByLabelText('What should the draft accomplish?'),{target:{value:'Owner goal'}});
  await fireEvent.input(screen.getByLabelText('What makes the result good enough?'),{target:{value:'Clear complete answer'}});
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await screen.findByRole('button',{name:'Start local draft'});
}

it('discards reviewed contracts when credentials rotate without a visible workspace change', async () => {
  await prepare();
  setCurrentScopeBearerToken('replacement-review-credential');
  await waitFor(() => expect(screen.queryByRole('button', { name: 'Start local draft' })).not.toBeInTheDocument());
  expect(screen.getByLabelText('What should the draft accomplish?')).toHaveValue('');
  expect(vi.mocked(request).mock.calls.some(([path, body]) => path === '/runs' && body !== undefined)).toBe(false);
});

it('offers 500 native model calls without changing the owner budget', async () => {
  await prepare();
  await fireEvent.change(screen.getByLabelText('Work execution'), {target:{value:'task'}});
  expect(screen.getByLabelText('Model calls per attempt')).toHaveValue(500);
  expect(screen.getByLabelText('Steps per attempt')).toHaveValue(1024);
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await screen.findByRole('button',{name:'Start local draft'});
  const body=vi.mocked(request).mock.calls.filter(([path])=>path==='/preview').at(-1)?.[1] as Record<string,unknown>;
  expect(body.native_harness).toMatchObject({maximum_physical_calls:500,maximum_iterations:1024});
  expect(body.budget).toMatchObject({max_cost_microusd:100000000,max_wall_clock_secs:86400,max_attempts:25});
});

it('reviews the native task limits and requires a new review when its owner changes', async () => {
  await prepare();
  await fireEvent.change(screen.getByLabelText('Work execution'), {target:{value:'task'}});
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
  await fireEvent.input(screen.getByLabelText('Steps per attempt'), {target:{value:'6'}});
  await fireEvent.input(screen.getByLabelText('Model calls per attempt'), {target:{value:'3'}});
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await screen.findByRole('button',{name:'Start local draft'});
  expect(request).toHaveBeenCalledWith('/preview', expect.objectContaining({native_harness:{agent_id:'engineer',maximum_iterations:6,maximum_physical_calls:3,capabilities:['files']}}), expect.any(AbortSignal));
  await fireEvent.change(screen.getByLabelText('Owning agent'), {target:{value:'another-owner'}});
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
});

it('uses the workspace primary agent and offers only enabled configured owners', async () => {
  (agentList as Writable<AgentSummary[]>).set([{agent_id:'workspace-writer',name:'Scribe',is_primary:true,status:'idle',updated_at:0},{agent_id:'disabled-owner',disabled:true,status:'disabled',updated_at:0}]);
  await prepare();
  expect(screen.getByLabelText('Owning agent')).toHaveValue('workspace-writer');
  expect(screen.queryByRole('option',{name:/disabled-owner/})).not.toBeInTheDocument();
  await fireEvent.change(screen.getByLabelText('Work execution'),{target:{value:'task'}});
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await screen.findByRole('button',{name:'Start local draft'});
  expect(request).toHaveBeenLastCalledWith('/preview',expect.objectContaining({native_harness:expect.objectContaining({agent_id:'workspace-writer'})}),expect.any(AbortSignal));
});

it('requires an explicit configured owner when the workspace has no primary agent', async () => {
  (agentList as Writable<AgentSummary[]>).set([{agent_id:'workspace-writer',name:'Scribe',status:'idle',updated_at:0}]);
  render(Page);
  await fireEvent.click(screen.getByRole('button',{name:'New draft'}));
  await fireEvent.click(screen.getByText('Models and task ownership'));
  await screen.findByRole('option',{name:'Scribe · workspace-writer'});
  await fireEvent.input(screen.getByLabelText('What should the draft accomplish?'),{target:{value:'Owner goal'}});
  await fireEvent.input(screen.getByLabelText('What makes the result good enough?'),{target:{value:'Clear complete answer'}});
  expect(screen.getByLabelText('Owning agent')).toHaveValue('');
  expect(screen.getByRole('button',{name:'Review contract and reservation'})).toBeDisabled();
  await fireEvent.change(screen.getByLabelText('Owning agent'),{target:{value:'workspace-writer'}});
  expect(screen.getByRole('button',{name:'Review contract and reservation'})).toBeEnabled();
});

it('uses a primary hydrated by another consumer of the shared agent store', async () => {
  (agentList as Writable<AgentSummary[]>).set([]);
  vi.mocked(refreshAgents).mockResolvedValueOnce([]);
  render(Page);
  await fireEvent.click(screen.getByRole('button',{name:'New draft'}));
  await fireEvent.click(screen.getByText('Models and task ownership'));
  await screen.findByRole('option',{name:'Draft model · actor'});
  expect(screen.getByLabelText('Owning agent')).toHaveValue('');
  (agentList as Writable<AgentSummary[]>).set([{agent_id:'hydrated-primary',is_primary:true,status:'idle',updated_at:0}]);
  await waitFor(()=>expect(screen.getByLabelText('Owning agent')).toHaveValue('hydrated-primary'));
});

it('invalidates a reviewed contract when acceptance inputs change',async () => {
  await prepare();
  await fireEvent.input(screen.getByLabelText('Quality target (0–1)'),{target:{value:'0.9'}});
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
});

it('previews complete generic contracts exactly and invalidates edited or malformed JSON',async () => {
  await prepare();
  await fireEvent.click(screen.getByText('Complete contract for research, code or media'));
  await fireEvent.click(screen.getByRole('button',{name:'Edit complete contract'}));
  const editor=screen.getByLabelText('Complete contract JSON') as HTMLTextAreaElement;
  const contract=JSON.parse(editor.value);
  contract.sources={owner:'Pinned evidence'};
  contract.native_harness={agent_id:'engineer',maximum_iterations:6,maximum_physical_calls:3,
    capabilities:['files','web_fetch'],public_research:{maximum_requests:2,maximum_response_bytes:4096,maximum_redirects:1,timeout_secs:10,allow_search:false}};
  await fireEvent.input(editor,{target:{value:JSON.stringify(contract)}});
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await screen.findByRole('button',{name:'Start local draft'});
  expect(request).toHaveBeenLastCalledWith('/preview',contract,expect.any(AbortSignal));
  await fireEvent.input(editor,{target:{value:'{'}});
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
  expect(screen.getByRole('button',{name:'Review contract and reservation'})).toBeDisabled();
  expect(screen.getByText('The complete contract must be valid JSON.')).toBeInTheDocument();
});

it('shows the server-resolved paid tool contract before owner admission',async()=>{
  const metered={capability:'asset-tool',action:'run',maximum_calls:2,
    capability_hash:'resolved-installed-pin',allowed_parameters:['prompt','output_path'],
    pricing:{kind:'reported',maximum_cost_microusd:40,cost_pointer:'/charge',version:'reviewed-rate'},
    receipt_pointer:'/provider_ref'};
  mockApi(async path=>path==='/profiles'?profiles:path==='/preview'?{...preview,native_harness:{metered_tools:[metered]}}:path==='/proposals'?{proposals:[]}:{runs:[]});
  await prepare();
  await fireEvent.click(screen.getByText('Admitted paid tools'));
  expect(screen.getByText(/resolved-installed-pin/)).toHaveTextContent('reviewed-rate');
  expect(vi.mocked(request).mock.calls.some(([path,body])=>path==='/runs'&&body!==undefined)).toBe(false);
});

it('keeps existing task intent fixed and retries the exact interrupted admission',async () => {
  const originalUrl=window.location.href;
  window.history.replaceState(null,'','/work?view=quality&task=task-existing');
  const context={task_id:'task-existing',context_hash:'reviewed-task-context',title:'Existing work',goal:'Preserve this task goal',agent_id:'task-owner',ui_thread_id:'task-thread',can_start:true,reason:null,pending_run_id:'00000000-0000-0000-0000-000000000001',pending_proposal_hash:'proposal-hash'};
  const starts:unknown[]=[];
  mockApi(async (path,body) => {
    if(path==='/task-context/task-existing')return context;
    if(path==='/runs'&&body){starts.push(body);throw new Error('Response lost');}
    return path==='/profiles'?profiles:path==='/preview'?preview:path==='/proposals'?{proposals:[]}:{runs:[]};
  });
  try {
    render(Page);
    await waitFor(()=>expect(screen.getByLabelText('What should the draft accomplish?')).toHaveValue(context.goal));
    expect(screen.getByLabelText('What should the draft accomplish?')).toBeDisabled();
    expect(screen.getByLabelText('Owning agent')).toHaveValue(context.agent_id);
    expect(screen.getByLabelText('Owning agent')).toBeDisabled();
    expect(screen.getByLabelText('Task thread')).toHaveValue(context.ui_thread_id);
    expect(screen.getByLabelText('Work execution')).toHaveValue('task');
    await fireEvent.input(screen.getByLabelText('What makes the result good enough?'),{target:{value:'Complete this existing work'}});
    await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
    const start=await screen.findByRole('button',{name:'Start local draft'});
    await fireEvent.click(start);
    await screen.findByRole('alert');
    await waitFor(()=>expect(start).toBeEnabled());
    await fireEvent.click(start);
    await waitFor(()=>expect(starts).toHaveLength(2));
    for(const body of starts)expect(body).toMatchObject({
      run_id:context.pending_run_id,existing_task:{task_id:context.task_id,context_hash:context.context_hash},
      agent_id:context.agent_id,ui_thread_id:context.ui_thread_id,
      proposal:{goal:context.goal,native_harness:{agent_id:context.agent_id}},
    });
  }finally{window.history.replaceState(null,'',originalUrl);}
});

it.each([true, false])('uses the existing task answer flow only for its pending actor question (checkpoint: %s)', async (hasCheckpoint) => {
  const attention = 'pending-question';
  const run = {
    id:'waiting-run',status:'waiting_owner',reason:hasCheckpoint?'Which audience?':'Provider unavailable',attention_id:attention,
    deadline_at:new Date().toISOString(),admission:{task_id:'task waiting'},
    contract:{goal:'A task waiting for input',budget:{max_attempts:2,max_cost_microusd:1000},criteria:[]},
    attempts:[{number:1,artifact:null,checkpoint:hasCheckpoint?{attention_id:attention}:null,verdicts:{},decided:false}],
    decisions:[],spend:{settled_microusd:0,reserved_microusd:100},routing:null,owner_approved:null,handoff:null
  };
  mockApi(async path => path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:{runs:[run]});
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/A task waiting for input/}));
  if(hasCheckpoint) {
    expect(screen.getByRole('link',{name:'Answer task question'})).toHaveAttribute('href','/work?selected=task+waiting');
    expect(screen.queryByRole('button',{name:'Resume after resolving blocker'})).not.toBeInTheDocument();
  } else {
    expect(screen.queryByRole('link',{name:'Answer task question'})).not.toBeInTheDocument();
    expect(screen.getByRole('button',{name:'Resume after resolving blocker'})).toBeEnabled();
  }
  expect(request).not.toHaveBeenCalledWith('/runs/waiting-run/resume',expect.anything(),expect.anything());
});

it('reviews the exact agent proposal before submitting its origin and contract hash',async () => {
  const item={proposal:{goal:'Agent-proposed draft',actor_profile:'actor',critic_profiles:['critic','critic','critic'],sources:{brief:'Sealed source'},criteria:[{id:'custom',specification:{kind:'rubric',instructions:'Custom requirements'}}],budget:{max_cost_microusd:900000,max_attempts:2,max_wall_clock_secs:120},handoff_kind:'capture'},proposal_hash:'proposal-hash',origin:{request_id:'proposal-request',agent_id:'author',task_id:'source-task',execution_id:'source-execution'}};
  mockApi(async (path,body)=>{
    if(path==='/profiles')return profiles;
    if(path==='/proposals')return {proposals:[item]};
    if(path==='/preview')return preview;
    if(path==='/runs'&&body)throw new Error('Response lost');
    return {runs:[]};
  });
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:'author: Agent-proposed draft'}));
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
  await screen.findByText('Sealed source');
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await screen.findByRole('button',{name:'Start local draft'});
  expect(request).toHaveBeenCalledWith('/preview',item.proposal,expect.any(AbortSignal));
  await fireEvent.click(screen.getByRole('button',{name:'Start local draft'}));
  await screen.findByRole('alert');
  expect(request).toHaveBeenCalledWith('/runs',expect.objectContaining({proposal:item.proposal,agent_id:'author',proposal_request_id:'proposal-request',expected_contract_hash:'reviewed-hash'}),expect.any(AbortSignal));
});

it('reuses the admission ID after an uncertain start response',async () => {
  await prepare();
  const ids:string[]=[];
  mockApi(async (path,body) => {
    if(path==='/runs' && body) {ids.push((body as {run_id:string}).run_id);throw new Error('Response lost');}
    return path==='/profiles'?profiles:{runs:[]};
  });
  await fireEvent.click(screen.getByRole('button',{name:'Start local draft'}));
  await screen.findByRole('alert');
  await waitFor(()=>expect(screen.getByRole('button',{name:'Start local draft'})).toBeEnabled());
  await fireEvent.click(screen.getByRole('button',{name:'Start local draft'}));
  await waitFor(()=>expect(ids).toHaveLength(2));
  expect(ids[0]).toBe(ids[1]);
});

it.each([
  ['Owning agent', 'agent_id', 'another-owner'],
  ['Task thread', 'ui_thread_id', 'another-thread'],
])('starts a new admission after an uncertain response and a reviewed change to %s',async (label,field,value) => {
  const starts:Record<string,unknown>[]=[];
  mockApi(async (path,body) => {
    if(path==='/runs'&&body){starts.push(body as Record<string,unknown>);throw new Error('Response lost');}
    return path==='/profiles'?profiles:path==='/preview'?preview:{runs:[]};
  });
  await prepare();
  await fireEvent.click(screen.getByRole('button',{name:'Start local draft'}));
  await screen.findByRole('alert');
  await waitFor(()=>expect(screen.getByLabelText(label)).toBeEnabled());
  const control=screen.getByLabelText(label);
  if(control instanceof HTMLSelectElement)await fireEvent.change(control,{target:{value}});
  else await fireEvent.input(control,{target:{value}});
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await fireEvent.click(await screen.findByRole('button',{name:'Start local draft'}));
  await waitFor(()=>expect(starts).toHaveLength(2));
  expect(starts[1][field]).toBe(value);
  expect(starts[1].run_id).not.toBe(starts[0].run_id);
});

it('starts a new admission when a repeated review resolves a different contract hash',async () => {
  let hash='reviewed-hash';
  const starts:Record<string,unknown>[]=[];
  mockApi(async(path,body)=>{
    if(path==='/runs'&&body){starts.push(body as Record<string,unknown>);throw new Error('Response lost');}
    return path==='/profiles'?profiles:path==='/preview'?{...preview,contract_hash:hash}:{runs:[]};
  });
  await prepare();
  await fireEvent.click(screen.getByRole('button',{name:'Start local draft'}));
  await screen.findByRole('alert');
  await waitFor(()=>expect(screen.getByRole('button',{name:'Review contract and reservation'})).toBeEnabled());
  hash='newly-resolved-contract';
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await fireEvent.click(await screen.findByRole('button',{name:'Start local draft'}));
  await waitFor(()=>expect(starts).toHaveLength(2));
  expect(starts[1].expected_contract_hash).toBe(hash);
  expect(starts[1].run_id).not.toBe(starts[0].run_id);
});

function reviewRun(id:string,goal:string,withPrevious=false) {
  const attempt=(number:number,hash:string)=>({number,artifact:{hash,files:{'draft.md':hash}},verdicts:{},decided:true});
  return {id,status:'accepted',reason:'Accepted',attention_id:null,started_at:new Date().toISOString(),deadline_at:new Date().toISOString(),source_epoch:0,
    admission:{task_id:`task-${id}`},contract:{goal,budget:{max_attempts:5,max_cost_microusd:1000},criteria:[]},
    attempts:withPrevious?[attempt(1,'old'),attempt(2,'accepted')]:[attempt(1,'accepted')],decisions:[],spend:{settled_microusd:0,reserved_microusd:0},
    routing:{artifact_hash:'accepted',route:'human_review',reason:'Owner review'},owner_approved:null,handoff:null};
}

it('allows exact-draft review while the optional previous attempt is still loading',async () => {
  const run=reviewRun('run','Current draft with slow comparison',true);
  let release:(value:unknown)=>void=()=>{};
  mockApi(async path=>path==='/profiles'?profiles:path.endsWith('/artifacts/accepted')?{files:{'draft.md':'Exact current bytes'}}:
    path.endsWith('/artifacts/old')?new Promise(r=>{release=r;}):{runs:[run]});
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/Current draft with slow comparison/}));
  try {
    await screen.findByText('Exact current bytes');
    expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled();
    expect(screen.queryByRole('button',{name:'Compare previous attempt'})).not.toBeInTheDocument();
  } finally { release({files:{'draft.md':'Previous comparison bytes'}}); }
  await screen.findByRole('button',{name:'Compare previous attempt'});
  await fireEvent.click(screen.getByRole('button',{name:'Compare previous attempt'}));
  expect(screen.getByText('Previous comparison bytes')).toBeInTheDocument();
});

it('keeps valid current bytes reviewable after a failed comparison and retries only that comparison',async () => {
  const run=reviewRun('run','Current draft with unavailable comparison',true);
  let unavailable=true;
  mockApi(async(path,body)=>{
    if(path==='/profiles')return profiles;
    if(path.endsWith('/artifacts/accepted'))return {files:{'draft.md':'Exact current bytes'}};
    if(path.endsWith('/artifacts/old')){
      if(unavailable)throw new Error('Previous file is unavailable');
      return {files:{'draft.md':'Recovered previous bytes'}};
    }
    if(path==='/runs/run/review'&&body)return {...run,owner_approved:true};
    return {runs:[run]};
  });
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/Current draft with unavailable comparison/}));
  await screen.findByText('Exact current bytes');
  expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled();
  const retry=await screen.findByRole('button',{name:'Retry previous attempt'});
  const currentReads=vi.mocked(request).mock.calls.filter(([path])=>path.endsWith('/artifacts/accepted')).length;
  unavailable=false;
  await fireEvent.click(retry);
  await screen.findByRole('button',{name:'Compare previous attempt'});
  expect(vi.mocked(request).mock.calls.filter(([path])=>path.endsWith('/artifacts/accepted'))).toHaveLength(currentReads);
  await fireEvent.click(screen.getByRole('button',{name:'Compare previous attempt'}));
  expect(screen.getByText('Recovered previous bytes')).toBeInTheDocument();
  await fireEvent.click(screen.getByRole('button',{name:'Approve exact draft'}));
  await waitFor(()=>expect(request).toHaveBeenCalledWith('/runs/run/review',{artifact_hash:'accepted',approved:true},expect.any(AbortSignal)));
});

it('keeps approval blocked when the accepted artifact fails even if its comparison loads',async()=>{
  const run=reviewRun('run','Unavailable accepted draft',true);
  mockApi(async path=>{
    if(path==='/profiles')return profiles;
    if(path.endsWith('/artifacts/accepted'))throw new Error('Accepted artifact unavailable');
    if(path.endsWith('/artifacts/old'))return {files:{'draft.md':'Previous bytes'}};
    return {runs:[run]};
  });
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/Unavailable accepted draft/}));
  expect(await screen.findByRole('alert')).toHaveTextContent('Accepted artifact unavailable');
  expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeDisabled();
  expect(screen.queryByText('Previous bytes')).not.toBeInTheDocument();
});

it('opens an exact review link without replacing it with its contextual task composer',async()=>{
  const originalUrl=window.location.href;
  window.history.replaceState(null,'','/work?view=quality&run=review-target&task=task-context');
  const run=reviewRun('review-target','Exact linked draft');
  mockApi(async path=>path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:
    path.endsWith('/artifacts/accepted')?{files:{'draft.md':'Exact linked bytes'}}:{runs:[run]});
  try {
    render(Page);
    await screen.findByText('Exact linked bytes');
    expect(screen.getByRole('heading',{name:'Exact linked draft'})).toBeInTheDocument();
    expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled();
    expect(screen.queryByRole('heading',{name:'Improve existing task'})).not.toBeInTheDocument();
    expect(vi.mocked(request).mock.calls.some(([path])=>path.startsWith('/task-context/'))).toBe(false);
  } finally {window.history.replaceState(null,'',originalUrl);}
});

it('discards a comparison failure after selecting another run',async()=>{
  const first=reviewRun('first','First run with pending comparison',true),second=reviewRun('second','Second run without comparison');
  let reject:(reason:Error)=>void=()=>{};
  mockApi(async path=>path==='/profiles'?profiles:path.endsWith('/artifacts/accepted')?{files:{'draft.md':'Exact current bytes'}}:
    path.endsWith('/artifacts/old')?new Promise((_,r)=>{reject=r;}):{runs:[first,second]});
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/First run with pending comparison/}));
  await screen.findByText('Exact current bytes');
  await fireEvent.click(screen.getByRole('button',{name:/Second run without comparison/}));
  await screen.findByRole('heading',{name:'Second run without comparison'});
  reject(new Error('Old comparison failed'));
  await new Promise(resolve=>setTimeout(resolve,0));
  await waitFor(()=>expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled());
  expect(screen.queryByRole('button',{name:'Retry previous attempt'})).not.toBeInTheDocument();
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
});

it.each(['run','proposal'])('retains a bookmarked %s while initial workspace resolution interrupts its detail request',async kind=>{
  const originalUrl=window.location.href;
  window.history.replaceState(null,'',`/work?view=quality&${kind}=bookmarked`);
  const run=reviewRun('bookmarked','Bookmarked accepted run');
  const brief={proposal:{goal:'Bookmarked agent proposal',actor_profile:'actor',critic_profiles:['critic','critic','critic'],sources:{},criteria:[],budget:{max_cost_microusd:1000,max_attempts:3,max_wall_clock_secs:120}},proposal_hash:'proposal-hash',origin:{request_id:'bookmarked',agent_id:'author',task_id:'source-task',execution_id:'execution'}};
  const record=kind==='run'?run:brief;
  let reads=0,release:(value:unknown)=>void=()=>{};
  vi.mocked(request).mockImplementation(async path=>{
    if(path==='/profiles')return profiles;
    if(path.startsWith('/catalog'))return {items:[],pagination:{total:0,limit:20,offset:0,has_more:false},counts:{},agents:[]};
    if(path===`/${kind==='run'?'runs':'proposals'}/bookmarked`){reads++;return reads===1?new Promise(r=>{release=r;}):record;}
    if(path.endsWith('/artifacts/accepted'))return {files:{'draft.md':'Bookmarked current bytes'}};
    return {};
  });
  try {
    render(Page);
    await waitFor(()=>expect(reads).toBe(1));
    scopeIdentityStore.observe('resolved-owner','resolved-workspace');
    await waitFor(()=>expect(reads).toBe(2));
    if(kind==='run')await screen.findByRole('heading',{name:run.contract.goal});
    else await screen.findByRole('heading',{name:brief.proposal.goal});
    release(record);
    await new Promise(resolve=>setTimeout(resolve,0));
    expect(reads).toBe(2);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  } finally {release(record);window.history.replaceState(null,'',originalUrl);}
});

it.each([
  ['cancel','Cancel run'],
  ['review','Approve exact draft'],
  ['report','Inspect run report'],
  ['lessons','Inspect run lessons'],
])('does not show a failed %s request on another selected run',async (action,label)=>{
  const first=reviewRun('first','First reviewed run'),second=reviewRun('second','Second reviewed run');
  let reject:(reason:Error)=>void=()=>{};
  mockApi(async path=>{
    if(path==='/profiles')return profiles;
    if(path.endsWith('/artifacts/accepted'))return {files:{'draft.md':path.includes('/first/')?'First bytes':'Second bytes'}};
    if(path===`/runs/first/${action}`)return new Promise((_,r)=>{reject=r;});
    return {runs:[first,second]};
  });
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/First reviewed run/}));
  await screen.findByText('First bytes');
  if(action==='report'||action==='lessons')await fireEvent.click(screen.getByText('Reports and learning'));
  await fireEvent.click(screen.getByRole('button',{name:label}));
  await waitFor(()=>expect(vi.mocked(request).mock.calls.some(([path])=>path===`/runs/first/${action}`)).toBe(true));
  await fireEvent.click(screen.getByRole('button',{name:/Second reviewed run/}));
  await screen.findByText('Second bytes');
  reject(new Error('First run request failed'));
  await waitFor(()=>expect(screen.getByRole('button',{name:'Cancel run'})).toBeEnabled());
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  expect(screen.getByRole('heading',{name:'Second reviewed run'})).toBeInTheDocument();
});

it('does not show a late artifact download failure on another selected run',async()=>{
  const first=reviewRun('first','First run with binary download'),second=reviewRun('second','Second run after download');
  Object.assign(first.attempts[0].artifact.files,{'report.bin':'sealed-file-hash'});
  let reject:(reason:Error)=>void=()=>{};
  const fetchSpy=vi.spyOn(globalThis,'fetch').mockImplementation(()=>new Promise((_,r)=>{reject=r;}));
  mockApi(async path=>path==='/profiles'?profiles:path.endsWith('/artifacts/accepted')?
    {files:{'draft.md':'Current bytes'},binary_files:path.includes('/first/')?{'report.bin':{media_type:'application/octet-stream',bytes:1,content_hash:'sealed-file-hash'}}:{}}:{runs:[first,second]});
  try {
    render(Page);
    await fireEvent.click(await screen.findByRole('button',{name:/First run with binary download/}));
    await fireEvent.click(await screen.findByRole('button',{name:'Download report.bin'}));
    await waitFor(()=>expect(fetchSpy.mock.calls.some(([path])=>String(path).includes('/runs/first/artifacts/accepted/file?name=report.bin'))).toBe(true));
    await fireEvent.click(screen.getByRole('button',{name:/Second run after download/}));
    await screen.findByRole('heading',{name:'Second run after download'});
    reject(new Error('First download failed'));
    await new Promise(resolve=>setTimeout(resolve,0));
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  } finally {fetchSpy.mockRestore();}
});

it('discards a pending contract response after workspace changes',async () => {
  let resolve:(value:unknown)=>void=()=>{};
  mockApi(async path => path==='/profiles'?profiles:path==='/preview'?new Promise(r=>{resolve=r;}):{runs:[]});
  render(Page);
  await fireEvent.click(screen.getByRole('button',{name:'New draft'}));
  await fireEvent.click(screen.getByText('Models and task ownership'));
  await waitFor(()=>expect(screen.getByRole('option',{name:'Draft model · actor'})).toBeInTheDocument());
  await fireEvent.input(screen.getByLabelText('What should the draft accomplish?'),{target:{value:'Private owner goal'}});
  await fireEvent.input(screen.getByLabelText('What makes the result good enough?'),{target:{value:'Complete'}});
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  scopeIdentityStore.observe('another-owner','another-workspace');
  resolve(preview);
  await waitFor(()=>expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument());
  expect(screen.getByLabelText('What should the draft accomplish?')).toHaveValue('');
});

it('requires the accepted bytes to finish loading before exact-draft approval',async () => {
  const draft=(number:number,hash:string)=>({number,artifact:{hash,files:{'draft.md':hash}},verdicts:{},decided:true});
  const run={id:'run',status:'accepted',reason:'Accepted',attention_id:null,started_at:new Date().toISOString(),deadline_at:new Date().toISOString(),source_epoch:0,
    admission:{task_id:'task'},contract:{goal:'Accepted owner draft',budget:{max_attempts:5,max_cost_microusd:1000},criteria:[]},
    attempts:[draft(1,'old'),draft(2,'accepted')],decisions:[],retained:{champion:null,best_draft:null},spend:{settled_microusd:0,reserved_microusd:0},
    routing:{artifact_hash:'accepted',route:'human_review',reason:'Human judgment needed'},owner_approved:null,handoff:null};
  let release:(value:unknown)=>void=()=>{};
  let pause=false;
  mockApi(async path=>path==='/profiles'?profiles:path.endsWith('/artifacts/accepted')?
    pause?new Promise(r=>{release=r;}):{files:{'draft.md':'Exact accepted draft'}}:
    path.endsWith('/artifacts/old')?{files:{'draft.md':'Older draft'}}:{runs:[run]});
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/Accepted owner draft/}));
  await waitFor(()=>expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled());
  await fireEvent.click(screen.getByText('Attempt history · 2 attempts'));
  await fireEvent.click(screen.getByRole('button',{name:/Attempt 1/}));
  await screen.findByText('Older draft');
  pause=true;
  await fireEvent.click(screen.getByRole('button',{name:/Attempt 2/}));
  expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeDisabled();
  release({files:{'draft.md':'Exact accepted draft'}});
  await waitFor(()=>expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled());
});

it('clears the previous run bytes and loads the newly started accepted artifact',async () => {
  const accepted=(id:string,hash:string)=>({id,status:'accepted',reason:'Accepted',attention_id:null,deadline_at:new Date().toISOString(),
    admission:{task_id:`task-${id}`},contract:{goal:`Draft ${id}`,budget:{max_attempts:3,max_cost_microusd:1000},criteria:[]},
    attempts:[{number:1,artifact:{hash,files:{'draft.md':hash}},verdicts:{},decided:true}],decisions:[],
    spend:{settled_microusd:0,reserved_microusd:0},routing:{artifact_hash:hash,route:'human_review',reason:'Human review'},owner_approved:null,handoff:null});
  const old=accepted('old','old-hash'), next=accepted('new','new-hash');
  let started=false, release:(value:unknown)=>void=()=>{};
  mockApi(async (path,body)=>{
    if(path==='/profiles')return profiles;
    if(path==='/preview')return preview;
    if(path==='/runs'&&body){started=true;return next;}
    if(path.endsWith('/artifacts/old-hash'))return {files:{'draft.md':'Previous run bytes'}};
    if(path.endsWith('/artifacts/new-hash'))return new Promise(r=>{release=r;});
    return {runs:started?[next,old]:[old]};
  });
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/Draft old/}));
  await screen.findByText('Previous run bytes');
  await fireEvent.click(screen.getByRole('button',{name:'New draft'}));
  await fireEvent.input(screen.getByLabelText('What should the draft accomplish?'),{target:{value:'New goal'}});
  await fireEvent.input(screen.getByLabelText('What makes the result good enough?'),{target:{value:'Clear answer'}});
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await fireEvent.click(await screen.findByRole('button',{name:'Start local draft'}));
  await waitFor(()=>expect(request).toHaveBeenCalledWith('/runs/new/artifacts/new-hash',undefined,expect.any(AbortSignal)));
  expect(screen.queryByText('Previous run bytes')).not.toBeInTheDocument();
  expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeDisabled();
  release({files:{'draft.md':'New reviewed bytes'}});
  await screen.findByText('New reviewed bytes');
  await waitFor(()=>expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled());
});

it('defaults to a bounded review inbox, searches proposals and hides policy editors',async () => {
  const proposals=Array.from({length:47},(_,index)=>({
    proposal:{goal:`Deliverable ${index+1}`,sources:{},budget:{max_cost_microusd:100,max_attempts:2,max_wall_clock_secs:60},criteria:[],critic_profiles:[],actor_profile:'actor'},
    proposal_hash:`hash-${index}`,origin:{request_id:`request-${index}`,agent_id:index%2?'writer':'researcher',task_id:`task-${index}`,execution_id:'execution'}
  }));
  mockApi(async path=>path==='/profiles'?profiles:path==='/proposals'?{proposals}:{runs:[]});
  render(Page);
  await screen.findByRole('button',{name:'researcher: Deliverable 1'});
  expect(screen.queryByLabelText('What should the draft accomplish?')).not.toBeInTheDocument();
  expect(screen.getAllByRole('button',{name:/^(writer|researcher): Deliverable/})).toHaveLength(20);
  await fireEvent.click(screen.getByRole('button',{name:'Next page'}));
  await screen.findByRole('button',{name:'researcher: Deliverable 21'});
  await fireEvent.input(screen.getByLabelText('Search work'),{target:{value:'Deliverable 47'}});
  await screen.findByRole('button',{name:'researcher: Deliverable 47'});
  expect(screen.getAllByRole('button',{name:/^(writer|researcher): Deliverable/})).toHaveLength(1);
  expect(request).toHaveBeenCalledWith(expect.stringContaining('q=Deliverable+47'),undefined,expect.any(AbortSignal));
  expect(request).not.toHaveBeenCalledWith('/guidance-policies',undefined,expect.anything());
  expect(request).not.toHaveBeenCalledWith('/campaigns',undefined,expect.anything());
});

it('attaches full referenced content to the contract and invalidates the previous preview',async () => {
  await prepare();
  const hit={provider:'local',relative_path:'brief.md',source_ref:'notes:local:brief.md',title:'Project fact sheet',modified_at_ms:1,matched_in_title:true,match_count:1,matches:[]};
  vi.mocked(findReferences).mockResolvedValue({hits:[hit],scanned_notes:1,scan_truncated:false,query_terms:['brief'],more_available:false});
  vi.mocked(readReference).mockResolvedValue({id:hit.source_ref,title:hit.title,origin:hit.source_ref,content:'Verified source facts'});
  await fireEvent.input(screen.getByLabelText('Find a reference'),{target:{value:'brief'}});
  await fireEvent.click(screen.getByRole('button',{name:'Search notes'}));
  await fireEvent.click(await screen.findByRole('button',{name:/Project fact sheet/}));
  await screen.findByRole('button',{name:'Remove reference Project fact sheet'});
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
  await fireEvent.click(screen.getByRole('button',{name:'Review contract and reservation'}));
  await screen.findByRole('button',{name:'Start local draft'});
  expect(request).toHaveBeenLastCalledWith('/preview',expect.objectContaining({sources:{'notes:local:brief.md':'Reference: Project fact sheet\nOrigin: notes:local:brief.md\n\nVerified source facts'}}),expect.any(AbortSignal));
  await fireEvent.click(screen.getByRole('button',{name:'Remove reference Project fact sheet'}));
  expect(screen.queryByRole('button',{name:'Start local draft'})).not.toBeInTheDocument();
});

it.each(['workspace','draft'])('discards a pending reference attachment when the %s changes',async (change) => {
  await prepare();
  const hit={provider:'local',relative_path:'private.md',source_ref:'notes:local:private.md',title:'Private reference',modified_at_ms:1,matched_in_title:true,match_count:1,matches:[]};
  vi.mocked(findReferences).mockResolvedValue({hits:[hit],scanned_notes:1,scan_truncated:false,query_terms:[],more_available:false});
  let release:(value:Awaited<ReturnType<typeof readReference>>)=>void=()=>{};
  vi.mocked(readReference).mockImplementation(()=>new Promise(resolve=>{release=resolve;}));
  await fireEvent.input(screen.getByLabelText('Find a reference'),{target:{value:'private'}});
  await fireEvent.click(screen.getByRole('button',{name:'Search notes'}));
  await fireEvent.click(await screen.findByRole('button',{name:/Private reference/}));
  expect(screen.getByRole('button',{name:'Start local draft'})).toBeDisabled();
  expect(screen.getByRole('button',{name:'Review contract and reservation'})).toBeDisabled();
  if(change==='workspace')scopeIdentityStore.observe('another-owner','another-workspace');
  else await fireEvent.click(screen.getByRole('button',{name:'New draft'}));
  release({id:hit.source_ref,title:hit.title,origin:hit.source_ref,content:'Private bytes'});
  await waitFor(()=>expect(screen.getByLabelText('Find a reference')).toHaveValue(''));
  expect(screen.queryByText('Private bytes')).not.toBeInTheDocument();
  expect(screen.queryByRole('button',{name:'Remove reference Private reference'})).not.toBeInTheDocument();
});

it('shows unverifiable stored runs without approval actions or invented spend',async () => {
  vi.mocked(request).mockImplementation(async path=>path==='/profiles'?profiles:path.startsWith('/catalog?')?
    {items:[{kind:'unavailable',item:{id:'damaged-run',reason:'Stored run could not be verified. Recovery and approval remain blocked.'}}],pagination:{total:1,limit:20,offset:0,has_more:false},counts:{attention:1},agents:[]}:{runs:[]});
  render(Page);
  await screen.findByText('damaged-run');
  expect(screen.getByText('Stored run unavailable')).toBeInTheDocument();
  expect(screen.queryByText(/spent/)).not.toBeInTheDocument();
  expect(screen.queryByRole('button',{name:/Approve exact draft/})).not.toBeInTheDocument();
});

it('opens the accepted artifact even when it comes from an earlier attempt',async()=>{
  const draft=(number:number,hash:string)=>({number,artifact:{hash,files:{'draft.md':hash}},verdicts:{},decided:true});
  const run={id:'champion',status:'accepted',reason:'Earlier artifact satisfies the contract',attention_id:null,started_at:new Date().toISOString(),deadline_at:new Date().toISOString(),
    admission:{task_id:'task'},contract:{goal:'Accepted earlier output',budget:{max_attempts:3,max_cost_microusd:1000},criteria:[]},attempts:[draft(1,'accepted'),draft(2,'later')],decisions:[],spend:{settled_microusd:0,reserved_microusd:0},
    routing:{artifact_hash:'accepted',route:'human_review',reason:'Owner review'},owner_approved:null,handoff:null};
  mockApi(async path=>path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:path.endsWith('/artifacts/accepted')?{files:{'draft.md':'Accepted earlier bytes'}}:{runs:[run]});
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:/Accepted earlier output/}));
  await screen.findByText('Accepted earlier bytes');
  expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled();
  expect(request).not.toHaveBeenCalledWith('/runs/champion/artifacts/later',undefined,expect.anything());
});

it('keeps a capture awaiting its own review in the inbox and opens its exact campaign',async()=>{
  const run={id:'capture-run',status:'accepted',reason:'Captured draft',attention_id:null,started_at:new Date().toISOString(),deadline_at:new Date().toISOString(),
    admission:{task_id:'task'},contract:{goal:'Capture needing owner review',budget:{max_attempts:3,max_cost_microusd:1000},criteria:[]},attempts:[],decisions:[],spend:{settled_microusd:0,reserved_microusd:0},
    routing:{artifact_hash:'hash',route:'human_review',reason:'Owner review'},owner_approved:true,handoff:{artifact_hash:'hash',kind:'capture'},
    capture_handoff:{target:{campaign_id:'exact-campaign',version_hash:'version',identity_id:'identity'},capture:{status:'awaiting_review'},available:true}};
  mockApi(async path=>path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:path==='/campaigns'?{campaigns:[],identities:[]}:{runs:[run]});
  render(Page);
  await fireEvent.click(await screen.findByRole('button',{name:'Capture needing owner review · Review capture'}));
  await fireEvent.click(screen.getByRole('button',{name:'Review capture policy'}));
  await waitFor(()=>expect(request).toHaveBeenCalledWith(expect.stringMatching(/^\/campaigns\?limit=10&offset=0/),undefined,expect.any(AbortSignal)));
  expect(screen.queryByRole('button',{name:'Approve exact draft'})).not.toBeInTheDocument();
});

it('pages capture reviews and opens the bound bytes before returning to approve',async()=>{
  const originalUrl=window.location.href;
  window.history.replaceState(null,'','/work?view=quality&campaign=campaign');
  const capture=(i:number)=>({id:`capture-${i}`,binding_hash:`binding-${i}`,status:'awaiting_review',reason:'Review required',audit_required:false,
    binding:{run_id:'capture-run',artifact_hash:'hash',identity_id:'identity',recipient:`Recipient ${i}`,task_id:'task',artifact_files:{'draft.md':'hash'}}});
  const view={state:{campaign:{id:'campaign',version_hash:'version',revoked_at:null,spec:{name:'Local capture policy',targets:[],maximum_acts:100,expires_at:new Date(Date.now()+60000).toISOString()}},captures:Object.fromEntries(Array.from({length:41},(_,i)=>[`capture-${i}`,capture(i)])),simulated_clean_approvals:0,simulated_clean_approvals_by_identity:{},production_clean_approvals:0,automatic_capture_suspended:false,live_dispatch_enabled:false},targets:[]};
  const run={id:'capture-run',status:'accepted',reason:'Captured',attention_id:null,started_at:new Date().toISOString(),deadline_at:new Date().toISOString(),
    admission:{task_id:'task'},contract:{goal:'Exact capture output',budget:{max_attempts:3,max_cost_microusd:1000},criteria:[]},attempts:[{number:1,artifact:{hash:'hash',files:{'draft.md':'hash'}},verdicts:{},decided:true}],decisions:[],spend:{settled_microusd:0,reserved_microusd:0},routing:{artifact_hash:'hash',route:'human_review',reason:'Owner review'},owner_approved:true,handoff:{artifact_hash:'hash',kind:'capture'}};
  mockApi(async path=>path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:path==='/campaigns'?{campaigns:[view],identities:[]}:path==='/runs/capture-run'?run:path.endsWith('/artifacts/hash')?{files:{'draft.md':'Exact capture bytes'}}:{runs:[run]});
  try {
    render(Page);
    await screen.findByText('Recipient 0');
    expect(screen.getAllByRole('button',{name:'Inspect bound artifact'})).toHaveLength(20);
    expect(screen.getAllByRole('button',{name:'Approve exact capture'})[0]).toBeDisabled();
    await fireEvent.change(screen.getByLabelText('Capture status'),{target:{value:'captured'}});
    await waitFor(()=>expect(request).toHaveBeenCalledWith('/campaigns/campaign/captures?limit=20&offset=0&status=captured',undefined,expect.any(AbortSignal)));
    expect(screen.queryByRole('button',{name:'Inspect bound artifact'})).not.toBeInTheDocument();
    await fireEvent.change(screen.getByLabelText('Capture status'),{target:{value:'awaiting_review'}});
    await screen.findByText('Recipient 0');
    await fireEvent.click(screen.getByRole('button',{name:'Next page'}));
    await screen.findByText('Recipient 20');
    await fireEvent.click(screen.getAllByRole('button',{name:'Inspect bound artifact'})[0]);
    await screen.findByText('Exact capture bytes');
    await fireEvent.click(screen.getByRole('button',{name:'Return to capture review'}));
    await waitFor(()=>expect(screen.getAllByRole('button',{name:'Approve exact capture'})[0]).toBeEnabled());
    expect(screen.getByLabelText('Search campaigns')).toHaveValue('campaign');
  }finally{window.history.replaceState(null,'',originalUrl);}
});

it('polls only the selected active run between catalog refreshes',async () => {
  const run={id:'active',status:'reviewing',reason:null,attention_id:null,started_at:new Date().toISOString(),deadline_at:new Date().toISOString(),
    admission:{task_id:'task'},contract:{goal:'In-progress work',budget:{max_attempts:3,max_cost_microusd:1000},criteria:[]},
    attempts:[],decisions:[],spend:{settled_microusd:0,reserved_microusd:0},routing:null,owner_approved:null,handoff:null};
  mockApi(async path=>path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:path==='/runs/active'?run:{runs:[run]});
  let tick:()=>Promise<void>=async()=>{};
  vi.spyOn(document,'hidden','get').mockReturnValue(false);
  const originalInterval=globalThis.setInterval;
  vi.spyOn(globalThis,'setInterval').mockImplementation((handler,ms,...args)=>{if(ms===3000){tick=handler as ()=>Promise<void>;return 123 as unknown as ReturnType<typeof setInterval>;}return originalInterval(handler,ms,...args);});
  render(Page);
  await fireEvent.click(screen.getByRole('button',{name:/^Runs/}));
  await fireEvent.click(await screen.findByRole('button',{name:/In-progress work/}));
  try {
    await tick();await tick();
    expect(vi.mocked(request).mock.calls.filter(call=>call[0].startsWith('/catalog?'))).toHaveLength(2);
    expect(vi.mocked(request).mock.calls.filter(call=>call[0]==='/runs/active')).toHaveLength(3);
  }finally{vi.restoreAllMocks();}
});

it('loads the accepted artifact when an admitted run gains its first attempt during polling',async()=>{
  const admitted={id:'new-poll',status:'admitted',reason:null,attention_id:null,started_at:new Date().toISOString(),deadline_at:new Date().toISOString(),
    admission:{task_id:'task'},contract:{goal:'New polling draft',budget:{max_attempts:2,max_cost_microusd:1000},criteria:[]},
    attempts:[],decisions:[],spend:{settled_microusd:0,reserved_microusd:0},routing:null,owner_approved:null,handoff:null};
  const accepted={...admitted,status:'accepted',attempts:[{number:1,artifact:{hash:'sealed',files:{'draft.md':'sealed'}},verdicts:{},decided:true}],routing:{artifact_hash:'sealed',route:'human_review',reason:'Owner review'}};
  let live=admitted as typeof admitted|typeof accepted;
  mockApi(async path=>path==='/profiles'?profiles:path==='/runs/new-poll'?live:path.endsWith('/artifacts/sealed')?{files:{'draft.md':'Sealed polling bytes'}}:{runs:[live]});
  let tick:()=>Promise<void>=async()=>{};
  vi.spyOn(document,'hidden','get').mockReturnValue(false);
  const originalInterval=globalThis.setInterval;
  vi.spyOn(globalThis,'setInterval').mockImplementation((handler,ms,...args)=>{if(ms===3000){tick=handler as ()=>Promise<void>;return 123 as unknown as ReturnType<typeof setInterval>;}return originalInterval(handler,ms,...args);});
  try {
    render(Page);
    await fireEvent.click(screen.getByRole('button',{name:/^Runs/}));
    await fireEvent.click(await screen.findByRole('button',{name:/New polling draft/}));
    live=accepted;await tick();
    await screen.findByText('Sealed polling bytes');
    expect(screen.getByRole('button',{name:'Approve exact draft'})).toBeEnabled();
  }finally{vi.restoreAllMocks();}
});

it('uses server totals and preserves an open full run when changing pages',async()=>{
  const run={id:'independent',status:'reviewing',reason:null,attention_id:null,started_at:new Date().toISOString(),deadline_at:new Date().toISOString(),
    admission:{task_id:'task'},contract:{goal:'Full run stays open',budget:{max_attempts:3,max_cost_microusd:1000},criteria:[]},attempts:[],decisions:[],spend:{settled_microusd:0,reserved_microusd:0},routing:null,owner_approved:null,handoff:null};
  const summary={id:run.id,goal:'Server result',task_id:'task',started_at:run.started_at,status:'reviewing',group:'active',label:'reviewing',attempt_count:0,max_attempts:3,spend:run.spend};
  vi.mocked(request).mockImplementation(async path=>{
    if(path==='/profiles')return profiles;
    if(path==='/runs/independent')return run;
    const offset=Number(new URL(path,'http://fixture').searchParams.get('offset'));
    return {items:[{kind:'run',item:{...summary,id:offset?'other':summary.id,goal:offset?'Another page':summary.goal}}],pagination:{total:1001,limit:20,offset,has_more:true},counts:{active:1001},agents:[]};
  });
  render(Page);
  await fireEvent.click(screen.getByRole('button',{name:/^Runs/}));
  await fireEvent.click(await screen.findByRole('button',{name:'Server result · reviewing'}));
  await screen.findByRole('heading',{name:'Full run stays open'});
  expect(screen.getByText('1001 items')).toBeInTheDocument();
  await fireEvent.click(screen.getByRole('button',{name:'Next page'}));
  await screen.findByRole('button',{name:'Another page · reviewing'});
  expect(screen.getByRole('heading',{name:'Full run stays open'})).toBeInTheDocument();
  expect(request).toHaveBeenCalledWith(expect.stringContaining('offset=20'),undefined,expect.any(AbortSignal));
  expect(vi.mocked(request).mock.calls.filter(c=>c[0]==='/runs/independent')).toHaveLength(1);
});

it('ignores an older server search response after a newer search completes',async()=>{
  let releaseAlpha!:(value:any)=>void,releaseBeta!:(value:any)=>void;
  const result=(goal:string)=>({items:[{kind:'proposal',item:{goal,origin:{request_id:goal,agent_id:'agent',task_id:'task',execution_id:'execution'},reference_count:0,maximum_cost_microusd:100}}],pagination:{total:1,limit:20,offset:0,has_more:false},counts:{brief:1},agents:['agent']});
  vi.mocked(request).mockImplementation(async path=>{
    if(path==='/profiles')return profiles;
    const q=new URL(path,'http://fixture').searchParams.get('q');
    if(q==='alpha')return new Promise(resolve=>releaseAlpha=resolve);
    if(q==='beta')return new Promise(resolve=>releaseBeta=resolve);
    return {...result('Initial brief')};
  });
  render(Page);await screen.findByRole('button',{name:'agent: Initial brief'});
  await fireEvent.input(screen.getByLabelText('Search work'),{target:{value:'alpha'}});
  await waitFor(()=>expect(releaseAlpha).toBeTypeOf('function'));
  await fireEvent.input(screen.getByLabelText('Search work'),{target:{value:'beta'}});
  await waitFor(()=>expect(releaseBeta).toBeTypeOf('function'));
  releaseBeta(result('New server brief'));
  await screen.findByRole('button',{name:'agent: New server brief'});
  releaseAlpha(result('Stale server brief'));
  await new Promise(resolve=>setTimeout(resolve,20));
  expect(screen.queryByRole('button',{name:'agent: Stale server brief'})).not.toBeInTheDocument();
  expect(screen.getByRole('button',{name:'agent: New server brief'})).toBeInTheDocument();
});

it('requests task pages and searches across the server catalog',async()=>{
  vi.mocked(request).mockImplementation(async path=>{
    if(path==='/profiles')return profiles;
    const query=new URL(path,'http://fixture').searchParams;
    if(path.startsWith('/work?')){
      const offset=Number(query.get('offset')),title=query.get('q')?'Matched elsewhere':offset?'Second task page':'First task page';
      return {goal_id:null,targets:[{task_id:title,context_hash:'context',title,agent_id:'worker',goal_id:null,due_date:null,task_status:'scheduled',policy:null,runs:[{id:'latest/run&42'}],run_count:42}],pagination:{total:query.get('q')?1:41,limit:20,offset,has_more:offset===0}};
    }
    return {items:[],pagination:{total:0,limit:20,offset:0,has_more:false},counts:{},agents:[]};
  });
  render(Page);await fireEvent.click(screen.getByRole('button',{name:'Tasks'}));
  await screen.findByRole('link',{name:'First task page'});
  expect(screen.getByText(/42 runs/)).toBeInTheDocument();
  expect(screen.getByRole('link',{name:'First task page'})).toHaveAttribute('href','/work?selected=First+task+page');
  expect(screen.getByRole('link',{name:'Latest run'})).toHaveAttribute('href','/work?view=quality&run=latest%2Frun%2642');
  await fireEvent.click(screen.getByRole('button',{name:'Next page'}));
  await screen.findByRole('link',{name:'Second task page'});
  await fireEvent.input(screen.getByLabelText('Search tasks'),{target:{value:'requested phrase'}});
  await screen.findByRole('link',{name:'Matched elsewhere'});
  expect(request).toHaveBeenCalledWith('/work?limit=20&offset=0&q=requested+phrase',undefined,expect.any(AbortSignal));
});

it('requests independent pages of agent guidance proposals',async()=>{
  vi.mocked(request).mockImplementation(async path=>{
    if(path==='/profiles')return profiles;
    const offset=Number(new URL(path,'http://fixture').searchParams.get('offset'));
    if(path.startsWith('/guidance-proposals?'))return {proposals:[{candidate_id:offset?'next':'first',guidance:{title:offset?'Next guidance':'First guidance',workflow:[],decision_points:[],verification:[],failure_modes:[]},training_run_ids:['training/run&1'],agent_id:'worker',task_id:'source/task',execution_id:null}],pagination:{total:11,limit:10,offset,has_more:offset===0}};
    if(path.startsWith('/guidance-policies?'))return {policies:[],pagination:{total:0,limit:10,offset:0,has_more:false}};
    return {items:[],pagination:{total:0,limit:20,offset:0,has_more:false},counts:{},agents:[]};
  });
  render(Page);
  await fireEvent.click(screen.getByRole('button',{name:'Policies & learning'}));
  await fireEvent.click(screen.getByRole('button',{name:'Reusable guidance'}));
  await fireEvent.click(screen.getByRole('button',{name:'Load agent guidance proposals'}));
  await screen.findByText('First guidance · proposed by worker');
  expect(screen.getByRole('link',{name:'Source task'})).toHaveAttribute('href','/work?view=quality&task=source%2Ftask');
  expect(screen.getByRole('link',{name:'Training run training/run&1'})).toHaveAttribute('href','/work?view=quality&run=training%2Frun%261');
  await fireEvent.click(screen.getByRole('button',{name:'Next page'}));
  await screen.findByText('Next guidance · proposed by worker');
  expect(request).toHaveBeenCalledWith('/guidance-proposals?limit=10&offset=10',undefined,expect.any(AbortSignal));
});

it("shows the owning task's pending ask in place, with its review link", async () => {
  const { ingestPendingHitlLineForTest, teardownPendingHitlBridge } = await import('$lib/stores/pendingHitlStore');
  ingestPendingHitlLineForTest(JSON.stringify({
    event_type:'HitlRequested',
    data:{
      correlation_id:'review-ask',request_id:'review-ask',source:'user_request',input_type:'choice',
      prompt:'Approve the accepted draft?',
      input_schema:{request_type:'relentless.accepted_review',options:[{id:'approve',label:'Approve draft'},{id:'reject',label:'Reject draft'}],context:{review_href:'/work?view=quality&run=review-run',review_label:'Review draft'}},
      scope:{principal:'owner',workspace:'work',task_id:'task review'}
    }
  }));
  try {
    const run = {
      id:'review-run',status:'waiting_owner',reason:'Owner review',attention_id:'review-ask',
      deadline_at:new Date().toISOString(),admission:{task_id:'task review'},
      contract:{goal:'A draft awaiting owner review',budget:{max_attempts:2,max_cost_microusd:1000},criteria:[]},
      attempts:[],decisions:[],spend:{settled_microusd:0,reserved_microusd:0},routing:null,owner_approved:null,handoff:null
    };
    mockApi(async path => path==='/profiles'?profiles:path==='/proposals'?{proposals:[]}:{runs:[run]});
    render(Page);
    await fireEvent.click(await screen.findByRole('button',{name:/A draft awaiting owner review/}));
    const dock = await screen.findByTestId('composer-ask-dock');
    expect(dock).toHaveTextContent('Approve the accepted draft?');
    expect(dock).toHaveTextContent('Approve draft');
    expect(dock).toHaveTextContent('Reject draft');
    expect(screen.getByRole('link',{name:'Review draft →'})).toHaveAttribute('href','/work?view=quality&run=review-run');
  } finally { teardownPendingHitlBridge(); }
});
