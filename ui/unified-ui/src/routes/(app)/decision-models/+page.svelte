<script lang="ts">
 import { onMount, onDestroy } from 'svelte';
 import { page } from '$app/stores';
 import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
 import DashboardChrome from '$lib/magician/dashboard/DashboardChrome.svelte';
 import DecisionRoutingPanel from '$lib/plane/DecisionRoutingPanel.svelte';
 import { metricsRequest, mergeStats, groupStats, percentile, rate, type Dashboard, type DailyRow, type ReviewCase } from '$lib/plane/decisionMetrics';
 let data:Dashboard|null=null, days=7, operation='', model='', cohort='', error='', loading=false, alive=true;
 let controller:AbortController|undefined, generation=0, casesGeneration=0;
 let cases:ReviewCase[]=[], selectedCase:ReviewCase|null=null, casesError='', casesLoading=false, reviewed='', saving=false;
 let review:Record<string,Record<string,string>>={};
 let casesController:AbortController|undefined;
 let nextBefore:number|null=null, nextBeforeId:string|null=null;
 let showSettings=false;
 $: today=Math.floor((data?.generated_at_ms??Date.now())/86400000);
 $: currentRows=(data?.rows??[]).filter(r=>r.day>=today-days+1 && (!model||r.model===model||r.kind==='sampling'));
 $: qualityRows=currentRows.filter(r=>r.kind==='quality'&&(!cohort||r.cohort===cohort));
 $: quality=mergeStats(qualityRows);
 $: previousQuality=mergeStats((data?.rows??[]).filter(r=>r.kind==='quality'&&r.day<today-days+1&&r.day>=today-2*days+1&&(!model||r.model===model)&&!!cohort&&r.cohort===cohort));
 $: usageRows=(data?.rows??[]).filter(r=>r.day>=today-days+1&&r.kind==='usage');
 $: decisionUsage=mergeStats(usageRows.filter(r=>r.identity.category==='decision'&&(!model||r.model===model)));
 $: usage=mergeStats(usageRows);
 $: usageGroups=groupStats(usageRows);
 $: monitoring=mergeStats(usageRows.filter(r=>r.identity.category==='monitoring'));
 $: sampling=mergeStats(currentRows.filter(r=>r.kind==='sampling'));
 $: operations=[...new Set((data?.rows??[]).map(r=>r.operation))].sort();
 $: models=[...new Set((data?.rows??[]).map(r=>r.model).filter(Boolean))].sort();
 $: cohorts=[...new Map(currentRows.filter(r=>r.kind==='quality').map(r=>[r.cohort,r])).values()];
 $: heads=[...new Set(qualityRows.flatMap(r=>Object.keys(r.stats.counts).filter(k=>k.startsWith('agreement:')||k.startsWith('review:')).map(k=>k.split(':')[1])))].sort();
 $: daily=Array.from({length:days},(_,i)=>today-days+1+i).map(day=>({day,quality:mergeStats(qualityRows.filter(r=>r.day===day)),usage:mergeStats(usageRows.filter(r=>r.day===day))}));
 const count=(s:{counts:Record<string,number>},k:string)=>s.counts[k]??0;
 const usd=(n:number)=>`$${n.toFixed(4)}`;
 const latency=(n:number|null)=>n===null?'Unavailable':n<1000?`~${n} ms`:`~${(n/1000).toFixed(2)} s`;
 async function load(){
  const id=++generation;controller?.abort();controller=new AbortController();loading=true;error='';
  selectedCase=null;cases=[];casesGeneration++;casesController?.abort();
  try {const params=new URLSearchParams({days:String(days),...(operation?{operation}:{})});const next=await metricsRequest<Dashboard>(`?${params}`,controller.signal);if(alive&&id===generation)data=next;}
  catch(e){if(alive&&id===generation && !(e instanceof DOMException&&e.name==='AbortError')){error=e instanceof Error?e.message:String(e);data=null;}}
  finally{if(alive&&id===generation)loading=false;}
 }
 async function loadCases(before?:number){
  const id=++casesGeneration;casesController?.abort();casesController=new AbortController();casesLoading=true;casesError='';selectedCase=null;
  try {const params=new URLSearchParams({days:String(days),...(operation?{operation}:{}),...(before?{before:String(before),before_id:nextBeforeId??'~'}:{})});const reply=await metricsRequest<{cases:ReviewCase[];next_before:number|null;next_before_id:string|null}>(`/cases?${params}`,casesController.signal);if(alive&&id===casesGeneration){cases=reply.cases;nextBefore=reply.next_before;nextBeforeId=reply.next_before_id;}}
  catch(e){if(alive&&id===casesGeneration&&!(e instanceof DOMException&&e.name==='AbortError'))casesError=e instanceof Error?e.message:String(e);}
  finally{if(alive&&id===casesGeneration)casesLoading=false;}
 }
 function selectCase(c:ReviewCase){selectedCase=c;reviewed='';review={};for(const item of c.primary?.items??[]){if(!item.response||!c.projection?.[item.item_id])continue;review[item.item_id]={};for(const head of c.primary?.required_questions??[]) review[item.item_id][head]=c.reviews[item.item_id]?.[head]===undefined?'':String(c.reviews[item.item_id][head]);}}
 $: reviewInvalid=!Object.keys(review).length||Object.values(review).some(labels=>Object.values(labels).some(v=>!v));
 async function saveReview(){
  const c=selectedCase;if(!c||reviewInvalid)return;saving=true;casesError='';reviewed='';
  const labels:Record<string,Record<string,unknown>>={};for(const [id,heads] of Object.entries(review)){labels[id]={};for(const [h,v] of Object.entries(heads)){const a=c.primary?.items.find(i=>i.item_id===id)?.response?.answers[h];labels[id][h]=a?.noul!==undefined?v==='true':v;}}
  try{const result=await metricsRequest<{revision:number}>('/review',undefined,{comparison_id:c.id,revision:c.review_revision,labels});c.review_revision=result.revision;c.reviews=labels;if(selectedCase?.id===c.id)selectedCase={...c};reviewed='Human review saved. This does not qualify the gate automatically.';await refreshSummary();}
  catch(e){casesError=e instanceof Error?e.message:String(e);}finally{saving=false;}
 }
 async function refreshSummary(){const id=generation;try{const next=await metricsRequest<Dashboard>(`?${new URLSearchParams({days:String(days),...(operation?{operation}:{})})}`);if(alive&&id===generation)data=next;}catch(e){if(alive&&id===generation)error=String(e);}}
 onMount(()=>{operation=$page.url.searchParams.get('operation')??'';let key='';const unsubscribe=scopeIdentityStore.subscribe(scope=>{const next=`${scope.principal}:${scope.workspace}`;if(next!==key){key=next;data=null;model='';cohort='';load();}});return unsubscribe;});
 onDestroy(()=>{alive=false;generation++;controller?.abort();casesController?.abort();});
