<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { page as routePage } from '$app/stores';
  import { workQualityHref } from '$lib/work/routes';
  import ServerPager from '$lib/shared/components/ServerPager.svelte';
  import { request, pagePath, usd, type Pagination, type GuidancePolicyView, type GuidanceEvidenceView, type GuidanceProposalView } from './api';
  export let scopeEpoch = 0;
  let observedEpoch = -1, alive = true, busy = false, error = '';
  let policies: GuidancePolicyView[] = [];
  let proposals: GuidanceProposalView[] = [];
  let editor = '', editingId = '', reviewedEditor = '', reviewedPolicy: GuidancePolicyView['policy'] | null = null;
  let abort: AbortController | null = null;
  let search='',page=1,previousSearch='',proposalPage=1,total=0,proposalTotal=0;
  let policyRequest=0,proposalRequest=0,lastQuery='',mounted=false,policyLoading=false;
  let queryTimer:ReturnType<typeof setTimeout>|undefined;
  $: if(search!==previousSearch){previousSearch=search;page=1;}
  $: policyQuery=pagePath('/guidance-policies',page,10,{q:search});
  $: if(mounted&&policyQuery!==lastQuery)schedulePage();
  function schedulePage(){lastQuery=policyQuery;policyRequest++;policyLoading=true;clearTimeout(queryTimer);queryTimer=setTimeout(()=>void load(),150);}
  $: pageCount=Math.max(1,Math.ceil(total/10));
  $: visiblePolicies=policies;
  let evidenceRun = '', evidencePolicy = '', evidenceHash = '', evidenceBody = '';
  function clearEvidence() { evidenceRun = ''; evidencePolicy = ''; evidenceHash = ''; evidenceBody = ''; }
  $: if(scopeEpoch!==observedEpoch)resetScope();
  function resetScope(){
    observedEpoch = scopeEpoch; abort?.abort(); abort = new AbortController();
    policies = []; editor = ''; editingId = ''; reviewedEditor = ''; reviewedPolicy = null; busy = false; error = '';
    policyRequest++;proposalRequest++;clearTimeout(queryTimer);lastQuery='';policyLoading=false;total=0;proposalTotal=0;proposals = [];search='';page=1;proposalPage=1;
    clearEvidence();
  }
  $: if (reviewedEditor && editor !== reviewedEditor) { reviewedEditor = ''; reviewedPolicy = null; }
  async function perform(action: () => Promise<void>) {
    if (busy) return; const epoch = scopeEpoch; busy = true; error = '';
    try { await action(); } catch (e) { if (alive && epoch === scopeEpoch) error = String(e); }
    finally { if (epoch === scopeEpoch) busy = false; }
  }
  async function load() {
    clearTimeout(queryTimer);
    const epoch=scopeEpoch,path=policyQuery,seq=++policyRequest;lastQuery=path;policyLoading=true;
    try{
      const result=await request<{policies:GuidancePolicyView[];pagination:Pagination}>(path,undefined,abort?.signal);
      if(!alive||epoch!==scopeEpoch||seq!==policyRequest||policyQuery!==path)return;
      policies=result.policies;total=result.pagination.total;
      const lastPage=Math.max(1,Math.ceil(total/10));if(page>lastPage)page=lastPage;
    }catch(e){if(alive&&epoch===scopeEpoch&&seq===policyRequest&&policyQuery===path)error=String(e);}finally{if(epoch===scopeEpoch&&seq===policyRequest)policyLoading=false;}
  }
  async function loadProposals() {
    const epoch=scopeEpoch,currentPage=proposalPage,seq=++proposalRequest;
    const result=await request<{proposals:GuidanceProposalView[];pagination:Pagination}>(pagePath('/guidance-proposals',currentPage,10),undefined,abort?.signal);
    if(alive&&epoch===scopeEpoch&&seq===proposalRequest&&proposalPage===currentPage){
      proposals=result.proposals;proposalTotal=result.pagination.total;
      const lastPage=Math.max(1,Math.ceil(proposalTotal/10));if(proposalPage>lastPage){proposalPage=lastPage;await loadProposals();}
    }
  }
  async function changeProposalPage(next:number){proposalPage=next;await perform(loadProposals);}
  function useProposal(proposal: GuidanceProposalView) {
    create();
    const spec = JSON.parse(editor);
    spec.name = proposal.guidance.title; spec.candidate_id = proposal.candidate_id;
    spec.agent_id = proposal.agent_id ?? ''; spec.training_run_ids = proposal.training_run_ids;
    editor = JSON.stringify(spec,null,2);
  }
  function create() {
    clearEvidence();
    editingId = crypto.randomUUID(); reviewedEditor = ''; reviewedPolicy = null;
    editor = JSON.stringify({name:'',candidate_id:'',agent_id:'',ui_thread_id:'',baseline_guidance:null,
      training_run_ids:[],evaluation_cases:[],canary_cases:[],
      evaluation_requirements:{metric:'weighted_quality',minimum_mean_gain:0,maximum_case_regression:0,minimum_candidate_acceptance_rate:1,maximum_acceptance_regressions:0},
      canary_requirements:{metric:'weighted_quality',minimum_mean_gain:0,maximum_case_regression:0,minimum_candidate_acceptance_rate:1,maximum_acceptance_regressions:0},
      maximum_evaluation_cost_microusd:0,maximum_canary_cost_microusd:0,maximum_guidance_bytes:16384,
      targets:[],maximum_adopted_runs:1,disable_after_regressions:1,unknown_monitor_disables_adoption:true,monitor_nonacceptance_is_regression:true,expires_at:'',
    },null,2);
  }
  async function preview() {
    const epoch = scopeEpoch, original = editor, id = editingId;
    const result = await request<{policy:GuidancePolicyView['policy']}>('/guidance-policies/preview',
      {policy_id:id,spec:JSON.parse(original)},abort?.signal);
    if (alive && epoch === scopeEpoch && editingId === id && editor === original) {
      reviewedPolicy = result.policy; reviewedEditor = original;
    }
  }
  async function activate() {
    if (!reviewedPolicy || reviewedEditor !== editor) return;
    const epoch = scopeEpoch, id = editingId;
    await request(`/guidance-policies/${id}`,{spec:JSON.parse(editor),expected_version_hash:reviewedPolicy.version_hash},abort?.signal);
    if (alive && epoch === scopeEpoch && editingId === id) {
      editingId = ''; editor = ''; reviewedEditor = ''; reviewedPolicy = null; await load();
    }
  }
  async function revoke(view: GuidancePolicyView) {
    const epoch = scopeEpoch;
    await request(`/guidance-policies/${view.policy.id}/revoke`,{expected_version_hash:view.policy.version_hash},abort?.signal);
    if (alive && epoch === scopeEpoch) await load();
  }
  async function inspectEvidence(view: GuidancePolicyView, runId: string) {
    const epoch = scopeEpoch;
    const expectedHash = view.observations[runId]?.evidence_hash ?? view.monitored[runId]?.evidence_hash;
    clearEvidence();
    if (!expectedHash) throw new Error('This run has no sealed policy evidence yet.');
    const result = await request<GuidanceEvidenceView>(`/guidance-policies/${view.policy.id}/evidence/${runId}`,undefined,abort?.signal);
    if (alive && epoch === scopeEpoch) {
      if (!result.sealed || result.content_hash !== expectedHash) throw new Error('Policy evidence changed; refresh its recorded result.');
      evidenceRun = runId; evidencePolicy = view.policy.spec.name; evidenceHash = result.content_hash;
      evidenceBody = JSON.stringify(result.evidence,null,2);
    }
  }
  onDestroy(() => {clearTimeout(queryTimer); alive = false; abort?.abort(); });
  async function resume(view: GuidancePolicyView) {
    const epoch = scopeEpoch;
    await request(`/guidance-policies/${view.policy.id}/resume`,{expected_version_hash:view.policy.version_hash},abort?.signal);
    if (alive && epoch === scopeEpoch) await load();
  }
  onMount(() => {mounted=true;
    const timer = setInterval(() => {
      if (!busy && !editingId && policies.some(view => ['evaluating','canary','qualified','promoted'].includes(view.status))) void perform(load);
    },5000);
    return () => clearInterval(timer);
  });
