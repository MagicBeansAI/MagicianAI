<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { page as routePage } from '$app/stores';
  import { workQualityHref } from '$lib/work/routes';
  import ServerPager from '$lib/shared/components/ServerPager.svelte';
  import { request, pagePath, type Pagination, type CampaignCatalog, type CampaignView, type CampaignCapture, type Run } from './api';
  export let selected:Run|null=null;
  export let loadedArtifactKey='';
  export let scopeEpoch=0;
  export let requestedCampaignId='';
  export let inspectArtifact:(runId:string,hash:string,campaignId?:string)=>Promise<void>;
  let observedEpoch=-1,alive=true,busy=false,error='',mounted=false;
  let catalog:CampaignCatalog|null=null;
  let editor='',editing:{id:string;expected_version:string|null}|null=null;
  let reviewedEditor='';
  let recipients:Record<string,string>={};
  let abort:AbortController|null=null;
  let attemptedLoad='';
  let search='',page=1,captureStatus='awaiting_review',capturePages:Record<string,number>={};
  let previousSearch='',lastQuery='',catalogLoading=false,loadRequest=0,queryTimer:ReturnType<typeof setTimeout>|undefined;
  type CaptureWindow={page:number;pageCount:number;total:number;rows:CampaignCapture[];loading:boolean};
  let captureWindows:Record<string,CaptureWindow>={},captureRequests:Record<string,number>={};
  $: if(search!==previousSearch){previousSearch=search;page=1;}
  $: campaignQuery=pagePath('/campaigns',page,10,{q:search});
  $: if(catalog&&campaignQuery!==lastQuery)schedulePage();
  function schedulePage(){lastQuery=campaignQuery;loadRequest++;catalogLoading=true;clearTimeout(queryTimer);queryTimer=setTimeout(()=>void load(),150);}
  $: pageCount=Math.max(1,Math.ceil((catalog?.pagination.total??0)/10));
  $: campaigns=catalog?.campaigns??[];
  $: visibleCampaigns=campaigns;

  $: if(scopeEpoch!==observedEpoch){observedEpoch=scopeEpoch;resetScope();}
  function resetScope(){
    abort?.abort();abort=new AbortController();
    loadRequest++;clearTimeout(queryTimer);lastQuery='';catalogLoading=false;captureWindows={};captureRequests={};catalog=null;editor='';editing=null;reviewedEditor='';recipients={};busy=false;error='';attemptedLoad='';search='';page=1;capturePages={};
  }
  $: if(mounted&&observedEpoch===scopeEpoch&&requestedCampaignId&&attemptedLoad!==`${scopeEpoch}:${requestedCampaignId}`&&!busy){
    attemptedLoad=`${scopeEpoch}:${requestedCampaignId}`;search=requestedCampaignId;void perform(load);
  }
  $: if(editor!==reviewedEditor&&reviewedEditor)reviewedEditor='';
  async function load(){
    clearTimeout(queryTimer);
    const epoch=scopeEpoch,path=pagePath('/campaigns',page,10,{q:search}),seq=++loadRequest;lastQuery=path;catalogLoading=true;
    try{
      const next=await request<CampaignCatalog>(path,undefined,abort?.signal);
      if(!alive||epoch!==scopeEpoch||seq!==loadRequest||campaignQuery!==path)return;
      catalog=next;
      const visibleIds=new Set(next.campaigns.map(view=>view.state.campaign.id));
      captureWindows=Object.fromEntries(Object.entries(captureWindows).filter(([id])=>visibleIds.has(id)));
      const lastPage=Math.max(1,Math.ceil(next.pagination.total/10));
      if(page>lastPage){page=lastPage;return;}
      await Promise.all(next.campaigns.map(view=>loadCaptures(view.state.campaign.id)));
    }catch(e){if(alive&&epoch===scopeEpoch&&seq===loadRequest&&campaignQuery===path)error=String(e);}finally{if(epoch===scopeEpoch&&seq===loadRequest)catalogLoading=false;}
  }
  async function loadCaptures(id:string){
    const epoch=scopeEpoch,currentPage=capturePages[id]??1,status=captureStatus;
    const seq=(captureRequests[id]??0)+1;captureRequests={...captureRequests,[id]:seq};
    captureWindows={...captureWindows,[id]:{page:currentPage,pageCount:1,total:0,rows:[],loading:true}};
    try{
      const result=await request<{captures:CampaignCapture[];pagination:Pagination}>(pagePath(`/campaigns/${id}/captures`,currentPage,20,{status}),undefined,abort?.signal);
      if(!alive||epoch!==scopeEpoch||captureRequests[id]!==seq||captureStatus!==status||!catalog?.campaigns.some(view=>view.state.campaign.id===id))return;
      const lastPage=Math.max(1,Math.ceil(result.pagination.total/20));
      if(currentPage>lastPage){capturePages={...capturePages,[id]:lastPage};await loadCaptures(id);return;}
      captureWindows={...captureWindows,[id]:{page:currentPage,pageCount:lastPage,total:result.pagination.total,rows:result.captures,loading:false}};
    }catch(e){if(alive&&epoch===scopeEpoch&&captureRequests[id]===seq)error=String(e);}
    finally{if(epoch===scopeEpoch&&captureRequests[id]===seq&&captureWindows[id])captureWindows={...captureWindows,[id]:{...captureWindows[id],loading:false}};}
  }
  async function changeCaptureStatus(event:Event){captureStatus=(event.currentTarget as HTMLSelectElement).value;capturePages={};await Promise.all(visibleCampaigns.map(view=>loadCaptures(view.state.campaign.id)));}
  async function changeCapturePage(id:string,next:number){capturePages={...capturePages,[id]:next};await loadCaptures(id);}
  async function perform(action:()=>Promise<void>){
    if(busy)return;const epoch=scopeEpoch;busy=true;error='';
    try{await action();}catch(e){if(alive&&epoch===scopeEpoch)error=String(e);}
    finally{if(epoch===scopeEpoch)busy=false;}
  }
  function edit(view?:CampaignView){
    editing={id:view?.state.campaign.id??crypto.randomUUID(),expected_version:view?.state.campaign.version_hash??null};
    editor=JSON.stringify(view?.state.campaign.spec??{
      name:'',goal_id:null,sender_identity:'',channel:'',message_kind:'',
      targets:[],allowed_contract_hashes:[],content_contract_hash:'',content_template:null,
      maximum_acts:1,maximum_acts_per_day:1,maximum_acts_per_recipient:1,expires_at:'',
      approval:{source_document:'',always_review:true,important_identity_ids:[],allow_automatic_capture:false,
        clean_approvals_before_automatic_capture:null,audit_every:1},
    },null,2);reviewedEditor='';error='';
  }
  async function reviewPolicy(){
    const original=editor,epoch=scopeEpoch;
    const spec=JSON.parse(original);if(!spec||typeof spec!=='object'||Array.isArray(spec))throw new Error('Enter a campaign policy object.');
    const result=await request<{spec:unknown}>('/campaigns/preview',{spec},abort?.signal);
    if(alive&&epoch===scopeEpoch&&editor===original){editor=JSON.stringify(result.spec,null,2);reviewedEditor=editor;error='';}
  }
  async function save(){
    if(!editing||reviewedEditor!==editor)return;
    const target={...editing},spec=JSON.parse(editor),epoch=scopeEpoch;
    await request(`/campaigns/${target.id}`,{expected_version:target.expected_version,spec},abort?.signal);
    if(!alive||epoch!==scopeEpoch)return;editor='';editing=null;reviewedEditor='';await load();
  }
  async function revoke(view:CampaignView){
    const epoch=scopeEpoch;
    await request(`/campaigns/${view.state.campaign.id}`,{expected_version:view.state.campaign.version_hash,spec:null},abort?.signal);
    if(alive&&epoch===scopeEpoch)await load();
  }
  async function capture(view:CampaignView){
    if(!selected?.handoff||selected.handoff.kind!=='capture'||loadedArtifactKey!==`${selected.id}:${selected.handoff.artifact_hash}`)return;
    const campaign=view.state.campaign;
    const epoch=scopeEpoch;
    await request(`/campaigns/${campaign.id}/captures`,{expected_version:campaign.version_hash,
      run_id:selected.id,artifact_hash:selected.handoff.artifact_hash,identity_id:recipients[campaign.id]},abort?.signal);
    if(alive&&epoch===scopeEpoch)await load();
  }
  async function reviewCapture(view:CampaignView,act:CampaignCapture,approved:boolean){
    if(approved&&loadedArtifactKey!==`${act.binding.run_id}:${act.binding.artifact_hash}`)return;
    const epoch=scopeEpoch;
    await request(`/campaigns/${view.state.campaign.id}/captures/${act.id}/review`,{binding_hash:act.binding_hash,approved},abort?.signal);
    if(alive&&epoch===scopeEpoch)await load();
  }
  onMount(()=>{mounted=true;});
  onDestroy(()=>{clearTimeout(queryTimer);alive=false;abort?.abort();});
