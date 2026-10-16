<script lang="ts">
 import { page } from '$app/stores';
 import { workReturnHref } from './routes';
 $: returnHref = workReturnHref($page.url.searchParams.get('from'));
 $: filtered = ['/llm', '/history', '/memory', '/events'].includes($page.url.pathname);
 $: agent = filtered ? $page.url.searchParams.get('agent_id') : null;
 $: task = ['/llm', '/events'].includes($page.url.pathname) ? $page.url.searchParams.get('task_id') : null;
 $: execution = $page.url.pathname === '/events' ? $page.url.searchParams.get('execution_id') : null;
</script>
<div class="work-specialist-shell">
 {#if returnHref}
 <nav class="work-specialist-context" aria-label="Work context">
  <a href={returnHref}>← Back to work item</a>
  {#if agent}<span>Member: {agent}</span>{/if}
  {#if task}<span>Task: {task}</span>{/if}
  {#if execution}<span>Run: {execution}</span>{/if}
  {#if !agent && !task && !execution}<span>Workspace tool</span>{/if}
 </nav>
 {/if}
 <div class="work-specialist-body"><slot /></div>
</div>
<style>
 .work-specialist-body :global(button) {min-height:44px;}
 .work-specialist-body :global(input:not([type=checkbox]):not([type=radio])),.work-specialist-body :global(select) {min-height:44px;}
 .work-specialist-shell {display:flex;flex-direction:column;flex:1;min-width:0;min-height:0;height:100%;color:var(--text-primary);background:var(--bg-base);}
 .work-specialist-context {display:flex;flex-wrap:wrap;align-items:center;gap:var(--space-sm);padding:var(--space-sm) var(--space-md);border-bottom:1px solid var(--border-soft);flex-shrink:0;font-size:var(--text-sm);}
 .work-specialist-context a {display:inline-flex;align-items:center;min-height:44px;color:var(--text-primary);text-decoration:none;}
 .work-specialist-context span {color:var(--text-secondary);overflow-wrap:anywhere;min-width:0;}
 .work-specialist-body {flex:1;min-width:0;min-height:0;overflow:auto;}
</style>