</script>

<section>
  <h2>Reusable guidance</h2>
  <p>Compare frozen procedural guidance with a baseline on held-out drafts and a separate internal canary. An approved policy can adopt passing guidance on its exact recurring tasks and disable future adoption after declared regressions.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  <button disabled={busy} onclick={() => perform(load)}>Load guidance policies</button>
  <button disabled={busy} onclick={() => perform(loadProposals)}>Load agent guidance proposals</button>
  <button disabled={busy} onclick={create}>New qualification policy</button>
  {#each proposals as proposal (proposal.candidate_id)}
    <details><summary>{proposal.guidance.title} · proposed by {proposal.agent_id ?? 'agent'}</summary>
      <p>Candidate {proposal.candidate_id}{#if proposal.task_id} · <a href={workQualityHref({ task: proposal.task_id }, $routePage.url)}>Source task</a>{/if}</p>
      <pre>{JSON.stringify(proposal.guidance,null,2)}</pre>
      {#each proposal.training_run_ids as runId}<p><a href={workQualityHref({ run: runId }, $routePage.url)}>Training run {runId}</a></p>{/each}
      <p>This proposal activates no guidance. Choose fresh evaluation/canary cases and exact targets in an explicit policy.</p>
      <button disabled={busy} onclick={() => useProposal(proposal)}>Prepare qualification policy</button>
    </details>
  {/each}
  {#if proposalTotal>10}<ServerPager currentPage={proposalPage} pageCount={Math.ceil(proposalTotal/10)} startItem={(proposalPage-1)*10+1} endItem={Math.min(proposalPage*10,proposalTotal)} totalItems={proposalTotal} ariaLabel="Guidance proposal pagination" disabled={busy} on:pagechange={event=>changeProposalPage(event.detail.page)}/>{/if}

  {#if evidenceBody}
    <details open><summary>Sealed evidence · {evidencePolicy} · {evidenceRun}</summary>
      <p>Recorded content hash: {evidenceHash}</p>
      <pre>{evidenceBody}</pre><button disabled={busy} onclick={clearEvidence}>Close evidence</button>
    </details>
  {/if}
  {#if editingId}
    <details open><summary>Owner evaluation and canary policy</summary>
      <p>Choose an existing procedural candidate and terminal training runs. Each evaluation/canary case is an object with an ID and a full internal draft proposal, using the targets’ native actor recipe and artifact kind. Supply fresh cases, a positive evaluation gain floor, finite phase budgets and an expiry. Targets need exact task context and baseline contract hashes from reviewed recurring draft policies. Capture handoffs require separate review.</p>
      <label>Policy<textarea rows="20" bind:value={editor} disabled={busy}></textarea></label>
      <button disabled={busy} onclick={() => perform(preview)}>Review frozen policy and guidance</button>
      {#if reviewedPolicy && reviewedEditor === editor}
        <h3>{reviewedPolicy.guidance.title}</h3>
        <pre>{JSON.stringify(reviewedPolicy.guidance,null,2)}</pre>
        <p>{reviewedPolicy.evaluation.length} evaluation pairs · {reviewedPolicy.canary.length} canary pairs · {reviewedPolicy.spec.maximum_adopted_runs} maximum adopted runs</p>
        <p>Evaluation ceiling {usd(reviewedPolicy.spec.maximum_evaluation_cost_microusd)} · Canary ceiling {usd(reviewedPolicy.spec.maximum_canary_cost_microusd)}</p>
        <details><summary>Exact frozen trial and target configuration</summary><pre>{JSON.stringify(reviewedPolicy,null,2)}</pre></details>
        <button disabled={busy} onclick={() => perform(activate)}>Approve policy and start qualification</button>
      {/if}
    </details>
  {/if}
  <label>Search guidance policies<input type="search" bind:value={search} placeholder="Name, policy or status"/></label>
  {#each visiblePolicies as view (view.policy.id)}
    <article>
      <h3>{view.policy.spec.name}</h3>
      <p>{view.status.replaceAll('_',' ')}{view.expired?' · expired':''} · {view.adoption_enabled?'future adoption enabled':'future adoption disabled'}</p>
      {#if view.reason}<p>{view.reason}</p>{/if}
      <p>Evaluation {usd(view.evaluation_spend.settled_microusd)} settled + {usd(view.evaluation_spend.reserved_microusd)} reserved · Canary {usd(view.canary_spend.settled_microusd)} settled + {usd(view.canary_spend.reserved_microusd)} reserved</p>
      <p>{Object.keys(view.observations).length}/{2*(view.policy.evaluation.length+view.policy.canary.length)} sealed observations · {Object.keys(view.adoptions).length}/{view.policy.spec.maximum_adopted_runs} adoption slots · {view.regressions} regressions</p>
      <p>Expires {view.policy.spec.expires_at}. Future adoption stops after {view.policy.spec.disable_after_regressions} regressions{view.policy.spec.unknown_monitor_disables_adoption?' or unknown monitoring evidence':''}.{view.policy.spec.monitor_nonacceptance_is_regression?' A nonaccepted run counts as a regression.':''}</p>
      <button disabled={busy||view.status==='revoked'} onclick={() => perform(() => revoke(view))}>Revoke future guidance adoption</button>
      {#if view.status==='paused'}<button disabled={busy||view.expired} onclick={() => perform(() => resume(view))}>Resume exact frozen qualification</button>{/if}
      <details><summary>Qualified guidance and approved targets</summary>
        <pre>{JSON.stringify(view.policy.guidance,null,2)}</pre>
        {#each view.policy.spec.targets as target}<p><a href={workQualityHref({ task: target.task_id }, $routePage.url)}>{target.task_id}</a></p>{/each}
      </details>
      <details><summary>Trial results</summary>
        {#each Object.values(view.reservations) as trial}
          <p><a href={workQualityHref({ run: trial.run_id }, $routePage.url)}>{trial.phase} · {trial.case_id} · {trial.variant}</a>
            {#if view.observations[trial.run_id]}
              · {view.observations[trial.run_id].accepted?'accepted':'not accepted'} · {view.observations[trial.run_id].complete?'complete review':'unknown review'}
              {#if view.observations[trial.run_id].quality!==null} · quality {view.observations[trial.run_id].quality}{/if}
              {#if view.observations[trial.run_id].operator_redirects>0} · operator-assisted; cannot qualify{/if}
              <button disabled={busy} onclick={() => perform(() => inspectEvidence(view,trial.run_id))}>Inspect sealed trial evidence</button>
            {:else} · evidence pending{/if}
          </p>
        {/each}
      </details>
      <details><summary>Adopted run monitoring</summary>
        {#each Object.values(view.adoptions) as adoption}
          <p><a href={workQualityHref({ run: adoption.run_id }, $routePage.url)}>{adoption.run_id}</a>
            {#if view.monitored[adoption.run_id]} · {view.monitored[adoption.run_id].regressed?'regression':view.monitored[adoption.run_id].unknown?'unknown evidence':'within declared bounds'}{view.monitored[adoption.run_id].regressed&&view.monitored[adoption.run_id].unknown?' · unknown evidence':''}{:else} · monitoring pending{/if}
            {#if view.monitored[adoption.run_id]}<button disabled={busy} onclick={() => perform(() => inspectEvidence(view,adoption.run_id))}>Inspect sealed monitoring evidence</button>{/if}
          </p>
        {/each}
      </details>
    </article>
  {/each}
  {#if total>10}<ServerPager currentPage={page} {pageCount} startItem={(page-1)*10+1} endItem={Math.min(page*10,total)} totalItems={total} ariaLabel="Guidance pagination" loading={policyLoading} on:pagechange={event=>page=event.detail.page}/>{/if}
</section>

<style>
  section{padding:1rem;border:1px solid var(--border-soft);border-radius:var(--radius-md,12px);background:var(--bg-card);color:var(--text-primary)}
  article{margin-top:1rem;padding-top:1rem;border-top:1px solid var(--border-soft);min-width:0}
  h2{font-size:1.05rem;margin:0 0 .65rem}h3{font-size:.95rem}p{font-size:.85rem;line-height:1.6}
  textarea,input{display:block;width:100%;padding:.65rem;border:1px solid var(--input-border,var(--border-soft));border-radius:8px;background:var(--input-bg,var(--bg-card));color:var(--text-primary);font:inherit;box-sizing:border-box}textarea{font-family:monospace;font-size:.8rem;line-height:1.6;resize:vertical}
  pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:28rem;overflow:auto;font-size:.8rem;background:var(--bg-soft);padding:.75rem;border-radius:8px}
  button{margin:.25rem;padding:.55rem .75rem;border:1px solid var(--border-soft);border-radius:8px;background:var(--bg-card);color:var(--text-primary);font:inherit;font-size:.8rem;cursor:pointer}button:hover{background:var(--bg-soft)}button:disabled{opacity:.5;cursor:default}button:focus-visible,input:focus-visible,textarea:focus-visible,summary:focus-visible{outline:2px solid var(--accent-primary);outline-offset:2px}label{display:block;margin-top:.75rem;font-size:.8rem}summary{cursor:pointer;padding:.5rem 0;font-size:.85rem}a{color:var(--text-primary);text-underline-offset:3px}[role="alert"]{color:var(--status-failed)}
</style>
