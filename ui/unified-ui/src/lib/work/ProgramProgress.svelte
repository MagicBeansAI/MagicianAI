<script lang="ts">
 import { page } from '$app/stores';
 import { workAgentHref, workTaskHref } from './routes';
 import { activeProgramState, programMetrics, record, text, type ProgramMember } from './programContext';
 export let member: ProgramMember;
 export let program = '';
 $: state = activeProgramState(member);
 $: source = record(state.program);
 $: matching = !program || text(source.relative_path) === program;
 $: metrics = program ? programMetrics(member,program) : [];
 $: updates = matching && Array.isArray(state.update_history) ? state.update_history.slice(-5).reverse().map(record) : [];
 $: loops = matching && Array.isArray(state.open_loops) ? state.open_loops : [];
 function evidenceHref(update: Record<string,unknown>): string {
  const url = new URL(workTaskHref(text(update.task_id)), $page.url);
  if (text(update.execution_id)) url.searchParams.set('execution_id',text(update.execution_id));
  url.searchParams.set('from',`${$page.url.pathname}${$page.url.search}`);
  return url.pathname+url.search;
 }
 function readable(value: unknown): string { return typeof value === 'string' ? value : JSON.stringify(value); }
</script>
<article class="program-progress" aria-label={`${member.name} playbook progress`}>
 <h3><a href={workAgentHref(member.id,$page.url)}>{member.name} →</a></h3>
 <p class="muted">Responsible member{member.version !== null ? ` · definition v${member.version}` : ''}</p>
 <ul aria-label="Configured direction">
  {#each member.references.filter(ref => !program || ref.path === program) as ref}
   <li>{ref.path}{ref.section ? ` · ${ref.section}` : ''}{ref.focus ? ` · focus: ${ref.focus}` : ''}</li>
  {/each}
 </ul>
 {#if member.error}<p role="alert">{member.error}</p>
 {:else if !member.overview}<p class="muted">Loading current progress…</p>
 {:else}
  {#if matching && Object.keys(state).length}
   <dl>
    <dt>Current source</dt><dd>{text(source.relative_path)}{text(source.section) ? ` · ${text(source.section)}` : ''}</dd>
    {#if text(state.current_phase)}<dt>Phase</dt><dd>{text(state.current_phase)}</dd>{/if}
    {#if text(state.current_step)}<dt>Step</dt><dd>{text(state.current_step)}</dd>{/if}
    {#if text(state.updated_at)}<dt>Last updated</dt><dd>{text(state.updated_at)}</dd>{/if}
    {#if text(state.update_reason)}<dt>Reason</dt><dd>{text(state.update_reason)}</dd>{/if}
   </dl>
   {#if loops.length}<details><summary>Open loops · {loops.length}</summary><ul>{#each loops as loop}<li>{readable(loop)}</li>{/each}</ul></details>{/if}
   {#if state.last_run_summary}<details><summary>Latest cycle summary</summary><p>{readable(state.last_run_summary)}</p></details>{/if}
   {#if updates.length}<details><summary>Recent progress evidence</summary>{#each updates as update}<p>{text(update.reason)}{#if text(update.task_id)} <a href={evidenceHref(update)}>Task{ text(update.execution_id) ? ' and execution' : ''} →</a>{/if}</p>{/each}</details>{/if}
  {:else}<p class="muted">No current runtime progress{program ? ' for this document' : ''}.</p>{/if}
  {#if metrics.length}<details><summary>Evaluation metrics · {metrics.length}</summary><ul>{#each metrics as metric}<li><strong>{text(metric.metric_id)}</strong>: {metric.current ?? 'not observed'} {text(metric.comparator)} {metric.target ?? '—'} · {metric.met === true ? 'Met' : metric.met === false ? 'Not met' : 'Not evaluated'}{text(metric.reason) ? ` · ${text(metric.reason)}` : ''}</li>{/each}</ul></details>{/if}
 {/if}
 <a class="operator" href={workAgentHref(member.id,$page.url)}>Focus health and operating controls →</a>
</article>
<style>
 .program-progress {min-width:0;overflow-wrap:anywhere;border:1px solid var(--border-soft);border-radius:var(--radius-lg);padding:var(--space-md);background:var(--bg-card);color:var(--text-primary);}
 h3,p {margin:0 0 var(--space-sm);} h3 {font-family:var(--font-display);} .muted,dt {color:var(--text-secondary);font-size:var(--text-sm);}
 a {color:var(--text-primary);} h3 a,.operator {display:inline-flex;align-items:center;min-height:44px;} .operator {margin-top:var(--space-sm);}
 dl {display:grid;grid-template-columns:minmax(0,1fr) minmax(0,2fr);gap:var(--space-sm);} dd {margin:0;} li {margin:var(--space-sm) 0;} summary {min-height:44px;display:flex;align-items:center;cursor:pointer;} a:focus-visible,summary:focus-visible {outline:2px solid var(--accent-primary);outline-offset:3px;}
 @media(max-width:430px) {dl {grid-template-columns:minmax(0,1fr);} dd {margin-bottom:var(--space-sm);}}
</style>