</script>

<section>
  <h2>Campaign captures</h2>
  <p>Track reviewed artifacts across existing tasks and counterparties. Captures are local records; nothing is sent.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  <button disabled={busy} onclick={()=>perform(load)}>Load campaigns and identities</button>
  <button disabled={busy} onclick={()=>edit()}>New campaign policy</button>
  {#if editing}
    <details open><summary>Advanced owner policy</summary>
      <p>Use exact task context and contract hashes from the work view, and identity IDs from the register below. Set the sender, channel, allowed targets, expiry and finite caps. Automatic capture requires an approved template with finite choices or values from hash-pinned sources. Leave its content hash blank for preview to compute it. Free-form drafts need exact review.</p>
      <label>Campaign policy<textarea rows="18" bind:value={editor} disabled={busy}></textarea></label>
      <button disabled={busy} onclick={()=>perform(reviewPolicy)}>Review policy</button>
      {#if reviewedEditor===editor&&reviewedEditor}
        <pre>{reviewedEditor}</pre>
        <button disabled={busy} onclick={()=>perform(save)}>Approve exact campaign policy</button>
      {/if}
    </details>
  {/if}
  {#if catalog}
    <details><summary>Counterparty identities</summary>
      {#each catalog.identities as identity}<p>{identity.counterparty}: {identity.recipient} · {identity.verified?'verified':'unverified; capture only'}<br/><code>{identity.id}</code></p>{/each}
      {#if !catalog.identities.length}<p>Create targets in the existing counterparty register before activating a campaign.</p>{/if}
    </details>
    <div class="filters"><label>Search campaigns<input type="search" bind:value={search} placeholder="Name or campaign ID"/></label><label>Capture status<select bind:value={captureStatus} onchange={changeCaptureStatus}><option value="awaiting_review">Awaiting review</option><option value="all">All captures</option><option value="captured">Captured</option><option value="rejected">Rejected</option></select></label></div>
    {#if !campaigns.length}<p>{search?'No campaigns match this search.':'No campaigns in this workspace.'}</p>{/if}
    {#each visibleCampaigns as view (view.state.campaign.id)}
      <article id={`campaign-${view.state.campaign.id}`}>
        <h3>{view.state.campaign.spec.name}</h3>
        <p>{view.capture_count}/{view.state.campaign.spec.maximum_acts} capture slots reserved · {view.state.campaign.revoked_at?'revoked':`expires ${new Date(view.state.campaign.spec.expires_at).toLocaleString()}`}</p>
        <p>{view.state.simulated_clean_approvals} clean approvals in capture simulation · {view.state.production_clean_approvals} production trust approvals{view.state.automatic_capture_suspended?' · automatic capture suspended after a rejected audit':''}</p>
        {#each Object.entries(view.state.simulated_clean_approvals_by_identity) as [identityId,count]}
          <p>{catalog.identities.find(identity=>identity.id===identityId)?.recipient??identityId}: {count} consecutive approved template captures</p>
        {/each}
        <button disabled={busy} onclick={()=>edit(view)}>Review successor policy</button>
        <button disabled={busy||!!view.state.campaign.revoked_at} onclick={()=>perform(()=>revoke(view))}>Revoke campaign</button>
        <details><summary>Task capture handoff targets</summary>
          <p>Choose the matching task and include its exact target when reviewing a capture contract or recurring task policy.</p>
          {#each view.state.campaign.spec.targets as target}
            <p>{target.task_id}</p><pre>{JSON.stringify({campaign_id:view.state.campaign.id,version_hash:view.state.campaign.version_hash,identity_id:target.identity_id},null,2)}</pre>
          {/each}
        </details>
        {#each view.targets as target}<p><a href={workQualityHref({ task: target.task_id }, $routePage.url)}>{target.title??target.task_id}</a>{target.due_date?` · due ${target.due_date}`:''}{target.context_changed?' · context changed; review required':''}{target.unavailable?` · ${target.unavailable}`:''}</p>{/each}
        {#if selected?.handoff?.kind==='capture'}
          <label>Capture selected accepted artifact for
            <select value={recipients[view.state.campaign.id]??''} disabled={busy} onchange={(event)=>recipients={...recipients,[view.state.campaign.id]:event.currentTarget.value}}>
              <option value="">Choose a campaign target</option>
              {#each view.state.campaign.spec.targets.filter(target=>target.task_id===selected?.admission.task_id) as target}
                <option value={target.identity_id}>{catalog.identities.find(identity=>identity.id===target.identity_id)?.recipient??target.identity_id}</option>
              {/each}
            </select>
          </label>
          <button disabled={busy||!!view.state.campaign.revoked_at||!recipients[view.state.campaign.id]||loadedArtifactKey!==`${selected.id}:${selected.handoff.artifact_hash}`} onclick={()=>perform(()=>capture(view))}>Bind exact local capture</button>
        {/if}
        {#each (captureWindows[view.state.campaign.id]?.rows??[]) as act (act.id)}
          <p><strong>{act.binding.recipient}</strong> · {act.status.replaceAll('_',' ')} · {act.reason}{act.audit_required?' · audit sample':''}</p>
          <button disabled={busy} onclick={()=>perform(()=>inspectArtifact(act.binding.run_id,act.binding.artifact_hash,view.state.campaign.id))}>Inspect bound artifact</button>
          {#if act.status==='awaiting_review'}
            <button disabled={busy||loadedArtifactKey!==`${act.binding.run_id}:${act.binding.artifact_hash}`} onclick={()=>perform(()=>reviewCapture(view,act,true))}>Approve exact capture</button>
            <button disabled={busy} onclick={()=>perform(()=>reviewCapture(view,act,false))}>Reject capture</button>
          {/if}
        {/each}
        {#if (captureWindows[view.state.campaign.id]?.total??0)>20}<ServerPager currentPage={captureWindows[view.state.campaign.id].page} pageCount={Math.ceil(captureWindows[view.state.campaign.id].total/20)} startItem={(captureWindows[view.state.campaign.id].page-1)*20+1} endItem={Math.min(captureWindows[view.state.campaign.id].page*20,captureWindows[view.state.campaign.id].total)} totalItems={captureWindows[view.state.campaign.id].total} ariaLabel={`Captures for ${view.state.campaign.spec.name}`} loading={captureWindows[view.state.campaign.id].loading} on:pagechange={event=>changeCapturePage(view.state.campaign.id,event.detail.page)}/>{/if}
      </article>
    {/each}
    {#if catalog.pagination.total>10}<ServerPager currentPage={page} {pageCount} startItem={(page-1)*10+1} endItem={Math.min(page*10,catalog.pagination.total)} totalItems={catalog.pagination.total} ariaLabel="Campaign pagination" loading={catalogLoading} on:pagechange={event=>page=event.detail.page}/>{/if}
  {/if}
</section>

<style>
  section{padding:1rem;border:1px solid var(--border-soft);border-radius:var(--radius-md,12px);background:var(--bg-card);color:var(--text-primary)}
  .filters{display:flex;flex-wrap:wrap;gap:1rem}.filters label{flex:1;min-width:12rem}
  article{margin-top:1rem;padding-top:1rem;border-top:1px solid var(--border-soft);min-width:0}
  h2{font-size:1.05rem;margin:0 0 .65rem}h3{font-size:.95rem}p{font-size:.85rem;line-height:1.6}
  textarea,input,select{display:block;width:100%;padding:.65rem;border:1px solid var(--input-border,var(--border-soft));border-radius:8px;background:var(--input-bg,var(--bg-card));color:var(--text-primary);font:inherit;box-sizing:border-box}textarea{font-family:monospace;font-size:.8rem;line-height:1.6;resize:vertical}
  pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:28rem;overflow:auto;font-size:.8rem;background:var(--bg-soft);padding:.75rem;border-radius:8px}code{overflow-wrap:anywhere}
  button{margin:.25rem;padding:.55rem .75rem;border:1px solid var(--border-soft);border-radius:8px;background:var(--bg-card);color:var(--text-primary);font:inherit;font-size:.8rem;cursor:pointer}button:hover{background:var(--bg-soft)}button:disabled{opacity:.5;cursor:default}button:focus-visible,input:focus-visible,textarea:focus-visible,select:focus-visible,summary:focus-visible{outline:2px solid var(--accent-primary);outline-offset:2px}label{display:block;margin-top:.75rem;font-size:.8rem}summary{cursor:pointer;padding:.5rem 0;font-size:.85rem}a{color:var(--text-primary);text-underline-offset:3px}[role="alert"]{color:var(--status-failed)}
</style>