</script>
<svelte:head><title>Decision models · Magician</title></svelte:head>
<DashboardChrome title="Decision models" summary="Track model quality, real application outcomes, latency and observed pipeline cost." lastRefreshed={data?.generated_at_ms??null} on:refresh={load}>
 <div class="decision-dashboard">
  <nav><a href="/llm">LLM usage and costs</a> · <a href="/settings">Engine settings</a></nav>
  <div class="filters">
   <label>Period<select bind:value={days} onchange={load}><option value={1}>Today (UTC)</option><option value={7}>Last 7 days</option><option value={30}>Last 30 days</option></select></label>
   <label>Operation<select bind:value={operation} onchange={()=>{model='';cohort='';load();}}><option value="">All operations</option>{#if operation&&!operations.includes(operation)}<option value={operation}>{operation}</option>{/if}{#each operations as name}<option value={name}>{name.replaceAll('_',' ')}</option>{/each}</select></label>
   <label>Decision model (quality)<select bind:value={model} onchange={()=>{cohort='';}}><option value="">All models</option>{#if model&&!models.includes(model)}<option value={model}>{model} · no evidence</option>{/if}{#each models as name}<option value={name}>{name}</option>{/each}</select></label>
   <label>Quality cohort<select bind:value={cohort}><option value="">All cohorts (mixed)</option>{#if cohort&&!cohorts.some(r=>r.cohort===cohort)}<option value={cohort}>Selected cohort · no evidence</option>{/if}{#each cohorts as row}<option value={row.cohort}>{row.model} · {String(row.identity.pack_version??'')} · {row.cohort.slice(0,8)}</option>{/each}</select></label>
  </div>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if loading}<p role="status">Loading decision summaries…</p>{/if}
  {#if data}
   {#if data.capture_dropped||data.write_errors||data.truncated}<p role="alert">Evidence is incomplete: {data.capture_dropped} capture drops, {data.write_errors} write errors{data.truncated?', summary row limit reached':''}. These process-wide diagnostics reset on restart; gaps cannot be treated as success.</p>{/if}
   {#if !data.collected_since_ms}<p>No decision evidence collected yet. History begins after this dashboard’s collector is deployed.</p>{:else}<p class="muted">Collecting since {new Date(data.collected_since_ms).toLocaleString()}. UTC daily summaries; updates can take up to 5 seconds. Model and workflow percentiles are approximate histogram bounds.</p>{/if}
   <div class="cards">
    <article><h3>Applied decisions</h3><strong>{count(quality,'applied_decision')}</strong><p>{count(quality,'applied_llm')} applied LLM results · {count(quality,'application_unknown')} without an application receipt · {count(quality,'eligible')} eligible answers</p></article>
    <article><h3>Application outcomes</h3><strong>{count(quality,'application:failed')} failed</strong><p>{count(quality,'application:stale')} stale · {count(quality,'application:preserved')} preserved · {count(quality,'application:revoked')} revoked · {count(quality,'application:unresolved')} unresolved</p></article>
    <article><h3>Observed pipeline cost</h3><strong>{usd(usage.cost_usd)}</strong><p>{usd(monitoring.cost_usd)} monitoring · {count(quality,'llm_receipt_missing')} LLM outcomes missing a linked receipt · {count(usage,'priced_calls')}/{count(usage,'calls')} priced calls</p>{#if count(usage,'priced_calls')!==count(usage,'calls')||count(quality,'llm_receipt_missing')}<p>Partial cost: unpriced calls are unknown.</p>{/if}</article>
    <article><h3>Decision model latency</h3><strong>{latency(percentile(decisionUsage.model_latency,.5))}</strong><p>p95 {latency(percentile(decisionUsage.model_latency,.95))}</p></article>
    <article><h3>Workflow latency</h3><strong>{latency(percentile(quality.workflow_latency,.5))}</strong><p>p95 {latency(percentile(quality.workflow_latency,.95))} · includes decision, fallback and persistence</p></article>
   </div>
   <h2>Quality</h2>
   <p>LLM agreement is a comparison, not ground truth. Human correctness uses explicitly reviewed frozen inputs. Review is selective; the confidence interval does not remove selection bias. {cohort?'Showing one cohort.':'Select one cohort for a fair comparison over time; pooled results can mix models and thresholds.'}</p>
   <div class="table-wrap"><table><thead><tr><th>Question</th><th>LLM agreement</th><th>Reviewed decision correctness</th><th>Reviewed LLM correctness</th><th>Previous period, same cohort</th></tr></thead><tbody>{#each heads as h}<tr><th>{h.replaceAll('_',' ')}</th><td>{rate(count(quality,`agreement:${h}:correct`),count(quality,`agreement:${h}:n`))}</td><td>{rate(count(quality,`review:${h}:correct`),count(quality,`review:${h}:n`))}</td><td>{rate(count(quality,`baseline:${h}:correct`),count(quality,`baseline:${h}:n`))}</td><td>{cohort?rate(count(previousQuality,`review:${h}:correct`),count(previousQuality,`review:${h}:n`)):'Select a cohort'}</td></tr>{/each}</tbody></table></div>
   {#if !heads.length}<p>No comparison or human review evidence in this selection.</p>{/if}
   <p>Reviewed needs-reply misses: {rate(count(quality,'needs_reply_missed'),count(quality,'needs_reply_positive'))}. Lower is better.</p>
   <h2>Comparison health</h2><p>Last completed comparison: {data.last_comparison_ms?new Date(data.last_comparison_ms).toLocaleString():'Unavailable'}.</p><p>Completed and unsuccessful statuses are reported separately. Comparison counters cover the selected operation, across its models.</p>
   <div class="chips">{#each Object.entries(sampling.counts).filter(([k])=>k.startsWith('sampling:')) as [k,n]}<span>{k.slice(9).replaceAll('_',' ')}: {n}</span>{/each}</div>
   <h2>Daily trend</h2>
   <div class="table-wrap"><table><thead><tr><th>UTC day</th><th>Applied decision / LLM</th><th>Reviewed lane correctness</th><th>Observed cost</th><th>Calls priced</th></tr></thead><tbody>{#each daily as d}<tr><td>{new Date(d.day*86400000).toISOString().slice(0,10)}</td><td>{count(d.quality,'applied_decision')} / {count(d.quality,'applied_llm')}</td><td>{rate(count(d.quality,'review:lane:correct'),count(d.quality,'review:lane:n'))}</td><td>{usd(d.usage.cost_usd)}</td><td>{count(d.usage,'priced_calls')}/{count(d.usage,'calls')}</td></tr>{/each}</tbody></table></div>
   <h2>Tokens, cache and cost by model</h2><p>Cost includes observed physical Decision Model attempts and linked caller/reference receipts. Missing receipts and incomplete provider retries can leave totals partial. Usage spans all models and quality cohorts in the selected operation; it includes caller/fallback models even when the quality filter selects Jev.</p>
   <div class="table-wrap"><table><thead><tr><th>Model / category</th><th>Calls / success</th><th>Cost / pricing coverage</th><th>Input / output tokens</th><th>Cache read / write tokens</th><th>Cache reporting coverage</th></tr></thead><tbody>{#each usageGroups as {row,stats:s}}<tr><td>{row.model}<br /><small>{String(row.identity.category)}</small></td><td>{count(s,'calls')}<br /><small>{count(s,'success_known')?`${count(s,'succeeded')}/${count(s,'success_known')} succeeded`:'Success reporting unavailable'}</small></td><td>{usd(s.cost_usd)} · {count(s,'priced_calls')}/{count(s,'calls')}</td><td>{s.input_tokens} / {s.output_tokens}<br /><small>known for {count(s,'token_known')}/{count(s,'calls')} calls</small></td><td>{s.cache_read_tokens} / {s.cache_write_tokens}</td><td>{count(s,'cache_read_known')} / {count(s,'cache_write_known')} of {count(s,'calls')}</td></tr>{/each}</tbody></table></div>
   <details><summary>Cohort identities</summary>{#each cohorts as row}<p><strong>{row.cohort.slice(0,12)}</strong> · {row.operation} · {row.model}</p><pre>{JSON.stringify(row.identity,null,2)}</pre>{/each}</details>
  {/if}
  <h2>Recent cases and human review</h2><p>Recent evidence is bounded to 7 days and 2,048 comparisons. Sampled channel classifications retain their body-blind projection for review. Other operations show closed decision labels only.</p>
  <button onclick={()=>loadCases()} disabled={casesLoading}>Load recent cases</button>
  {#if nextBefore&&cases.length===25}<button onclick={()=>loadCases(nextBefore!)} disabled={casesLoading}>Older cases</button>{/if}
  {#if casesError}<p role="alert">{casesError}</p>{/if}
  <div class="case-list">{#each cases as c}<button onclick={()=>selectCase(c)}>{new Date(c.at_ms).toLocaleString()} · {c.operation} · {c.reference_status??'not sampled'} · {c.id.slice(-8)}</button>{/each}</div>
  {#if selectedCase}
   <h3>Review {selectedCase.id}</h3>
   {#if !selectedCase.projection}<p>Frozen input is unavailable for this case. It cannot count toward human correctness.</p>{/if}
   {#each selectedCase.primary?.items??[] as item}
    <article class="review-case"><h4>{item.response?.model.model??'Unanswered'} · {item.item_id.slice(0,10)}</h4>
     {#if selectedCase.projection?.[item.item_id]}<dl>{#each Object.entries(selectedCase.projection[item.item_id]) as [field,value]}<dt>{field.replaceAll('_',' ')}</dt><dd>{value||'—'}</dd>{/each}</dl>{/if}
     {#each selectedCase.primary?.required_questions??[] as head}
      {@const a=item.response?.answers[head]}
      <p>{head}: model {a?.choice??a?.noul??a?.score??'unavailable'} · LLM {String(selectedCase.reference?.labels?.[item.item_id]?.[head]??'unavailable')}</p>
      {#if review[item.item_id]&&a&&(a.choice!==undefined||a.noul!==undefined)}<label>Correct {head}<select bind:value={review[item.item_id][head]}><option value="">Choose your judgment</option>{#each (a.noul!==undefined?['true','false']:(a.options??[])) as value}<option {value}>{value}</option>{/each}</select></label>{/if}
     {/each}
    </article>
   {/each}
   <button onclick={saveReview} disabled={saving||reviewInvalid}>{saving?'Saving…':'Save human review'}</button>{#if reviewed}<p role="status">{reviewed}</p>{/if}
  {/if}
  <h2>Decision settings</h2><button onclick={()=>showSettings=!showSettings}>{showSettings?'Hide settings':'Configure models, thresholds and monitoring'}</button>
  {#if showSettings}<DecisionRoutingPanel initialOperation={operation}/>{/if}
 </div>
</DashboardChrome>
<style>
 .decision-dashboard{padding:1rem;max-width:1400px;margin:auto}h2{margin-top:2rem;font-size:1.2rem}h3{font-size:1rem}p{margin:.6rem 0;font-size:.9rem;overflow-wrap:anywhere}.muted,small{opacity:.75}.filters,.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,220px),1fr));gap:1rem;margin:1rem 0}label{display:flex;flex-direction:column;gap:.4rem;margin:.5rem 0}select,button{padding:.5rem;border:1px solid #8886;border-radius:.4rem;background:var(--bg-secondary,transparent);max-width:100%}button{margin:.3rem}button:disabled{opacity:.5}article{padding:1rem;border:1px solid #8884;border-radius:.6rem}strong{font-size:1.25rem}.table-wrap{overflow:auto}table{width:100%;border-collapse:collapse;font-size:.85rem}td,th{padding:.65rem;text-align:left;border-bottom:1px solid #8884}pre{font-size:.8rem;overflow:auto;max-height:260px}.chips,.case-list{display:flex;flex-wrap:wrap;gap:.4rem}.chips span{border:1px solid #8884;padding:.3rem}.review-case{margin:1rem 0}dl{display:grid;grid-template-columns:minmax(100px,1fr) 3fr;font-size:.85rem;gap:.3rem}dd{margin:0;overflow-wrap:anywhere}dt{font-weight:600}nav a{text-decoration:underline}
</style>
