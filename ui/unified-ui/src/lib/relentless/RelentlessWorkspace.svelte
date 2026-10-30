<script lang="ts">
  import WorkHelp from '$lib/work/WorkHelp.svelte';
  import { WORK_HELP } from '$lib/work/help';
  import { onMount, onDestroy } from 'svelte';
  import { page as routePage } from '$app/stores';
  import { workQualityHref, workTaskHref } from '$lib/work/routes';
  import ServerPager from '$lib/shared/components/ServerPager.svelte';
  import ReferencePicker from '$lib/relentless/ReferencePicker.svelte';
  import { runGroup, runLabel, proposalCriteria, referenceSources, type SourceReference } from '$lib/relentless/workspace';
  import CampaignWorkspace from '$lib/relentless/CampaignWorkspace.svelte';
  import ComposerAskDock from '$lib/magician/chat/ComposerAskDock.svelte';
  import { pendingAsks, pendingAsksForTask } from '$lib/attention/pendingAsks';
  import GuidanceWorkspace from '$lib/relentless/GuidanceWorkspace.svelte';
  import { scopeIdentityStore, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
  import { agentList, refreshAgents } from '$lib/stores/agentStore';
  import { fetchRoutingOverview, type RoutingProfile } from '$lib/stores/modelRoutingStore';
  import ModelSelectionControl from '$lib/llm/ModelSelectionControl.svelte';
  import { routeForSelection, type ModelChoice, type ModelSelection } from '$lib/llm/modelCatalog';
  import { qualityModels, qualitySelection } from './modelChoices';
  import { request, pagePath, requestArtifactFile, terminal, usd, costLimit, countLimit, type CatalogPage, type RunSummary, type Profile, type Run, type Attempt, type ProposedDraft, type TaskContext, type ArtifactFiles, type WorkView, type RunReportView } from '$lib/relentless/api';
  import Icon from '$lib/shared/icons/Icon.svelte';

  let goal = '', source = '', rubric = '', filename = 'draft.md';
  let references:SourceReference[] = [], referencesPending = false, draftRevision = 0;
  let tab:'inbox'|'runs'|'tasks'|'policies' = 'inbox';
  let policyTab:'capture'|'learning' = 'capture';
  let campaignVisited=false, guidanceVisited=false;
  $: if(tab==='policies'&&policyTab==='capture')campaignVisited=true;
  $: if(tab==='policies'&&policyTab==='learning')guidanceVisited=true;
  let formOpen = false, search = '', statusFilter = 'all', inboxFilter = 'all', proposalAgent = '', page = 1;
  const pageSize = 20;
  let previousFilter = '', catalogRefreshed = 0, countsRefreshed = 0, polling = false, mounted=false;
  let catalogItems:CatalogPage['items']=[], total=0, counts:Record<string,number>={}, proposingAgents:string[]=[];
  let lastQuery='', catalogRequest=0, detailRequest=0, queryTimer:ReturnType<typeof setTimeout>|undefined;
  $: filterSignature=JSON.stringify([tab,search,statusFilter,inboxFilter,proposalAgent]);
  $: if(filterSignature!==previousFilter){previousFilter=filterSignature;page=1;}
  $: queryPath=tab==='tasks'?pagePath('/work',page,pageSize,{q:search,goal_id:workGoal}):pagePath('/catalog',page,pageSize,{view:tab==='runs'?'runs':'inbox',q:search,status:tab==='runs'?statusFilter:inboxFilter,agent_id:proposalAgent});
  $: if(mounted&&['inbox','runs','tasks'].includes(tab)&&queryPath!==lastQuery){lastQuery=queryPath;schedulePage();}
  $: pageCount=Math.max(1,Math.ceil(total/pageSize));
  $: visibleItems=catalogItems;
  $: visibleTasks=workView?.targets??[];
  $: taskPageCount=Math.max(1,Math.ceil((workView?.pagination.total??0)/pageSize));

  let actor = '', critic = '', agent = '', thread = 'general';
  $: availableAgents = $agentList.filter(a=>!a.disabled&&!a.configured_disabled).sort((a,b)=>(a.name||a.agent_id).localeCompare(b.name||b.agent_id));
  $: if(!agent&&profiles.length&&!taskContext&&!proposed){const primary=availableAgents.find(a=>a.is_primary);if(primary)agent=primary.agent_id;}
  let attempts = 25, hours = 24, budget = 100, threshold = .8;
  let budgetDefaults={max_attempts:25,max_wall_clock_secs:86400,max_cost_microusd:100000000,plateau_attempts:4294967295};
  let defaultsApplied=false, resetCost=100, resetHours=24, resetAttempts=25;
  let pendingReset:{signature:string;id:string}|null=null;
  let workMode = 'draft', stepsPerAttempt = 1024, callsPerAttempt = 500;
  let completeContract = '';
  let artifactEvidence: {evidence:Record<string,unknown>;content_hash:string;sealed:boolean}|null = null;
  let audioProviders:{provider_id:string;model:string;configuration_hash:string}[] = [];
  let humanReview = 'required', handoffKind = 'internal_draft';
  let unknownReview = 'revise_supported_defects';
  let profiles: Profile[] = [], selected: Run | null = null;
  let catalogModels: ModelChoice[] = [], catalogRoutes: RoutingProfile[] = [];
  $: draftModels = qualityModels(catalogModels, profiles, catalogRoutes);
  $: reviewModels = actorFamily ? qualityModels(catalogModels, profiles, catalogRoutes, actorFamily) : [];
  $: modelsValid = validModel(actor, draftModels) && validModel(critic, reviewModels);
  function validModel(route: string, models: ModelChoice[]) {
    const selection = qualitySelection(models, catalogRoutes, route);
    return !!selection && profiles.some(profile => profile.name === route)
      && models.some(model => model.id === selection.model_id && model.installed !== false && !!model.choices[selection.thinking]);
  }
  function chooseModel(role: 'actor'|'critic', selection: ModelSelection) {
    const route = routeForSelection(role === 'actor' ? draftModels : reviewModels, selection);
    if (route) { if (role === 'actor') actor = route; else critic = route; }
  }
  let proposed: ProposedDraft | null = null;
  let proposalsError = '';
  let requestedProposalId = '';
  let requestedRunId = '';
  let requestedCampaignId = '';
  let taskId = '', requestedTaskId = '', taskContext: TaskContext | null = null;
  let policyRunLimit = 1, policyExpiry = '', pendingPolicy: {signature:string;id:string} | null = null;
  let captureHandoff = '';
  let lessons: unknown = null;
  let runReport: RunReportView | null = null;
  let workGoal = '', workView: WorkView | null = null;
  interface Preview { contract: unknown; contract_hash:string; maximum_attempt_microusd: number; budget_can_admit_attempt: boolean; native_harness?:{metered_tools?:unknown[]}|null }
  let preview: Preview | null = null;
  let reviewedSignature = '';
  let error = '', loadError = '', profilesError = '', busy = false, generation = 0, alive = true, selectedAttempt = 0;
  let artifact: Record<string,string> = {}, previous: Record<string,string> = {};
  let comparePrevious = false;
  let comparisonError = '', comparisonLoading = false;
  let binaryFiles: NonNullable<ArtifactFiles['binary_files']> = {};
  let artifactPreview:{key:string;name:string;mediaType:string;url:string}|null = null;
  let previewRequest = 0, previewLoading = '';
  let loadedArtifactKey = '', artifactRequest = 0;
  let loading = false, timer: ReturnType<typeof setInterval> | undefined;
  let pendingStart: { signature: string; id: string } | null = null;
  let controller: AbortController | null = null;
  $: current = selected?.attempts.find(a => a.number === selectedAttempt);
  // The owning task's pending asks (review approvals, budget and capture notices), answered here in place.
  $: ownerAsks = selected ? pendingAsksForTask($pendingAsks, selected.admission.task_id) : [];
  $: waitingForTaskAnswer = selected?.status === 'waiting_owner' && !!selected.attention_id && !selected.attempts.at(-1)?.decided && selected.attempts.at(-1)?.checkpoint?.attention_id === selected.attention_id;
  $: artifactMatchesSelection = !!selected && !!current?.artifact && loadedArtifactKey === artifactKey(selected,current);
  $: formSignature = JSON.stringify([goal,source,rubric,filename,actor,critic,agent,thread,attempts,hours,budget,threshold,humanReview,handoffKind,unknownReview,workMode,stepsPerAttempt,callsPerAttempt,proposed,taskId,taskContext,policyRunLimit,policyExpiry,captureHandoff,completeContract,references]);
  $: completeContractProblem = contractProblem(completeContract);
  $: if (preview && formSignature !== reviewedSignature) preview = null;
  $: canStart = !referencesPending && !!agent && (!!taskContext||!!proposed||availableAgents.some(a=>a.agent_id===agent)) && !!thread && (!taskId.trim() || (taskContext?.task_id===taskId.trim() && taskContext.can_start)) && (proposed!==null || (completeContract.trim()? !completeContractProblem : (goal.trim().length > 0 && rubric.trim().length > 0 && modelsValid && validLimit(attempts,true) && validLimit(hours,false,3600) && validLimit(budget,false,1000000))));
  $: actorFamily = profiles.find(p => p.name === actor)?.family;

  // Empty means Unlimited: clearing a bound number input yields null.
  function validLimit(value:number|null|undefined, integer=false, multiplier=1) {
    return value==null || (Number.isFinite(value) && value>=0 && Number.isSafeInteger(Math.ceil(value*multiplier)) && (!integer || Number.isInteger(value)));
  }
  const limitUnits = (value:number|null|undefined, multiplier:number) => Math.ceil((value??0)*multiplier);
  function contractProblem(text:string) {
    if(!text.trim())return '';
    try {const value=JSON.parse(text);if(!value||Array.isArray(value)||typeof value!=='object')return 'Enter a complete contract object.';return '';}
    catch{return 'The complete contract must be valid JSON.';}
  }
  function proposal():ProposedDraft['proposal'] {
    if(proposed)return proposed.proposal;
    if(completeContract.trim())return JSON.parse(completeContract) as ProposedDraft['proposal'];
    return {
      goal, artifact_file: filename, maximum_artifact_bytes: 65536, human_review:humanReview, handoff_kind:handoffKind,
      actor_profile: actor, critic_profiles: [critic,critic,critic],
      ...(workMode === 'task' ? {native_harness:{agent_id:agent,maximum_iterations:stepsPerAttempt,maximum_physical_calls:callsPerAttempt,capabilities:['files']}} : {}),
      sources: {...(source.trim() ? {brief: source} : {}),...referenceSources(references)},
      criteria: [
        {id:'readable_artifact', specification:{kind:'check',assertions:[{kind:'utf8',file:filename},{kind:'byte_length',file:filename,minimum:1,maximum:65536}]},weight:null,must_pass:true,pass_score:null},
        {id:'grounding',specification:{kind:'grounding',instructions:'Review all factual claims against the sealed source notes. Unsupported claims must be removed or corrected; uncertainty must remain visible.'},weight:null,must_pass:true,pass_score:1},
        {id:'quality',specification:{kind:'rubric',instructions:rubric,anchors:[{score:0,description:'Does not satisfy the requested outcome; major omissions or unusable output.'},{score:.5,description:'Useful draft with substantive gaps against the stated requirements.'},{score:1,description:'Fully satisfies the stated requirements; complete and ready for the intended local review.'}]},weight:1,must_pass:false,pass_score:null}
      ], score_at_least:threshold,
      review:{panel_size:3,score_quorum:3,min_confidence:.8,max_score_spread:.2,noise_margin:.02,max_retries_per_critic:1,...(unknownReview==='pause'?{}:{unknown_review:unknownReview})},
      budget:{max_attempts:attempts??0,max_wall_clock_secs:limitUnits(hours,3600),max_cost_microusd:limitUnits(budget,1_000_000),plateau_attempts:budgetDefaults.plateau_attempts}
    };
  }
  function editCompleteContract() {completeContract=JSON.stringify(proposal(),null,2);preview=null;pendingStart=null;}
  function schedulePage(){
    clearTimeout(queryTimer);catalogRequest++;loading=true;
    queryTimer=setTimeout(()=>void load(),150);
  }
  async function load() {
    clearTimeout(queryTimer);
    if(!['inbox','runs','tasks'].includes(tab))return;
    const path=tab==='tasks'?pagePath('/work',page,pageSize,{q:search,goal_id:workGoal}):pagePath('/catalog',page,pageSize,{view:tab==='runs'?'runs':'inbox',q:search,status:tab==='runs'?statusFilter:inboxFilter,agent_id:proposalAgent});
    const g=generation,seq=++catalogRequest;
    lastQuery=path;loading=true;
    try {
      if(tab==='tasks') {
        const result=await request<WorkView>(path,undefined,controller?.signal);
        if(!alive||g!==generation||seq!==catalogRequest||queryPath!==path)return;
        workView=result;total=result.pagination.total;
      } else {
        const result=await request<CatalogPage>(path,undefined,controller?.signal);
        if(!alive||g!==generation||seq!==catalogRequest||queryPath!==path)return;
        catalogItems=result.items;total=result.pagination.total;counts=result.counts;countsRefreshed=Date.now();proposingAgents=result.agents;
      }
      catalogRefreshed=Date.now();loadError='';
      const lastPage=Math.max(1,Math.ceil(total/pageSize));
      if(page>lastPage)page=lastPage;
    }catch(e){if(alive&&g===generation&&seq===catalogRequest&&queryPath===path)loadError=e instanceof Error?e.message:String(e);}
    finally{if(g===generation&&seq===catalogRequest)loading=false;}
  }
  /** The catalog's `counts` are scope totals: the server counts every run and brief before it
   * applies the search, status or agent filter. The catalog tabs get them with each page; the
   * Tasks and Policies tabs ask for them here so "needing review" is right on every tab. */
  async function loadCounts() {
    const g=generation;
    try {
      const result=await request<CatalogPage>(pagePath('/catalog',1,1,{view:'inbox'}),undefined,controller?.signal);
      if(alive&&g===generation){counts=result.counts;countsRefreshed=Date.now();}
    }catch{/* the counts stay as they were; the catalog tabs report load errors */}
  }
  async function loadProfiles() {
    const g=generation;
    try {
      const [result,agents,catalog]=await Promise.all([request<{profiles:Profile[];actor_default:string;critic_default:string;audio_review_providers?:typeof audioProviders;budget_defaults?:typeof budgetDefaults;native_defaults?:{maximum_iterations:number;maximum_physical_calls:number}}>('/profiles',undefined,controller?.signal),refreshAgents(),fetchRoutingOverview()]);
      if(alive&&g===generation){
        if(result.budget_defaults)budgetDefaults=result.budget_defaults;
        if(!defaultsApplied){
          if(!formOpen&&!proposed&&!taskContext){attempts=budgetDefaults.max_attempts;hours=budgetDefaults.max_wall_clock_secs/3600;budget=budgetDefaults.max_cost_microusd/1000000;stepsPerAttempt=result.native_defaults?.maximum_iterations??1024;callsPerAttempt=result.native_defaults?.maximum_physical_calls??500;}
          resetCost=budgetDefaults.max_cost_microusd/1000000;resetHours=budgetDefaults.max_wall_clock_secs/3600;resetAttempts=budgetDefaults.max_attempts;defaultsApplied=true;
        }
        profilesError='';profiles=result.profiles;catalogModels=catalog.models??[];catalogRoutes=catalog.profiles;audioProviders=result.audio_review_providers??[];actor=actor||result.actor_default;critic=critic||result.critic_default;if(!agent&&!taskContext&&!proposed)agent=agents.find(a=>a.is_primary&&!a.disabled&&!a.configured_disabled)?.agent_id??'';}
    } catch(e){if(alive&&g===generation)profilesError=String(e);}
  }
  async function openProposal(id:string){
    const g=generation,seq=++detailRequest;
    try{
      const next=await request<ProposedDraft>(`/proposals/${encodeURIComponent(id)}`,undefined,controller?.signal);
      if(alive&&g===generation&&seq===detailRequest)selectProposal(next);
    }catch(e){if(alive&&g===generation&&seq===detailRequest)proposalsError=String(e);}
  }
  async function refresh() {
    const g=generation;
    await Promise.all([loadProfiles(),load(),...(['inbox','runs'].includes(tab)?[]:[loadCounts()])]);
    if(!alive||g!==generation)return;
    if(requestedRunId){
      const id=requestedRunId;await inspect(id);
      if(!alive||g!==generation)return;
      if(requestedRunId===id)requestedRunId='';
    }
    if(requestedProposalId){
      const id=requestedProposalId;await openProposal(id);
      if(!alive||g!==generation)return;
      if(requestedProposalId===id)requestedProposalId='';
    }
    if(requestedTaskId){taskId=requestedTaskId;await loadTaskContext();}
  }
  async function loadTaskContext() {
    const seq=++detailRequest;draftRevision++;referencesPending=false;
    const id=taskId.trim(),g=generation;preview=null;pendingStart=null;taskContext=null;completeContract='';
    if(!id)return;
    try {
      const context=await request<TaskContext>(`/task-context/${encodeURIComponent(id)}`,undefined,controller?.signal);
      if(!alive||g!==generation||seq!==detailRequest||taskId.trim()!==id)return;
      formOpen=true;selected=null;clearArtifacts();taskId=id;taskContext=context;proposed=null;goal=context.goal;agent=context.agent_id;thread=context.ui_thread_id;workMode='task';captureHandoff=context.policy?.capture_handoff?JSON.stringify(context.policy.capture_handoff,null,2):'';if(captureHandoff)handoffKind='capture';references=[...references.filter(ref=>!ref.id.startsWith('task:')),{id:`task:${context.task_id}`,title:context.title,origin:`Task ${context.task_id} · context ${context.context_hash}`,content:context.goal}];error='';
    }catch(e){if(alive&&g===generation&&seq===detailRequest&&taskId.trim()===id)error=String(e);}
  }
  function clearTaskContext() {taskId='';requestedTaskId='';taskContext=null;preview=null;pendingStart=null;pendingPolicy=null;captureHandoff='';}
  function selectProposal(next:ProposedDraft|null) {
    draftRevision++;referencesPending=false;
    detailRequest++;proposed=next;completeContract='';preview=null;pendingStart=null;error='';
    if(next){formOpen=true;selected=null;clearArtifacts();clearTaskContext();references=[];agent=next.origin.agent_id;}
  }
  async function attachProposalToTask() {
    if(!proposed||busy)return;
    const saved=proposed,g=generation;busy=true;error='';
    try {
      const context=await request<TaskContext>(`/task-context/${encodeURIComponent(saved.origin.task_id)}`,undefined,controller?.signal);
      if(!alive||g!==generation||proposed!==saved)return;
      if(!saved.proposal.native_harness||context.goal!==saved.proposal.goal||context.agent_id!==saved.origin.agent_id)throw new Error('This proposal needs the same task goal, owning agent and native actor to attach. Review a revised proposal or use a new task.');
      taskId=context.task_id;taskContext=context;goal=context.goal;agent=context.agent_id;thread=context.ui_thread_id;workMode='task';preview=null;pendingStart=null;
    }catch(e){if(alive&&g===generation)error=String(e);}finally{if(g===generation)busy=false;}
  }
  function downloadProposedSource(id:string,encoded:unknown) {
    if(typeof encoded!=='string')return;
    try {
      const bytes=Uint8Array.from(atob(encoded),char=>char.charCodeAt(0));
      const url=URL.createObjectURL(new Blob([bytes],{type:'application/octet-stream'})),link=document.createElement('a');
      link.href=url;link.download=id.replace(/[\\/]/g,'_');link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
    }catch(e){error=String(e);}
  }
  async function inspect(input:Run|RunSummary|string) {
    const g=generation,seq=++detailRequest;
    formOpen=false;selected=null;clearArtifacts();lessons=null;runReport=null;error='';
    try {
      const run=typeof input==='string'||!('attempts' in input)?await request<Run>(`/runs/${typeof input==='string'?input:input.id}`,undefined,controller?.signal):input;
      if(!alive||g!==generation||seq!==detailRequest)return;
      selected=run;selectedAttempt=(run.status==='accepted'?run.attempts.find(attempt=>attempt.artifact?.hash===run.routing?.artifact_hash):undefined)?.number??run.attempts.at(-1)?.number??0;
      await showAttempt(currentFor(run,selectedAttempt));
    }catch(e){if(alive&&g===generation&&seq===detailRequest)error=String(e);}
  }
  async function inspectCampaignArtifact(id:string,hash:string,campaignId?:string) {
    const g=generation,seq=++detailRequest;
    const run=await request<Run>(`/runs/${id}`,undefined,controller?.signal);
    if(!alive||g!==generation||seq!==detailRequest)return;
    const attempt=run.attempts.find(attempt=>attempt.artifact?.hash===hash);
    if(!attempt)throw new Error('Bound artifact is unavailable in its run.');
    lessons=null;runReport=null;formOpen=false;selected=run;selectedAttempt=attempt.number;if(campaignId)requestedCampaignId=campaignId;tab='runs';await showAttempt(attempt);
  }
  const currentFor = (run:Run, number:number) => run.attempts.find(a=>a.number===number);
  const artifactKey = (run:Run, attempt:Attempt|undefined) => attempt?.artifact ? `${run.id}:${attempt.artifact.hash}` : '';
  function clearPreview() {previewRequest++;if(artifactPreview)URL.revokeObjectURL(artifactPreview.url);artifactPreview=null;previewLoading='';}
  function clearArtifacts() { clearPreview();artifactRequest++; loadedArtifactKey=''; artifact={}; previous={}; comparisonError=''; comparisonLoading=false; binaryFiles={}; artifactEvidence=null; }
  async function inspectArtifactEvidence() {
    if(!selected||!current?.artifact||!artifactMatchesSelection)return;
    const g=generation,key=loadedArtifactKey,id=selected.id,hash=current.artifact.hash;
    try {
      const proof=await request<NonNullable<typeof artifactEvidence>>(`/runs/${id}/artifacts/${hash}/evidence`,undefined,controller?.signal);
      if(!alive||g!==generation||loadedArtifactKey!==key)return;
      if(!proof.sealed||proof.evidence.run_id!==id||proof.evidence.artifact_hash!==hash)throw new Error('Evidence belongs to a different artifact.');
      artifactEvidence=proof;
    }catch(e){if(alive&&g===generation&&loadedArtifactKey===key)error=String(e);}
  }
  async function showAttempt(attempt:Attempt|undefined) {
    clearArtifacts();comparePrevious=false;
    if (!selected || !attempt?.artifact) return;
    const g=generation, id=selected.id, number=attempt.number, key=artifactKey(selected,attempt), requestId=artifactRequest; selectedAttempt=number;
    void loadPreviousAttempt(selected,attempt);
    try {
      const next=await request<ArtifactFiles>(`/runs/${id}/artifacts/${attempt.artifact.hash}`,undefined,controller?.signal);
      if(alive&&g===generation&&selected?.id===id&&selectedAttempt===number&&artifactKey(selected,currentFor(selected,number))===key&&requestId===artifactRequest){artifact=next.files;binaryFiles=next.binary_files??{};loadedArtifactKey=key;}
    } catch(e){if(alive&&g===generation&&requestId===artifactRequest)error=String(e);}
  }
  async function loadPreviousAttempt(run:Run,attempt:Attempt) {
    const before=run.attempts.find(a=>a.number===attempt.number-1)?.artifact;
    if(!before)return;
    const g=generation,id=run.id,number=attempt.number,key=artifactKey(run,attempt),requestId=artifactRequest;
    const isCurrent=()=>alive&&g===generation&&selected?.id===id&&selectedAttempt===number&&artifactKey(selected,currentFor(selected,number))===key&&requestId===artifactRequest;
    comparisonError='';comparisonLoading=true;
    try {
      const prior=await request<ArtifactFiles>(`/runs/${id}/artifacts/${before.hash}`,undefined,controller?.signal);
      if(isCurrent())previous=prior.files;
    } catch(e){if(isCurrent())comparisonError=String(e);}
    finally{if(isCurrent())comparisonLoading=false;}
  }
  const sameRunSelection=(id:string,g:number,seq:number)=>alive&&g===generation&&selected?.id===id&&seq===detailRequest;
  const canPreview = (mediaType:string) => ['image/png','image/jpeg','image/webp','video/mp4','video/webm','audio/wav','audio/mpeg'].includes(mediaType);
  async function previewArtifact(name:string) {
    if(!selected||!current?.artifact||!artifactMatchesSelection)return;
    const metadata=binaryFiles[name];if(!metadata||!canPreview(metadata.media_type))return;
    clearPreview();
    const id=selected.id,hash=current.artifact.hash,g=generation,key=loadedArtifactKey,revision=previewRequest;
    previewLoading=name;
    try {
      const blob=await requestArtifactFile(`/runs/${id}/artifacts/${hash}/file?name=${encodeURIComponent(name)}`,controller?.signal);
      if(!alive||g!==generation||loadedArtifactKey!==key||revision!==previewRequest)return;
      if(blob.size!==metadata.bytes||blob.type!==metadata.media_type)throw new Error('Artifact preview differs from its sealed file metadata.');
      artifactPreview={key,name,mediaType:metadata.media_type,url:URL.createObjectURL(blob)};
    }catch(e){if(alive&&g===generation&&revision===previewRequest)error=String(e);}
    finally{if(g===generation&&revision===previewRequest)previewLoading='';}
  }
  async function downloadArtifact(name:string) {
    if(!selected||!current?.artifact||!artifactMatchesSelection)return;
    const id=selected.id,hash=current.artifact.hash,g=generation,key=loadedArtifactKey,seq=detailRequest;
    try {
      const blob=await requestArtifactFile(`/runs/${id}/artifacts/${hash}/file?name=${encodeURIComponent(name)}`,controller?.signal);
      if(!alive||g!==generation||loadedArtifactKey!==key)return;
      const url=URL.createObjectURL(blob),link=document.createElement('a');
      link.href=url;link.download=name.split('/').at(-1)??name;link.click();
      setTimeout(()=>URL.revokeObjectURL(url),1000);
    }catch(e){if(sameRunSelection(id,g,seq)&&loadedArtifactKey===key)error=String(e);}
  }
  async function reviewContract() {
    busy=true;error='';preview=null;const g=generation, signature=formSignature;
    try {const next=await request<Preview>('/preview',proposal(),controller?.signal);if(alive&&g===generation&&formSignature===signature){reviewedSignature=signature;preview=next;}}
    catch(e){if(alive&&g===generation)error=String(e);}finally{if(g===generation)busy=false;}
  }
  async function start() {
    if(!canStart||busy||!preview?.budget_can_admit_attempt||formSignature!==reviewedSignature)return;busy=true;error='';const g=generation;
    try {
      const admission={proposal:proposal(),agent_id:agent,ui_thread_id:thread,expected_contract_hash:preview.contract_hash,...(captureHandoff.trim()?{capture_handoff:JSON.parse(captureHandoff)}:{}),...(taskContext?{existing_task:{task_id:taskContext.task_id,context_hash:taskContext.context_hash}}:{}),...(proposed?{proposal_request_id:proposed.origin.request_id}:{})};
      const signature=JSON.stringify(admission);
      if(!pendingStart||pendingStart.signature!==signature)pendingStart={signature,id:taskContext?.pending_run_id??crypto.randomUUID()};
      const run=await request<Run>('/runs',{run_id:pendingStart.id,...admission},controller?.signal);
      if(alive&&g===generation){pendingStart=null;preview=null;await inspect(run);await load();}
    }catch(e){if(alive&&g===generation)error=String(e);}finally{if(g===generation)busy=false;}
  }
  async function configurePolicy(enabled:boolean) {
    if(!taskContext||busy||(enabled&&(!preview||!policyExpiry)))return;
    const context=taskContext,g=generation,p=enabled?proposal():null;
    const expiry=enabled?new Date(policyExpiry).toISOString():new Date().toISOString();
    const signature=JSON.stringify([context.task_id,context.context_hash,context.policy?.id,p,preview?.contract_hash,policyRunLimit,expiry,enabled,captureHandoff]);
    if(!pendingPolicy||pendingPolicy.signature!==signature)pendingPolicy={signature,id:crypto.randomUUID()};
    busy=true;error='';
    try {
      await request(`/task-policy/${encodeURIComponent(context.task_id)}`,{
        policy_id:pendingPolicy.id,expected_policy_id:context.policy?.id??null,
        expected_context_hash:context.context_hash,expected_contract_hash:preview?.contract_hash??'',
        proposal:enabled?p:null,max_runs:enabled?policyRunLimit:0,expires_at:expiry,
        capture_handoff:enabled&&captureHandoff.trim()?JSON.parse(captureHandoff):null,
      },controller?.signal);
      if(alive&&g===generation){pendingPolicy=null;preview=null;await loadTaskContext();}
    }catch(e){if(alive&&g===generation)error=String(e);}finally{if(g===generation)busy=false;}
  }
  async function act(action:'cancel'|'resume'|'reconcile-costs') {
    if(!selected||busy)return;busy=true;error='';const g=generation,id=selected.id,seq=detailRequest;
    try {const next=await request<Run>(`/runs/${id}/${action}`,action==='resume'?{attention_id:selected.attention_id}:{},controller?.signal);if(sameRunSelection(id,g,seq)){selected=next;await load();}}
    catch(e){if(sameRunSelection(id,g,seq))error=String(e);}finally{if(g===generation)busy=false;}
  }
  async function continueBudget() {
    if(!selected?.budget_continuation?.available||!selected.exhausted_limits||selected.generation===undefined||!selected.contract_hash||busy)return;
    const limits=selected.exhausted_limits;
    const reset={...(limits.cost?{cost_microusd:limitUnits(resetCost,1000000)}:{}),...(limits.time?{wall_clock_secs:limitUnits(resetHours,3600)}:{}),...(limits.attempts?{attempts:resetAttempts??0}:{})};
    if(!Object.values(reset).every(n=>Number.isSafeInteger(n)&&n>=0)){error='Enter nonnegative whole allowances; 0 means Unlimited (time is measured in seconds).';return;}
    const reviewed={expected_generation:selected.generation,expected_contract_hash:selected.contract_hash,reset};
    const signature=JSON.stringify([selected.id,reviewed]);
    if(pendingReset?.signature!==signature)pendingReset={signature,id:crypto.randomUUID()};
    busy=true;error='';const id=selected.id,g=generation,seq=detailRequest;
    try {const next=await request<Run>(`/runs/${id}/budget-resets`,{reset_id:pendingReset.id,...reviewed},controller?.signal);if(sameRunSelection(id,g,seq)){selected=next;pendingReset=null;await load();}}
    catch(e){if(sameRunSelection(id,g,seq))error=String(e);}finally{if(g===generation)busy=false;}
  }
  async function reviewDraft(approved:boolean) {
    if(!selected?.routing||busy||(approved&&(!artifactMatchesSelection||current?.artifact?.hash!==selected.routing.artifact_hash)))return;busy=true;error='';const g=generation,id=selected.id,seq=detailRequest;
    try {const next=await request<Run>(`/runs/${id}/review`,{artifact_hash:selected.routing.artifact_hash,approved},controller?.signal);if(sameRunSelection(id,g,seq)){selected=next;await load();}}
    catch(e){if(sameRunSelection(id,g,seq))error=String(e);}finally{if(g===generation)busy=false;}
  }
  async function loadReport(download=false) {
    if(!selected||busy)return;
    const id=selected.id,g=generation,seq=detailRequest;busy=true;error='';
    try {
      const result=await request<RunReportView>(`/runs/${id}/report`,undefined,controller?.signal);
      if(!sameRunSelection(id,g,seq))return;
      runReport=result;
      if(download){
        const url=URL.createObjectURL(new Blob([result.markdown],{type:'text/markdown;charset=utf-8'}));
        const link=document.createElement('a');link.href=url;link.download=`relentless-${id}-report.md`;link.click();
        setTimeout(()=>URL.revokeObjectURL(url),1000);
      }
    }catch(e){if(sameRunSelection(id,g,seq))error=String(e);}finally{if(g===generation)busy=false;}
  }
  async function showReportedOutput() {
    if(!selected||runReport?.report.run_id!==selected.id||!runReport.report.output)return;
    const output=runReport.report.output;
    const attempt=selected.attempts.find(attempt=>attempt.number===output.attempt_number&&attempt.artifact?.hash===output.artifact_hash);
    if(attempt)await showAttempt(attempt);
  }
  async function loadLessons() {
    if(!selected||busy)return;
    const id=selected.id,g=generation,seq=detailRequest;busy=true;error='';
    try {const result=await request(`/runs/${id}/lessons`,undefined,controller?.signal);if(sameRunSelection(id,g,seq))lessons=result;}
    catch(e){if(sameRunSelection(id,g,seq))error=String(e);}finally{if(g===generation)busy=false;}
  }
  async function loadWork() {page=1;await load();}

  function openDraft() {
    if(busy)return;
    detailRequest++;proposed=null;selected=null;clearArtifacts();clearTaskContext();completeContract='';
    draftRevision++;goal='';source='';rubric='';references=[];referencesPending=false;formOpen=true;error='';
  }
  async function switchTab(next:typeof tab) {
    tab=next;search='';statusFilter='all';inboxFilter='all';proposalAgent='';page=1;
    if(!['inbox','runs'].includes(next)&&Date.now()-countsRefreshed>=30000)void loadCounts();

  }
  async function poll() {
    if(polling||loading||!alive||document.hidden)return;
    if(!['inbox','runs'].includes(tab)&&Date.now()-countsRefreshed>=30000)void loadCounts();
    // Refresh the catalog less often. Only the selected, unfinished run needs
    // detailed journal polling; an entire workspace is not polled every 3s.
    if(Date.now()-catalogRefreshed>=30000){await load();return;}
    if(!selected||!['active','review','attention'].includes(runGroup(selected)))return;
    const id=selected.id,g=generation;polling=true;
    try {
      const next=await request<Run>(`/runs/${id}`,undefined,controller?.signal);
      if(!alive||g!==generation||selected?.id!==id)return;
      const becameAccepted=selected.status!=='accepted'&&next.status==='accepted';
      if(becameAccepted||!currentFor(next,selectedAttempt))selectedAttempt=(becameAccepted?next.attempts.find(attempt=>attempt.artifact?.hash===next.routing?.artifact_hash):undefined)?.number??next.attempts.at(-1)?.number??0;
      selected=next;
      if(loadedArtifactKey!==artifactKey(next,currentFor(next,selectedAttempt)))await showAttempt(currentFor(next,selectedAttempt));
    }catch(e){if(alive&&g===generation&&selected?.id===id)loadError=String(e);}
    finally{if(g===generation)polling=false;}
  }
  onMount(()=>{
    requestedProposalId=new URL(window.location.href).searchParams.get('proposal')??'';
    requestedRunId=new URL(window.location.href).searchParams.get('run')??'';
    requestedCampaignId=new URL(window.location.href).searchParams.get('campaign')??'';
    // An exact review target owns navigation. A task carried as context must
    // not replace that target with the existing-task draft composer.
    requestedTaskId=requestedRunId||requestedProposalId||requestedCampaignId?'':new URL(window.location.href).searchParams.get('task')??'';
    let key='';const unsub=scopeIdentityStore.subscribe(scope=>{
      const next=JSON.stringify([scope.principal,scope.workspace,scope.isResolved,getCurrentScopeCredentialRevision()]);
      if(next===key)return;key=next;generation++;controller?.abort();controller=new AbortController();
      catalogRequest++;detailRequest++;catalogItems=[];total=0;counts={};countsRefreshed=0;proposingAgents=[];lastQuery='';selected=null;lessons=null;runReport=null;clearArtifacts();preview=null;pendingStart=null;pendingPolicy=null;captureHandoff='';error='';loadError='';profilesError='';proposalsError='';proposed=null;busy=false;loading=false;polling=false;page=1;search='';statusFilter='all';inboxFilter='all';proposalAgent='';profiles=[];catalogModels=[];catalogRoutes=[];actor='';critic='';
      goal='';source='';rubric='';references=[];referencesPending=false;taskContext=null;taskId=requestedTaskId;workView=null;workGoal='';completeContract='';audioProviders=[];agent='';
      void refresh();
    });
    if(requestedTaskId||requestedProposalId)formOpen=true;
    if(requestedCampaignId)tab='policies';
    mounted=true;
    timer=setInterval(poll,3000);return unsub;
  });
  onDestroy(()=>{clearTimeout(queryTimer);alive=false;generation++;controller?.abort();clearPreview();if(timer)clearInterval(timer);});
</script>

<svelte:head><title>Quality checks · Work · Magican</title></svelte:head>
<div class="workspace">
  <!-- Top Command Masthead -->
  <header class="quality-masthead">
    <div class="quality-masthead__identity">
      <div class="quality-masthead__badge-row">
        <span class="masthead-badge">Output review</span>
        <span class="masthead-count">{(counts.brief??0)+(counts.review??0)+(counts.attention??0)} needing review</span>
      </div>
      <h2>Quality checks <WorkHelp topic={WORK_HELP.quality} /></h2>
      <p>Review agent briefs, follow run progress, inspect quality policies, and approve finished work.</p>
    </div>
    <div class="quality-masthead__actions">
      <button type="button" class="action-btn" disabled={loading||busy} onclick={refresh} title="Refresh quality items">
        <Icon name="rotate-ccw" size={13} />
        <span>{loading ? 'Refreshing...' : 'Refresh'}</span>
      </button>
      <button type="button" class="action-btn action-btn--primary" disabled={busy} onclick={()=>{tab='inbox';openDraft();}} title="Create a new task brief">
        <Icon name="plus" size={14} />
        <span>New draft</span>
      </button>
    </div>
  </header>
  {#if error||profilesError||loadError||proposalsError}<div class="error notice" role="alert">{error||profilesError||loadError||proposalsError}</div>{/if}
  <nav class="tabs" aria-label="Quality views">
    <button class:active={tab==='inbox'} aria-current={tab==='inbox'?'page':undefined} onclick={()=>switchTab('inbox')}>Needs review <span class="count">{(counts.brief??0)+(counts.review??0)+(counts.attention??0)}</span></button>
    <button class:active={tab==='runs'} aria-current={tab==='runs'?'page':undefined} onclick={()=>switchTab('runs')}>Runs <span class="count">{counts.active??0} active</span></button>
    <button class:active={tab==='tasks'} aria-current={tab==='tasks'?'page':undefined} onclick={()=>switchTab('tasks')}>Tasks</button>
    <button class:active={tab==='policies'} aria-current={tab==='policies'?'page':undefined} onclick={()=>switchTab('policies')}>Policies & learning</button>
  </nav>
  {#if tab==='inbox'||tab==='runs'}
    <div class:has-detail={formOpen||!!selected} class="work-layout">
      <section class="catalog" aria-label={tab==='inbox'?'Review inbox':'Run catalog'}>
        <div class="section-heading"><div><h2>{tab==='inbox'?'Needs your review':'All runs'}</h2><p>{tab==='inbox'?'Agent proposals, accepted drafts and tasks needing input.':'Progress and history across your tasks.'}</p></div><span class="muted">{total} items</span></div>
        <div class="filters">
          <label class="search-field"><span class="sr-only">Search work</span><input type="search" bind:value={search} placeholder={tab==='inbox'?'Search work, agent or task…':'Search work or task…'}/></label>
          {#if tab==='runs'}<label><span class="sr-only">Run status</span><select aria-label="Run status" bind:value={statusFilter}><option value="all">All statuses</option><option value="active">In progress</option><option value="review">Review draft</option><option value="attention">Needs input</option><option value="complete">Completed</option><option value="stopped">Stopped</option></select></label>
          {:else}<label><span class="sr-only">Review type</span><select aria-label="Review type" bind:value={inboxFilter}><option value="all">All reviews</option><option value="attention">Needs input</option><option value="review">Finished work</option><option value="brief">Agent briefs</option></select></label>{#if proposingAgents.length>1}<label><span class="sr-only">Proposing agent</span><select aria-label="Proposing agent" bind:value={proposalAgent}><option value="">All proposing agents</option>{#each proposingAgents as id}<option value={id}>{id}</option>{/each}</select></label>{/if}{/if}
        </div>
        <div class="work-list">
          {#each visibleItems as entry (entry.kind==='proposal'?`proposal:${entry.item.origin.request_id}`:`run:${entry.item.id}`)}
            {#if entry.kind==='proposal'}
              <button class="work-row" class:selected={proposed?.origin.request_id===entry.item.origin.request_id} disabled={busy} aria-label={`${entry.item.origin.agent_id}: ${entry.item.goal}`} onclick={()=>openProposal(entry.item.origin.request_id)}>
                <div class="row-title"><strong>{entry.item.goal}</strong><span class="work-status review">Review brief</span></div>
                <span class="row-meta">{entry.item.origin.agent_id} · {entry.item.reference_count} references · {costLimit(entry.item.maximum_cost_microusd)} ceiling</span>
              </button>
            {:else if entry.kind==='unavailable'}
              <div class="work-row" role="status"><div class="row-title"><strong>Stored run unavailable</strong><span class="work-status attention">Needs attention</span></div><span class="row-meta">{entry.item.id}</span><p>{entry.item.reason}</p></div>
            {:else}
              <button class="work-row" class:selected={selected?.id===entry.item.id} aria-label={`${entry.item.goal} · ${entry.item.label}`} onclick={()=>inspect(entry.item)}>
                <div class="row-title"><strong>{entry.item.goal}</strong><span class:review={entry.item.group==='review'} class:attention={entry.item.group==='attention'} class:completed={entry.item.group==='complete'} class="work-status">{entry.item.label}</span></div>
                <span class="row-meta">{entry.item.attempt_count}/{countLimit(entry.item.max_attempts)} attempts · {usd(entry.item.spend.settled_microusd)} spent{entry.item.started_at?` · ${new Date(entry.item.started_at).toLocaleDateString()}`:''}</span>
              </button>
            {/if}
          {/each}
          {#if loading}<p role="status">Loading work…</p>{:else if !catalogItems.length}<div class="empty-state"><h3>{search||statusFilter!=='all'||proposalAgent?'No matching work':tab==='inbox'?'You’re caught up':'No runs yet'}</h3><p>{search||statusFilter!=='all'||proposalAgent?'Change the search or filters to see other work.':tab==='inbox'?'Agents’ proposed briefs and finished drafts will appear here when they need your attention.':'Start from an existing task or review a brief proposed by an agent.'}</p>{#if !search&&statusFilter==='all'&&!proposalAgent}<button onclick={()=>switchTab('tasks')}>Choose an existing task</button>{/if}</div>{/if}
        </div>
        {#if total>pageSize}<ServerPager currentPage={page} {pageCount} startItem={(page-1)*pageSize+1} endItem={Math.min(page*pageSize,total)} totalItems={total} ariaLabel="Work pagination" {loading} on:pagechange={event=>page=event.detail.page}/>{/if}
      </section>
      {#if formOpen}
      <section class="detail form" aria-label={proposed?'Review agent brief':'New draft'}>
        <div class="section-heading"><div><div class="eyebrow">{proposed?'Agent proposal':'Manual setup'}</div><h2>{proposed?'Review agent brief':taskContext?'Improve existing task':'New draft'}</h2></div><button aria-label="Close draft setup" disabled={busy} onclick={()=>formOpen=false}>Close</button></div>
        {#if proposed}
          <h3 class="brief-goal">{proposed.proposal.goal}</h3>
          <div class="origin"><span>Proposed by <strong>{proposed.origin.agent_id}</strong></span><a href={workTaskHref(proposed.origin.task_id)}>Source task</a><span>Request <code>{proposed.origin.request_id}</code></span></div>
          <div class="budget-summary"><span><strong>{costLimit(proposed.proposal.budget.max_cost_microusd)}</strong> budget ceiling</span><span><strong>{countLimit(proposed.proposal.budget.max_attempts)}</strong> attempts</span><span><strong>{countLimit(Math.ceil(proposed.proposal.budget.max_wall_clock_secs/60))}</strong> minutes</span></div>
          <h3>Definition of done</h3>
          {#each proposalCriteria(proposed.proposal) as criterion}<div class="criterion"><strong>{criterion.id.replaceAll('_',' ')} {#if criterion.required}<span class="work-status">Required</span>{/if}</strong><p>{criterion.instructions}</p></div>{/each}
          <h3>References <span class="muted">{Object.keys(proposed.proposal.sources).length}</span></h3>
          {#each Object.entries(proposed.proposal.sources) as [id,text]}<details class="source"><summary>{id}</summary><pre>{text}</pre></details>{/each}
          {#if !Object.keys(proposed.proposal.sources).length}<p class="muted">No text references supplied. Check that factual claims have adequate evidence.</p>{/if}
          {#each Object.entries((proposed.proposal.source_files??{}) as Record<string,unknown>) as [id,encoded]}<button onclick={()=>downloadProposedSource(id,encoded)}>Inspect source file: {id}</button>{/each}
          <p>Handoff: {proposed.proposal.handoff_kind==='capture'?'communication capture':'local task draft'} · {proposed.proposal.human_review==='independent'?'independent reviewer decides whether owner review is needed':'owner review required'}.</p>
          <details><summary>Models and complete contract</summary><p>Draft model: {proposed.proposal.actor_profile} · Review models: {proposed.proposal.critic_profiles.join(', ')}</p><pre>{JSON.stringify(proposed.proposal,null,2)}</pre><code>Proposal {proposed.proposal_hash}</code></details>
          {#if proposed.proposal.native_harness&&!taskContext}<button disabled={busy} onclick={attachProposalToTask}>Use the source task after it settles</button>{/if}
          {#if taskContext}<p>Attached to {taskContext.title}. {taskContext.reason??''}</p><button disabled={busy} onclick={clearTaskContext}>Use a new task instead</button>{/if}
          <p class="muted">The brief is the agent’s exact proposal. Ask the owning agent for a revised brief if its requirements or references need to change.</p>
        {:else}
          {#if taskContext}<div class="notice"><a href={workTaskHref(taskContext.task_id)}>{taskContext.title}</a> · {taskContext.agent_id}{#if taskContext.reason}<p role="status">{taskContext.reason}</p>{/if}<button disabled={busy} onclick={loadTaskContext}>Refresh task context</button><button disabled={busy} onclick={clearTaskContext}>Use a new task instead</button></div>
          {:else}<p class="muted">Agents can prepare this brief for you. To keep existing ownership and history, <button class="text-button" onclick={()=>switchTab('tasks')}>choose an existing task</button>.</p>{/if}
          <label>What should the draft accomplish?<textarea rows="3" bind:value={goal} disabled={busy||!!taskContext||!!completeContract}></textarea></label>
          {#key draftRevision}<ReferencePicker bind:references bind:pending={referencesPending} scopeEpoch={generation} disabled={busy||!!completeContract}/>{/key}
          <details><summary>Add source notes manually</summary><label>Source notes<textarea rows="4" bind:value={source} disabled={busy||!!completeContract} placeholder="Facts the draft may rely on"></textarea></label></details>
          <label>What makes the result good enough?<textarea rows="3" bind:value={rubric} disabled={busy||!!completeContract} placeholder="Audience, required content and acceptance requirements"></textarea></label>
          <label>Budget ceiling (USD)<input type="number" min="0" step=".01" bind:value={budget} disabled={busy||!!completeContract}/></label>
          <p class="muted">{countLimit(attempts)} attempts · {countLimit(hours)} hours · {humanReview==='required'?'owner reviews accepted drafts':'independent reviewer decides'} · {handoffKind==='capture'?'capture-only communication':'local draft'}.</p>
          <p class="muted">Leave a limit empty or enter 0 for Unlimited. Positive values are caps; unlimited work can incur model and tool charges.</p>
          <details><summary>Acceptance, limits and handoff</summary>
            <div class="fields"><label>Maximum attempts<input type="number" min="0" step="1" bind:value={attempts} disabled={busy||!!completeContract}/></label><label>Time limit (hours)<input type="number" min="0" step="any" bind:value={hours} disabled={busy||!!completeContract}/></label><label>Quality target (0–1)<input type="number" min="0" max="1" step=".01" bind:value={threshold} disabled={busy||!!completeContract}/></label></div>
            <div class="fields"><label>Human review<select bind:value={humanReview} disabled={busy||!!completeContract}><option value="required">Review every accepted draft</option><option value="independent">Independent reviewer decides</option></select></label><label>Handoff<select bind:value={handoffKind} disabled={busy||!!completeContract}><option value="internal_draft">Local task draft</option><option value="capture">Communication capture</option></select></label><label>Panel disagreement<select bind:value={unknownReview} disabled={busy||!!completeContract}><option value="pause">Pause for owner attention</option><option value="revise_supported_defects">Revise a qualified required defect</option></select></label></div>
          </details>
          <details><summary>Models and task ownership</summary><div class="fields">
            <label>Work execution<select bind:value={workMode} disabled={busy||!!taskContext||!!completeContract}><option value="draft">Direct drafting</option><option value="task">Task agent with local file tools</option></select></label>
            {#if workMode==='task'}<label>Steps per attempt<input type="number" min="1" step="1" bind:value={stepsPerAttempt} disabled={busy||!!completeContract}/></label><label>Model calls per attempt<input type="number" min="1" step="1" bind:value={callsPerAttempt} disabled={busy||!!completeContract}/></label>{/if}
            <ModelSelectionControl label="Draft model" models={draftModels} selection={qualitySelection(draftModels,catalogRoutes,actor)}
              savedLabel={actor?'Unavailable draft model':''} disabled={busy||!!completeContract||!draftModels.length} on:change={event=>chooseModel('actor',event.detail)} />
            <ModelSelectionControl label="Independent review model" models={reviewModels} selection={qualitySelection(reviewModels,catalogRoutes,critic)}
              savedLabel={critic?'Choose an independent model':''} disabled={busy||!!completeContract||!reviewModels.length} on:change={event=>chooseModel('critic',event.detail)} />
            {#if !modelsValid&&!completeContract}<p role="status">Choose available draft and review models from different model families. Saved choices stay unchanged until you replace them.</p>{/if}
            <label>Artifact filename<input bind:value={filename} disabled={busy||!!completeContract}/></label><label>Owning agent<select bind:value={agent} disabled={busy||!!taskContext||!!completeContract}><option value="">Choose an agent</option>{#if agent&&!availableAgents.some(a=>a.agent_id===agent)}<option value={agent}>{agent}</option>{/if}{#each availableAgents as owner (owner.agent_id)}<option value={owner.agent_id}>{owner.name||owner.agent_id} · {owner.agent_id}</option>{/each}</select></label><label>Task thread<input bind:value={thread} disabled={busy||!!taskContext||!!completeContract}/></label>
          </div>
          {#if taskContext}<label>Optional campaign capture target<textarea rows="3" bind:value={captureHandoff} disabled={busy} placeholder={'{"campaign_id":"…","version_hash":"…","identity_id":"…"}'}></textarea></label><p class="muted">Copy an exact target from Policies & learning. Acceptance records a local capture under that policy.</p>{/if}</details>
          <details><summary>Complete contract for research, code or media</summary>
            <p>The complete contract controls the sources, file set, tools, checks, sample coverage and limits. Review it again after every edit.</p>
            {#if !completeContract}<button disabled={busy} onclick={editCompleteContract}>Edit complete contract</button>
            {:else}<label>Complete contract JSON<textarea rows="14" bind:value={completeContract} disabled={busy}></textarea></label>{#if completeContractProblem}<p role="alert">{completeContractProblem}</p>{/if}<button disabled={busy} onclick={()=>{completeContract='';preview=null;pendingStart=null;}}>Return to form values</button>{/if}
            {#if audioProviders.length}<details><summary>Available audio review providers</summary><pre>{JSON.stringify(audioProviders,null,2)}</pre></details>{/if}
          </details>
        {/if}
        <div class="admission"><button class:primary={!preview} disabled={!canStart||busy} onclick={reviewContract}>{busy?'Working…':'Review contract and reservation'}</button>
        {#if preview}
          <p>Each attempt reserves up to <strong>{usd(preview.maximum_attempt_microusd)}</strong> for the draft and reviews. This is a reservation, not spending already incurred.</p>
          {#if !preview.budget_can_admit_attempt}<p role="alert" class="error">The budget cannot admit an attempt with these model bounds.</p>{/if}
          <details><summary>Frozen acceptance contract</summary><pre>{JSON.stringify(preview.contract,null,2)}</pre></details>
          {#if preview.native_harness?.metered_tools?.length}<details><summary>Admitted paid tools</summary><p>These installed tool pins, inputs, call limits and pricing share this work's budget.</p><pre>{JSON.stringify(preview.native_harness.metered_tools,null,2)}</pre></details>{/if}
          <button class="primary" disabled={!canStart||busy||!preview.budget_can_admit_attempt} onclick={start}>Start local draft</button>
        {/if}</div>
        {#if taskContext}<details><summary>Recurring task policy</summary>
          {#if taskContext.policy}<p>Enabled for task starts: {taskContext.policy_runs??0}/{taskContext.policy.max_runs} runs accepted; expires {new Date(taskContext.policy.expires_at).toLocaleString()}.</p><button disabled={busy} onclick={()=>configurePolicy(false)}>Disable future quality runs</button>{/if}
          {#if workMode==='task'||proposed?.proposal.native_harness}<div class="fields"><label>Maximum future task runs<input type="number" min="1" step="1" bind:value={policyRunLimit}/></label><label>Policy expires<input type="datetime-local" bind:value={policyExpiry}/></label></div><p>Uses the reviewed contract for future task starts, including scheduled occurrences. {#if preview}Total ceiling: {costLimit(Number(proposal().budget.max_cost_microusd)*policyRunLimit)}.{/if}</p><button disabled={busy||!preview?.budget_can_admit_attempt||!Number.isInteger(policyRunLimit)||policyRunLimit<1||!policyExpiry||!(new Date(policyExpiry).getTime()>Date.now())} onclick={()=>configurePolicy(true)}>Enable reviewed task policy</button>{/if}
        </details>{/if}
        <p class="muted">Outputs remain local. Approval does not send or submit anything.</p>
      </section>
      {/if}
    {#if selected&&!formOpen}<section class="detail run" aria-label="Selected run">
      <div class="section-heading"><div class="eyebrow">Task run</div><div class="actions">{#if requestedCampaignId}<button onclick={()=>{policyTab='capture';tab='policies';}}>Return to capture review</button>{/if}<button aria-label="Close run details" onclick={()=>{detailRequest++;selected=null;clearArtifacts();}}>Close</button></div></div>
      <h2>{selected.contract.goal}</h2>
      <p><strong>{runLabel(selected)}</strong> · {usd(selected.spend.settled_microusd)} settled · {usd(selected.spend.reserved_microusd)} reserved · cumulative ceiling {costLimit((selected.effective_budget??selected.contract.budget).max_cost_microusd)}</p>
      <p>{selected.reason??'Drafting and reviewing…'}</p>
      {#if selected.budget_usage}<p class="muted">Current allowances remaining: {selected.budget_usage.cost.remaining_microusd===null?'Unlimited':usd(selected.budget_usage.cost.remaining_microusd)} · {selected.budget_usage.attempts.remaining===null?'Unlimited':selected.budget_usage.attempts.remaining} attempts · {selected.budget_usage.time.remaining_secs===null?'Unlimited':(selected.budget_usage.time.remaining_secs/3600).toFixed(1)} hours. Total elapsed: {(selected.budget_usage.cumulative.elapsed_wall_clock_secs/3600).toFixed(1)} hours (including waits).</p>{/if}
      {#if selected.status==='budget_exhausted'}
        {#if selected.budget_continuation?.available&&selected.exhausted_limits}
          <section class="notice" aria-label="Budget continuation">
            <h3>Continue this work?</h3>
            <p>The reached allowances below will reset. Other allowances keep their remaining balance. Progress, paid checkpoints and total usage stay with this task. Enter 0 to remove a reached cap.</p>
            <p>Total so far: {selected.attempts.length} attempts · {usd(selected.spend.settled_microusd)} settled · {usd(selected.spend.reserved_microusd)} reserved · {selected.budget_resets?.length??0} approved resets.</p>
            <div class="fields">
              {#if selected.exhausted_limits.cost}<label>New cost allowance (USD)<input type="number" min="0" step=".01" bind:value={resetCost} disabled={busy}/></label>{/if}
              {#if selected.exhausted_limits.time}<label>New time allowance (hours)<input type="number" min="0" step=".001" bind:value={resetHours} disabled={busy}/></label>{/if}
              {#if selected.exhausted_limits.attempts}<label>New attempt allowance<input type="number" min="0" step="1" bind:value={resetAttempts} disabled={busy}/></label>{/if}
            </div>
            <button disabled={busy||selected.generation===undefined||!selected.contract_hash} onclick={continueBudget}>Approve continuation</button>
            <button disabled={busy} onclick={()=>act('cancel')}>Stop work</button>
          </section>
        {:else}<p>{selected.budget_continuation?.reason??'Continuation availability is being checked.'}</p>{/if}
      {/if}
      {#if selected.budget_resets?.length}
        <details><summary>Allowance reset history ({selected.budget_resets.length})</summary>
          {#each selected.budget_resets as reset (reset.id)}<p>{new Date(reset.approved_at).toLocaleString()} · {reset.approved_by} · {reset.reset.cost_microusd!==undefined?`${costLimit(reset.reset.cost_microusd)} cost `:''}{reset.reset.wall_clock_secs!==undefined?`${reset.reset.wall_clock_secs?reset.reset.wall_clock_secs/3600:'Unlimited'} hours `:''}{reset.reset.attempts!==undefined?`${countLimit(reset.reset.attempts)} attempts`:''} · previously {reset.attempts_before} attempts and {usd(reset.spend_before.settled_microusd)} settled.</p>{/each}
        </details>
      {/if}
      <details><summary>Reports and learning</summary><button disabled={busy} onclick={()=>loadReport()}>Inspect run report</button>
      <button disabled={busy} onclick={()=>loadReport(true)}>Download report</button>
      <button disabled={busy} onclick={loadLessons}>Inspect run lessons</button>
      {#if runReport?.report.run_id===selected.id}
        <details open><summary>Result, stop reason and open items</summary>
          <p class="muted">This report is a snapshot. Inspect it again after the run changes.</p>
          {#if runReport.report.output}<button disabled={busy} onclick={showReportedOutput}>Inspect reported artifact</button>{/if}
          <pre>{runReport.markdown}</pre>
        </details>
      {/if}
      {#if terminal(selected.status) && selected.spend.reserved_microusd > 0}
        <button disabled={busy} onclick={()=>act('reconcile-costs')}>Reconcile available cost receipts</button>
      {/if}
      {#if lessons}<details open><summary>Observed feedback for learning review</summary><pre>{JSON.stringify(lessons,null,2)}</pre></details>{/if}
      </details>
      {#if selected.capture_handoff}
        <p>Campaign capture: {selected.capture_handoff.capture?.status.replaceAll('_',' ')??'pending'}{selected.capture_handoff.unavailable_reason?` · ${selected.capture_handoff.unavailable_reason}`:''}</p>
        {#if selected.capture_handoff.capture?.status==='awaiting_review'}<button onclick={()=>{requestedCampaignId=selected?.capture_handoff?.target?.campaign_id??'';policyTab='capture';tab='policies';}}>Review capture policy</button>{/if}
      {/if}
      <p class="muted">Deadline: {new Date(selected.deadline_at).toLocaleString()} · <a href={workTaskHref(selected.admission.task_id)}>Owning task</a></p>
      {#if ownerAsks.length}<div class="owner-asks"><ComposerAskDock targets={ownerAsks} disabled={busy} /></div>{/if}
      {#if selected.routing}
        <p><strong>{selected.handoff?.kind==='capture'?'Communication captured':selected.handoff?'Local draft handed off':selected.owner_approved===false?'Owner declined handoff':selected.routing.route.replaceAll('_',' ')}</strong>: {selected.routing.reason}</p>
        {#if selected.status==='accepted'&&!selected.handoff&&selected.routing.route==='human_review'&&selected.owner_approved===null}<p>Review the accepted artifact below before approving its exact bytes.</p><button disabled={busy||current?.artifact?.hash!==selected.routing.artifact_hash||!artifactMatchesSelection||(!Object.keys(artifact).length&&!Object.keys(binaryFiles).length)} onclick={()=>reviewDraft(true)}>Approve exact draft</button><button disabled={busy} onclick={()=>reviewDraft(false)}>Decline handoff</button>{/if}
      {/if}
      {#if selected.status==='waiting_owner'}
        {#if waitingForTaskAnswer}
          <p>This task needs your answer before it can continue.</p>
          <a href={workTaskHref(selected.admission.task_id)}>Answer task question</a>
        {:else}
          <p>Resolve the issue described above, then retry with the same task inputs.</p>
          <button onclick={()=>act('resume')} disabled={busy||!selected.attention_id}>Resume after resolving blocker</button>
        {/if}
      {/if}
      {#if !terminal(selected.status)||(selected.status==='accepted'&&!selected.handoff&&selected.owner_approved!==false)}<button onclick={()=>act('cancel')} disabled={busy}>Cancel run</button>{/if}
      <details><summary>Attempt history · {selected.attempts.length} attempts</summary><div class="attempts">{#each selected.attempts as attempt}<button class:selected={selectedAttempt===attempt.number} onclick={()=>showAttempt(attempt)}>Attempt {attempt.number} · {attempt.artifact?attempt.decided?'reviewed':terminal(selected.status)?'review incomplete':selected.status==='waiting_owner'?'review blocked':'reviewing':terminal(selected.status)?'draft incomplete':'drafting'}</button>{/each}</div></details>
      {#if current}
        <details><summary>Artifact identity</summary><code>{current.artifact?.hash??'No sealed artifact yet'}</code></details>
        {#each current.review?.criteria??[] as criterion}{#each criterion.reasons as reason}<p class="error"><strong>{criterion.criterion_id}</strong>: {reason}</p>{/each}{/each}
        <details><summary>Independent reviews and findings</summary>
        <div class="table-scroll"><table><thead><tr><th>Reviewer</th><th>Criterion</th><th>Outcome</th><th>Score</th><th>Confidence</th></tr></thead><tbody>{#each Object.entries(current.verdicts) as [criticId,verdict]}{#each verdict.scores as score}<tr><td>{criticId}</td><td>{score.criterion_id}</td><td>{score.outcome}</td><td>{score.score??'—'}</td><td>{score.confidence??'—'}</td></tr>{/each}{#if !verdict.scores.length}<tr><td>{criticId}</td><td colspan="4">{verdict.status}</td></tr>{/if}{/each}</tbody></table></div>
        {#each Object.entries(current.verdicts) as [criticId,verdict]}{#each verdict.findings as finding}<p><strong>{criticId} · {finding.criterion_id}</strong> ({finding.location}): {finding.detail}</p>{/each}{/each}
        </details>
        {#if artifactMatchesSelection&&comparisonLoading}<p class="muted" role="status">Loading previous attempt for comparison…</p>{/if}
        {#if artifactMatchesSelection&&comparisonError}<div class="notice"><p role="status">Previous attempt could not be loaded. The selected artifact is available for review.</p><p class="muted">{comparisonError}</p><button class="comparison-retry" onclick={()=>selected&&current&&loadPreviousAttempt(selected,current)}>Retry previous attempt</button></div>{/if}
        {#if artifactMatchesSelection&&Object.keys(previous).length}<button onclick={()=>comparePrevious=!comparePrevious}>{comparePrevious?'Hide previous attempt':'Compare previous attempt'}</button>{/if}
        {#each Object.entries(artifactMatchesSelection?artifact:{}) as [name,text]}<h3>{name}</h3><div class:comparison={comparePrevious&&previous[name]!==undefined} class="drafts">{#if comparePrevious&&previous[name]!==undefined}<article><h4>Previous attempt</h4><pre>{previous[name]}</pre></article>{/if}<article><h4>Selected attempt</h4><pre>{text}</pre></article></div>{/each}
        {#each Object.entries(artifactMatchesSelection?binaryFiles:{}) as [name,file]}<h3>{name}</h3><p>{file.media_type} · {file.bytes.toLocaleString()} bytes</p>{#if canPreview(file.media_type)}<button disabled={previewLoading===name} onclick={()=>previewArtifact(name)}>{previewLoading===name?'Loading preview…':`Preview ${name}`}</button>{/if}<button onclick={()=>downloadArtifact(name)}>Download {name}</button>{/each}
        {#if artifactPreview&&artifactMatchesSelection&&artifactPreview.key===loadedArtifactKey}
          <figure class="artifact-preview">
            <figcaption>{artifactPreview.name} · exact sealed file</figcaption>
            {#if artifactPreview.mediaType.startsWith('image/')}<img src={artifactPreview.url} alt={`Sealed artifact ${artifactPreview.name}`} />
            {:else if artifactPreview.mediaType.startsWith('audio/')}<audio src={artifactPreview.url} controls preload="metadata" aria-label={`Sealed artifact ${artifactPreview.name}`}></audio>
            {:else}
              <!-- svelte-ignore a11y_media_has_caption (Exact sealed media preview; no fabricated caption track.) -->
              <video src={artifactPreview.url} controls preload="metadata" aria-label={`Sealed artifact ${artifactPreview.name}`}></video>
            {/if}
            <button onclick={clearPreview}>Close preview</button>
          </figure>
        {/if}
        {#if artifactMatchesSelection}<button onclick={inspectArtifactEvidence}>Inspect sealed sources and check evidence</button>{/if}
        {#if artifactEvidence}<details open><summary>Evidence for this artifact · {artifactEvidence.content_hash}</summary><pre>{JSON.stringify(artifactEvidence.evidence,null,2)}</pre></details>{/if}
      {/if}
      <details><summary>Decision history</summary>{#each selected.decisions as decision}<p><strong>{decision.kind.replaceAll('_',' ')}</strong>: {decision.reason}</p>{/each}</details>
      {#if selected.attempts.some(attempt=>attempt.failure)}<details><summary>Attempts needing file repair</summary>
        {#each selected.attempts.filter(attempt=>attempt.failure) as attempt}<p><strong>Attempt {attempt.number}</strong>: {attempt.failure}</p>{/each}
      </details>{/if}
      <p class="muted">Acceptance records the exact reviewed artifact. It does not authorize sending or submitting anything.</p>
    </section>{/if}

    </div>
  {:else if tab==='tasks'}
    <section class="task-catalog">
      <div class="section-heading"><div><h2>Tasks with quality review</h2><p>Review attached runs and recurring policies. To add another task, open Work and choose Improve quality from its menu.</p></div><button disabled={busy} onclick={loadWork}>Refresh work view</button></div>
      <div class="filters"><label class="search-field"><span class="sr-only">Search tasks</span><input aria-label="Search tasks" type="search" bind:value={search} placeholder="Search tasks…"/></label><details><summary>Filter by goal ID</summary><label>Goal identifier (optional)<input bind:value={workGoal} disabled={busy}/></label><button disabled={busy} onclick={loadWork}>Apply goal filter</button></details></div>
      {#if !workView}<p role="status">Loading tasks…</p>{:else if !visibleTasks.length}<div class="empty-state"><h3>No matching tasks</h3><p>Create work or ask an agent to prepare a brief.</p><a class="button" href="/work">Open work</a></div>{/if}
      {#each visibleTasks as target (target.task_id)}<article class="task-row"><div><h3><a href={workTaskHref(target.task_id)}>{target.title}</a></h3><p class="muted">{target.task_status.replaceAll('_',' ')}{target.agent_id?` · ${target.agent_id}`:''}{target.due_date?` · Due ${target.due_date}`:''} · {target.run_count} runs</p>{#if target.policy}<p class="muted">Policy {target.policy.runs_accepted}/{target.policy.max_runs} runs · {target.policy.context_changed?'Task changed; review required':target.policy.expired?'Expired':`Expires ${new Date(target.policy.expires_at).toLocaleDateString()}`}</p>{/if}</div><div class="actions"><button disabled={busy} onclick={async()=>{taskId=target.task_id;await loadTaskContext();tab='inbox';}}>Set up improvement</button>{#if target.runs.length}<a class="button" href={workQualityHref({ run: target.runs[0]?.id }, $routePage.url)}>Latest run</a>{/if}</div></article>{/each}
      {#if total>pageSize}<ServerPager currentPage={page} pageCount={taskPageCount} startItem={(page-1)*pageSize+1} endItem={Math.min(page*pageSize,total)} totalItems={total} ariaLabel="Task pagination" {loading} on:pagechange={event=>page=event.detail.page}/>{/if}
      <div class="notice"><strong>Works with your agent team</strong><p>A coordinator such as your CEO defines the outcome and delegates the task. The assigned agent gathers references and proposes a reviewable brief. Quality checks guide revisions and review the result under the approved limits.</p><p class="muted">Agents use the existing <code>propose_relentless</code> tool. Task ownership, delegation and communication permissions follow the existing agent configuration.</p></div>
    </section>
  {/if}
  {#if campaignVisited||guidanceVisited}
    <section class="policy-workspace" hidden={tab!=='policies'}><div class="section-heading"><div><h2>Policies & learning</h2><p>Configure reusable capture rules and evaluated guidance when you need them.</p></div></div>
      <nav class="subtabs" aria-label="Policy views"><button class:active={policyTab==='capture'} onclick={()=>policyTab='capture'}>Communication captures</button><button class:active={policyTab==='learning'} onclick={()=>policyTab='learning'}>Reusable guidance</button></nav>
      {#if campaignVisited}<div hidden={policyTab!=='capture'}><CampaignWorkspace {selected} {loadedArtifactKey} {requestedCampaignId} scopeEpoch={generation} inspectArtifact={inspectCampaignArtifact}/></div>{/if}
      {#if guidanceVisited}<div hidden={policyTab!=='learning'}><GuidanceWorkspace scopeEpoch={generation}/></div>{/if}
      {#if selected}<div class="notice"><strong>{selected.contract.goal}</strong><p>Selected artifact: <code>{current?.artifact?.hash??'No sealed artifact'}</code></p><button onclick={()=>tab='runs'}>Open artifact details</button></div>{/if}
    </section>
  {/if}
</div>
<style>
  .quality-masthead {
    align-items: flex-start;
    background:
      radial-gradient(ellipse at top right, color-mix(in srgb, var(--accent-primary) 12%, transparent), transparent 60%),
      linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 96%, var(--bg-soft) 4%), var(--bg-card));
    border: 1px solid var(--border-soft);
    border-radius: 12px;
    display: flex;
    flex-wrap: wrap;
    gap: 1.25rem;
    justify-content: space-between;
    padding: 1.25rem 1.5rem;
    margin-bottom: 1.25rem;
    box-sizing: border-box;
  }
  .quality-masthead__identity {
    max-width: min(48rem, 100%);
    min-width: 0;
  }
  .quality-masthead__badge-row {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 0.6rem;
    margin-bottom: 0.4rem;
  }
  .masthead-badge {
    align-items: center;
    background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
    border-radius: var(--radius-chip, 6px);
    color: var(--accent-primary);
    display: inline-flex;
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    padding: 0.2rem 0.55rem;
    text-transform: uppercase;
  }
  .masthead-count {
    color: var(--text-muted);
    font-size: 0.76rem;
    font-weight: 500;
  }
  .quality-masthead h2 {
    font-family: var(--font-display, var(--font-primary));
    font-size: 1.85rem;
    font-weight: 700;
    letter-spacing: -0.02em;
    line-height: 1.15;
    margin: 0.15rem 0 0.4rem;
    color: var(--text-primary);
  }
  .quality-masthead p {
    color: var(--text-secondary);
    font-size: 0.92rem;
    line-height: 1.45;
    margin: 0;
  }
  .quality-masthead__actions {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 0.45rem;
    align-self: flex-start;
  }
  .quality-masthead__actions .action-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    min-height: 1.85rem;
    padding: 0.32rem 0.75rem;
    font-size: 0.78rem;
    font-weight: 600;
    border: 1px solid var(--border-soft);
    border-radius: var(--radius-chip, 6px);
    color: var(--text-secondary);
    background: var(--bg-card);
    cursor: pointer;
    box-sizing: border-box;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
    transition: all 0.15s ease;
  }
  .quality-masthead__actions .action-btn:hover:not(:disabled) {
    color: var(--text-primary);
    border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
    background: var(--bg-soft);
    box-shadow: 0 2px 6px color-mix(in srgb, var(--accent-primary) 8%, rgba(0, 0, 0, 0.04));
  }
  .quality-masthead__actions .action-btn--primary {
    background: var(--accent-primary);
    border-color: var(--accent-primary);
    color: var(--accent-contrast, #ffffff);
  }
  .quality-masthead__actions .action-btn--primary:hover:not(:disabled) {
    filter: brightness(1.08);
    color: var(--accent-contrast, #ffffff);
  }
  .quality-masthead__actions .action-btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .workspace{--workspace-border:var(--border-soft);width:100%;max-width:var(--app-content-max,1320px);margin:0 auto;box-sizing:border-box;padding:1.5rem 1.45rem 2rem;color:var(--text-primary);font-size:.875rem}
  .section-heading,.actions,.row-title,.origin,.budget-summary{display:flex;align-items:center;gap:.75rem}.eyebrow{font-size:.7rem;color:var(--text-muted);letter-spacing:.08em;text-transform:uppercase;font-weight:600}.section-heading p{margin:0;color:var(--text-secondary);line-height:1.5}.actions{flex-wrap:wrap}.section-heading{justify-content:space-between;align-items:flex-start;margin-bottom:1rem}.section-heading>.muted{flex-shrink:0;white-space:nowrap}.section-heading h2{margin:0 0 .35rem;font-size:1.1rem;letter-spacing:-.02em}.section-heading p{font-size:.8rem}.tabs,.subtabs{display:flex;gap:.4rem;border-bottom:1px solid var(--border-soft);margin-bottom:1.25rem;overflow-x:auto}.tabs button,.subtabs button{border:0;border-bottom:2px solid transparent;border-radius:0;background:transparent;white-space:nowrap;padding:.75rem .85rem;color:var(--text-secondary)}.tabs .active,.subtabs .active{border-bottom-color:var(--accent-primary);color:var(--text-primary);font-weight:600}.count{margin-left:.4rem;padding:.15rem .4rem;border-radius:20px;background:var(--bg-soft);font-size:.7rem;color:var(--text-secondary)}
  .work-layout{display:grid;gap:1rem;align-items:start}.work-layout.has-detail{grid-template-columns:minmax(260px,.85fr) minmax(0,1.55fr)}.catalog,.detail,.task-catalog,.policy-workspace{min-width:0;border:1px solid var(--border-soft);background:var(--bg-card);border-radius:var(--radius-lg,16px);padding:1.2rem}.detail{max-height:calc(100dvh - 260px);overflow:auto;scrollbar-gutter:stable}.work-list{max-height:calc(100dvh - 410px);min-height:15rem;overflow:auto}.filters{display:flex;gap:.65rem;align-items:flex-start;margin-bottom:1rem}.filters label{margin:0;min-width:0}.has-detail .filters{flex-wrap:wrap}.has-detail .filters .search-field{flex-basis:100%}.search-field{flex:1}.work-row{display:block;text-align:left;width:100%;padding:1rem .8rem;border:0;border-bottom:1px solid var(--border-soft);border-radius:0;background:transparent;margin:0;min-width:0}.work-row:hover{background:var(--bg-soft)}.work-row.selected{background:var(--accent-primary-soft);box-shadow:inset 3px 0 var(--accent-primary)}.row-title{display:grid;grid-template-columns:minmax(0,1fr) auto;align-items:start;line-height:1.45}.row-title strong{min-width:0;font-weight:550;overflow-wrap:anywhere;display:-webkit-box;-webkit-line-clamp:2;line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}.row-meta{display:block;color:var(--text-secondary);font-size:.75rem;margin-top:.4rem}.work-status{display:inline-block;flex-shrink:0;font-size:.68rem;padding:.2rem .45rem;border-radius:5px;background:var(--bg-soft);color:var(--text-secondary);font-weight:500;white-space:nowrap}.work-status.review{background:var(--status-attention-soft);color:var(--text-primary)}.work-status.attention{background:var(--status-paused-soft);color:var(--text-primary)}.work-status.completed{background:var(--status-completed-soft);color:var(--text-primary)}.empty-state{text-align:center;padding:3.5rem 1rem;color:var(--text-secondary);max-width:32rem;margin:auto}.empty-state h3{color:var(--text-primary);margin:0 0 .5rem;font-size:1rem}.empty-state p{line-height:1.6}.notice{padding:1rem;border:1px solid var(--border-soft);border-radius:10px;background:var(--bg-soft);margin:.75rem 0;line-height:1.55}.notice p{margin:.5rem 0 0;overflow-wrap:anywhere}.comparison-retry{min-height:44px}.origin{flex-wrap:wrap;font-size:.8rem;color:var(--text-secondary);line-height:1.5}.origin code{font-size:.7rem}.brief-goal{font-size:1.15rem;line-height:1.5;margin:.7rem 0}.budget-summary{flex-wrap:wrap;justify-content:space-between;background:var(--bg-soft);padding:.9rem;border-radius:10px;margin:1rem 0;font-size:.75rem;color:var(--text-secondary)}.budget-summary strong{display:block;font-size:1.1rem;color:var(--text-primary);margin-bottom:.2rem}.criterion{border-left:2px solid var(--border-soft);padding:.2rem 0 .2rem .75rem;margin:.6rem 0}.criterion p{font-size:.8rem;color:var(--text-secondary);margin:.4rem 0;line-height:1.6}.criterion strong{font-size:.85rem}.admission{margin:1.2rem 0 .8rem;padding-top:1rem;border-top:1px solid var(--border-soft)}
  label{display:flex;flex-direction:column;gap:.4rem;margin:.8rem 0;font-size:.8rem;font-weight:500}input,select,textarea{padding:.6rem .7rem;border:1px solid var(--input-border,var(--border-soft));border-radius:var(--radius-sm,8px);background:var(--input-bg,var(--bg-card));font:inherit;color:var(--text-primary);width:100%;min-width:0;box-sizing:border-box}textarea{resize:vertical;line-height:1.5}input:disabled,select:disabled,textarea:disabled{opacity:.65}.fields{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:.5rem 1rem}button,.button{display:inline-flex;align-items:center;justify-content:center;padding:.55rem .8rem;border:1px solid var(--border-soft);border-radius:var(--radius-sm,8px);background:var(--bg-card);font:inherit;color:var(--text-primary);text-decoration:none;cursor:pointer;line-height:1.3}button:hover,.button:hover{background:var(--bg-soft)}button:disabled{opacity:.5;cursor:default}button.primary{background:var(--button-primary-bg,var(--accent-primary));color:var(--button-primary-color,var(--text-on-accent));border-color:var(--accent-primary)}button.primary:hover{background:var(--accent-primary-hover)}button.text-button{padding:0;border:0;color:var(--accent-primary);background:transparent}.detail>button,.detail details button{margin:.2rem .25rem .2rem 0}button:focus-visible,a:focus-visible,input:focus-visible,select:focus-visible,textarea:focus-visible,summary:focus-visible{outline:2px solid var(--accent-primary);outline-offset:3px}a{color:var(--text-primary);text-underline-offset:3px}h3{font-size:.95rem;margin:1.15rem 0 .6rem}p{line-height:1.6}details{border-top:1px solid var(--border-soft);margin-top:.75rem;padding-top:.25rem}summary{cursor:pointer;padding:.6rem 0;line-height:1.5;font-weight:500;font-size:.8rem}code{overflow-wrap:anywhere;font-size:.75rem}pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:26rem;overflow:auto;font-size:.8rem;line-height:1.6;background:var(--bg-soft);padding:.8rem;border-radius:8px}.muted{color:var(--text-secondary);font-size:.8rem}.error{color:var(--status-failed)}.drafts{display:grid;gap:1rem}.comparison{grid-template-columns:1fr 1fr}article{min-width:0}article h4{font-size:.75rem;color:var(--text-secondary);margin:.5rem 0}.table-scroll{overflow:auto}table{width:100%;font-size:.75rem;border-collapse:collapse}th,td{text-align:left;padding:.6rem;border-bottom:1px solid var(--border-soft)}.attempts{display:flex;flex-wrap:wrap;gap:.35rem}.attempts button{font-size:.75rem}.attempts button.selected{background:var(--accent-primary-soft);border-color:var(--accent-primary)}.task-row{display:flex;justify-content:space-between;align-items:center;gap:1rem;padding:1rem 0;border-bottom:1px solid var(--border-soft)}.task-row h3{margin:0}.task-row p{margin:.4rem 0 0}.artifact-preview{margin:1rem 0;display:flex;flex-direction:column;align-items:flex-start;gap:.75rem}.artifact-preview img,.artifact-preview video{max-width:100%;max-height:36rem}.artifact-preview figcaption{overflow-wrap:anywhere}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}
  .owner-asks :global(.composer-ask){width:100%;margin:.75rem 0;box-sizing:border-box;box-shadow:none}
  @media(min-width:1500px){.work-layout.has-detail{grid-template-columns:minmax(320px,1fr) minmax(0,2fr)}}
  @media(max-width:1000px){.workspace{padding:1rem}.work-layout.has-detail{grid-template-columns:minmax(240px,.7fr) minmax(0,1.3fr)}.row-title{flex-direction:column;gap:.35rem}.comparison{grid-template-columns:1fr}}
  @media(max-width:720px){.workspace{padding:.85rem}.tabs button{padding:.65rem .55rem;font-size:.8rem}.work-layout.has-detail{display:flex;flex-direction:column-reverse}.work-layout>*{width:100%;box-sizing:border-box}.detail{max-height:none}.work-layout.has-detail .catalog{display:none}.catalog,.detail,.task-catalog,.policy-workspace{padding:.95rem;border-radius:12px}.work-list{max-height:none}.filters{flex-wrap:wrap}.search-field{min-width:12rem}.row-title{flex-direction:row}.task-row{align-items:flex-start;flex-direction:column}.fields{grid-template-columns:1fr}.origin{align-items:flex-start}.budget-summary{gap:1rem}.actions{gap:.4rem}}
</style>
