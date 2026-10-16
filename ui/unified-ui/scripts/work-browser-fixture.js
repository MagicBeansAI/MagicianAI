/** Reviewed browser-only fixture. Every API request stays in this tab's memory. */
(() => {
	localStorage.setItem('magician:auth:scoped-bearer', 'unified-work-fixture');
	localStorage.setItem('magican-theme', localStorage.getItem('magican-theme') || 'sorbet');
	const originalFetch = window.fetch.bind(window);
	const now = new Date().toISOString();
	const task = (id,title,status='pending') => ({id,title,description:`Complete ${title.toLowerCase()} and explain the result.`,status,priority:'p2',due_date:null,tags:[],agent_id:'forge',ui_thread_id:'general',created_by:'user',lifecycle:'persistent',output_mode:'accumulate',approved:true,is_blocked:false,has_plan:true,plan_status:'approved',pending_questions:[],active_root_execution_id:status==='running'?`run-${id}`:null,latest_root_execution_id:status==='pending'?null:`run-${id}`,last_completed_root_execution_id:status==='completed'?`run-${id}`:null,created_at:now,updated_at:now});
	const tasks = [task('redirect','Fix onboarding redirect','running'),task('brief','Prepare competitive positioning brief'),task('briefing','Executive briefing','completed')];
	const agents = [{definition:{agent_id:'forge',name:'Forge',description:'Builds and verifies product changes.',kind:'Personal',is_primary:true,disabled:false,tools:['bash','editor'],excluded_tools:[],delegation_targets:['sleuth'],harness:{program_section:'Engineering'},autonomous_config:{schedule:'0 9 * * *',focus_areas:[{name:'Launch readiness',description:'Keep launch work moving.',priority:'high',program:'launch_readiness.md'}]}},status:'running',version:1,etag:'fixture-forge',created_at:now,updated_at:now},{definition:{agent_id:'sleuth',name:'Sleuth',description:'Research and evidence.',kind:'Worker',is_primary:false,disabled:false,tools:['web_fetch'],excluded_tools:[],delegation_targets:[]},status:'idle',version:1,etag:'fixture-sleuth',created_at:now,updated_at:now}];
	const programs = [{name:'launch_readiness.md',title:'Launch readiness',content:'# Launch readiness\n\nShip onboarding with verified redirects and a clear competitive brief.\n\n## Stop conditions\n\nAsk the owner before external delivery.\n\n## Missions (CEO)\n\n- **Fix onboarding redirect** (high)\n- **Prepare competitive positioning brief** (medium)',missions_section:'- **Fix onboarding redirect** (high)',history:[{snapshot:'launch_readiness-previous.md',modified:now}]}];
	const emptyPagination = {total:0,limit:20,offset:0,has_more:false};
	const plan = id => ({plan_id:`plan-${id}`,status:'approved',version:1,created_at:now,updated_at:now,plan_graph:{steps:['Inspect the current behavior','Implement the correction','Verify the result'].map((task,index)=>({id:`step-${index+1}`,task,tool:index===1?'editor':'bash',parameters:{},expected_outputs:[],confidence:0.9,metadata:{}})),edges:[{from:'step-1',to:'step-2',reason:'Inspect before editing'},{from:'step-2',to:'step-3',reason:'Verify the correction'}],unresolved_inputs:[],confidence:0.9,provenance:{strategy:'fixture',generator:null,notes:null}}});
	const panel = t => ({default_tab:t.status==='running'?'run':'plan',overview:{task_id:t.id,execution_id:t.latest_root_execution_id,principal:'fixture-owner',workspace:'fixture-work',ui_thread_id:t.ui_thread_id,title:t.title,description:t.description,status:t.status,priority:t.priority,assigned_agent_id:t.agent_id,active_agent_id:t.agent_id,has_plan:true,progress:t.status==='running'?35:null,current_step:'Implement the correction',created_at:Date.now(),updated_at:Date.now()},run:{summary:null,responsibility:null,pending_questions:[],needs_attention:[],recent_activity:[],activity_log:[]},output:{result:null,deliveries:[],recent_runs:[]},debug:{selected_execution:null,taskplan:null,timeline:[],observations:[],shell_entries:[]}});
	const counts = {requests:0,approvals:0,escalations:0,needs_action:0,failed:0,running:1};
	let runtimeEnabled = true;
	const response = (body,status=200) => new Response(JSON.stringify(body),{status,headers:{'content-type':'application/json'}});
	window.__workFixture = {tasks,agents,programs,calls:[],unknown:[]};
	window.fetch = async (input,init={}) => {
		const url = new URL(typeof input==='string'?input:input instanceof URL?input.href:input.url,location.href);
		if (!url.pathname.startsWith('/api/') && url.pathname!=='/health') return originalFetch(input,init);
		const path=url.pathname,method=init.method||'GET';
		window.__workFixture.calls.push({path,method,body:init.body||null});
		if(path==='/health')return response({status:'ok',ready:true});
		if(path.endsWith('/auth/session'))return response({principal:'fixture-owner',workspace:'fixture-work',identity:{name:'fixture-owner',display_name:'Fixture owner'},workspaces:[{id:'fixture-work',display_name:'Fixture work',is_default:true}]});
		if(path.endsWith('/ui/preferences'))return response({theme:localStorage.getItem('magican-theme'),saved:true});
		if(path.endsWith('/startup'))return response({ready:true,status:'ready',phase:'ready'});
		if(path.endsWith('/agents')) {
   if(method==='POST') {const body=JSON.parse(init.body||'{}');const definition=body.definition||body;const created={definition:{...definition,agent_id:definition.agent_id||definition.name.toLowerCase().replace(/[^a-z0-9]+/g,'-')},version:1,etag:'fixture-created',created_at:now,updated_at:now};agents.push(created);return response(created,201);}
   return response({agents,system_agents:[],total_count:agents.length});
  }
  if(path.endsWith('/skills'))return response({skills:[{name:'bash',kind:'tool-pack'},{name:'editor',kind:'tool-pack'}]});
  if(path.includes('/ui-threads/'))return response({thread:{id:decodeURIComponent(path.split('/').at(-1)),name:'Launch planning',principal:'fixture-owner',workspace:'fixture-work',created_at:now,updated_at:now}});
  if(path.endsWith('/ui-threads'))return response({threads:[{id:'general',name:'General',principal:'fixture-owner',workspace:'fixture-work'},{id:'launch',name:'Launch planning',principal:'fixture-owner',workspace:'fixture-work'}]});
  const tierMatch=/^\/api\/magician\/v2\/agents\/([^/]+)\/memory\/([^/]+)$/.exec(path);
  if(tierMatch && tierMatch[2]!=='consolidation-health') return response({agent_id:tierMatch[1],tier:{name:tierMatch[2],scope:tierMatch[2]==='goal'?'agent_goal':'agent',description:'Reviewed launch evidence and operating context.',render:{format:'key_value',template:''}},goal_id:url.searchParams.get('goal_id')||'launch',available_goal_ids:['launch','verification'],shared:false,renderer:'key_value',data:{tier_name:tierMatch[2],fields:{notes:'Keep the full launch evidence, task links and owner decisions visible in compact views. '.repeat(8),verified_outputs:['redirect report','launch checklist']},last_updated:now},consolidation_rules:[],shared_with_agents:[]});

  if (path.endsWith('/agents/forge/harness-overview')) return response({context:{program:{relative_path:'program.md',section:'Engineering'}},evaluation:{active_program_state:{program:{relative_path:'program.md',section:'Engineering'},current_phase:'Verify launch',current_step:'Review onboarding evidence',updated_at:now,update_reason:'Latest cycle completed the redirect check'},focus_areas:[],program_metrics:{documents:[{relative_path:'launch_readiness.md',metrics:[{metric_id:'verified_launch_checks',current:3,target:4,comparator:'>=',met:false}]}]}},system_status:{agents:[]},recent_proposals:[]});

		const agentMatch = /^\/api\/magician\/v2\/agents\/([^/]+)$/.exec(path);
		if(agentMatch) {
   const agent=agents.find(a=>a.definition.agent_id===decodeURIComponent(agentMatch[1]));
   if(!agent)return response({error:'Agent not found'},404);
   if(method==='PUT') {const headers=new Headers(init.headers);if(headers.get('if-match')!==agent.etag)return response({error:'Definition changed'},412);agent.definition=JSON.parse(init.body||'{}');agent.etag='fixture-updated';agent.version+=1;}
   return response(agent);
  }
		if(path.endsWith('/system-agents'))return response([]);
		if(path.endsWith('/harness/status'))return response({enabled:true,effective_source:'config'});
		if(path.endsWith('/harness/runtime')) {
			if(method==='PUT') runtimeEnabled = JSON.parse(init.body || '{}').enabled === true;
			return response({enabled:runtimeEnabled,paused:!runtimeEnabled,configured_paused:!runtimeEnabled,env_override:null,effective_source:'config',changed:method==='PUT',warnings:[]});
		}
  if(path==='/api/magician/v2/observe/catch-up')return response({status:{policy:{schema_version:1,revision:1,enabled:true,lookback_days:7,max_items_per_source:100,max_total_items:500,max_duration_minutes:15},phase:'completed',boot_id:'fixture-boot',boot_started_at_ms:Date.now(),admitted_items:12,processed_items:12,reserved_items:0,remaining_items:0,sources:[{source_id:'fixture',display_name:'Launch evidence',item_unit:'items',replay_mode:'checkpointed_replay',admitted:12,processed:12,runs:1,failures:0,limitation:'Fixture-backed replay'}]},warnings:[],options:{lookback_days:[1,7,14],max_items_per_source:[100,500],max_total_items:[500,1000],max_duration_minutes:[15,30]}});
  if(path.startsWith('/api/magician/v2/resource-authority/')) {
   const endpoint=path.split('/resource-authority/')[1];
   if(endpoint==='ceilings')return response({ceilings:[{id:'fixture-ceiling',commodity:'USD',ceiling:100,relaxation:0,period:'daily',carryover:{type:'none'},period_start:now,spent_in_period:12,reserved_in_period:3,remaining_in_period:85}]});
   if(endpoint==='tokens')return response({tokens:[{id:'fixture-token',issued_by:'fixture-owner',issued_to:'forge',commodity:'USD',ceiling:25,period:'daily',carryover:{type:'none'},period_start:now,spent_in_period:12,remaining_in_period:13,burn_rate_per_day:2,status:'active',conditions:[],created_at:now}]});
   if(endpoint==='ledger')return response({entries:[{id:'fixture-entry',timestamp:now,accrual_date:now,reference:'Launch verification',agent_id:'forge',entries:[{account:'spent',amount:12,commodity:'USD'}],metadata:{}}],total_count:1});
   if(endpoint==='freeze')return response({frozen:false});
   if(endpoint==='reservations')return response({reservations:[{id:'fixture-reservation',token_id:'fixture-token',commodity:'USD',amount:3,agent_id:'forge',created_at:now,max_duration_secs:600,is_stale:false}]});
   if(endpoint==='ledger/balances')return response({balances:[]});
   if(endpoint==='ledger/audit')return response({ok:true,message:'Fixture ledger is balanced'});
  }
		if(path.endsWith('/harness/anomalies'))return response({anomalies:[]});
		if(/^\/api\/magician\/v2\/agents\/[^/]+\/trigger$/.test(path)) return response({status:'accepted',execution_id:`fixture-dispatch-${window.__workFixture.calls.length}`});
		if(path.endsWith('/programs'))return response({programs:programs.map(({name,title})=>({name,title}))});
		if(path.startsWith('/api/magician/v2/programs/')) {
			const name=decodeURIComponent(path.split('/')[5]||'');
			const doc=programs.find(p=>p.name===name);
			if(!doc)return response({error:'Program not found'},404);
			if(path.endsWith('/revert'))return response({reverted:true,program:doc.name,restored_from:doc.history[0].snapshot});
			return response(doc);
		}
		if(path==='/api/magician/v3/tasks/internal')return response({tasks:[],pagination:emptyPagination});
		if(path==='/api/magician/v3/tasks') {
			if(method==='POST') {const body=JSON.parse(init.body||'{}');const created=task(`created-${tasks.length+1}`,body.title||body.description?.slice(0,80)||'New work');Object.assign(created,{description:body.description||created.description,agent_id:body.agent_id,has_plan:false,plan_status:null});tasks.push(created);return response({task:created},201);}
			const view=url.searchParams.get('view')||'all';
			const rows=tasks.filter(t=>view==='completed'?t.status==='completed':view==='running'?['running','paused'].includes(t.status):view==='inbox'?t.status==='pending':t.status!=='completed');
			const lane_counts={all:tasks.filter(t=>t.status!=='completed').length,inbox:tasks.filter(t=>t.status==='pending').length,today:0,overdue:0,running:tasks.filter(t=>['running','paused'].includes(t.status)).length,completed:tasks.filter(t=>t.status==='completed').length};
			return response({tasks:rows,pagination:{total:rows.length,limit:25,offset:0,has_more:false},lane_counts,view});
		}
		const taskMatch=/^\/api\/magician\/v3\/tasks\/([^/]+)(.*)$/.exec(path);
		if(taskMatch) {
			const t=tasks.find(t=>t.id===decodeURIComponent(taskMatch[1]));if(!t)return response({error:'Task not found'},404);
			const suffix=taskMatch[2];
			if(suffix==='/plan')return response({plan:plan(t.id)});
			if(suffix==='/execution-panel')return response(panel(t));
			if(suffix==='/outputs')return response({outputs:{outputs:[]}});
			if(suffix==='/execute'){t.status='running';t.active_root_execution_id=`run-${t.id}`;return response({execution_id:t.active_root_execution_id,status:'running'});}
			if(!suffix)return response({task:t});
		}
		if(/\/executions\/[^/]+\/(pause|resume|cancel|steer|control-state)$/.test(path)) {
			const id=path.split('/').at(-2),t=tasks.find(t=>`run-${t.id}`===id),action=path.split('/').at(-1);
			if(t&&action==='pause')t.status='paused';if(t&&action==='resume')t.status='running';
			return response({execution_id:id,status:t?.status||'running',can_pause:true,can_resume:true,can_cancel:true});
		}
  if(path==='/api/magician/v3/events')return response({events:[]});
		if(path.endsWith('/feed/attention'))return response({counts,totals:counts,requests:[],approvals:[],escalations:[],failed:[],running:[]});
		if(path.endsWith('/relentless/catalog'))return response({items:[],pagination:emptyPagination,counts:{},agents:[]});
		if(path.endsWith('/relentless/profiles'))return response({profiles:[{name:'draft',model:'Draft model',family:'actor',maximum_request_microusd:1},{name:'review',model:'Review model',family:'critic',maximum_request_microusd:1}],actor_default:'draft',critic_default:'review'});
		if(path.endsWith('/relentless/work'))return response({targets:tasks.map(t=>({task_id:t.id,title:t.title,agent_id:t.agent_id,goal_id:null,due_date:null,task_status:t.status,run_count:0,runs:[],policy:null})),pagination:{...emptyPagination,total:tasks.length},goals:[]});
		if(path.includes('/relentless/task-context/')){const t=tasks.find(t=>t.id===decodeURIComponent(path.split('/').at(-1)));return t?response({task_id:t.id,context_hash:'fixture-context',title:t.title,goal:t.description,agent_id:t.agent_id,ui_thread_id:t.ui_thread_id,can_start:true,reason:null}):response({},404);}
		if(path.endsWith('/relentless/campaigns'))return response({campaigns:[],identities:[],pagination:emptyPagination});
		if(path.includes('/relentless/guidance'))return response({policies:[],candidates:[],pagination:emptyPagination});
		if(path.endsWith('/threads'))return response({threads:[{id:'general',name:'general',principal:'fixture-owner',workspace:'fixture-work',created_at:Date.now(),updated_at:Date.now()}]});
		if(path.includes('/monitors'))return response({monitors:[],items:[],pagination:emptyPagination});
		if(path.includes('/apps/directory'))return response({entries:[],pagination:emptyPagination});
		if(path.startsWith('/api/magician/v2/agents/')) return response({error:'This specialist fixture is unavailable'},503);
		if (!window.__workFixture.unknown.includes(path)) window.__workFixture.unknown.push(path);
		return response({threads:[],active:[],sessions:[],items:[],agents:[],profiles:[],policies:[],pending:[],requests:[],counts:{},status:'ok',enabled:false,available:true});
	};
	class FixtureSocket extends EventTarget {
		static CONNECTING=0;static OPEN=1;static CLOSING=2;static CLOSED=3;
		readyState=0;protocol='';extensions='';bufferedAmount=0;binaryType='blob';
		constructor(url){super();this.url=String(url);setTimeout(()=>{this.readyState=1;this.onopen?.(new Event('open'));this.dispatchEvent(new Event('open'));},20);}
		send(){}close(){this.readyState=3;}
	}
	window.WebSocket=FixtureSocket;
})();
