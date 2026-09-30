<script lang="ts">
  import { concurrentVoiceStore, markConcurrentVoiceResultRead, loadConcurrentVoiceResult, dismissConcurrentVoiceResult, cancelConcurrentVoiceRequest, readConcurrentVoiceResult, selectConcurrentVoiceContext, type ConcurrentVoiceRequest } from './concurrentVoice';
  import { isVoiceRequestVisible } from './concurrentVoiceCoordinator';
  import { chatStore, type ContentBlockRecord } from '$lib/stores/chatStore';
  import { get } from 'svelte/store';
  import ChatContentBlocks from '$lib/magician/components/chat/ChatContentBlocks.svelte';
  import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
  let expanded: string | null = null;
  let queueExpanded = false;
  let optionsOpen = false;
  let resultText = '';
  let resultSession = '';
  let resultBlocks: ContentBlockRecord[] = [];
  async function view(id: string): Promise<void> {
    queueExpanded = true;
    optionsOpen = false;
    if (expanded === id) { expanded = null; return; }
    expanded = id; resultText = 'Loading…'; resultBlocks = [];
    try {
      const result = await loadConcurrentVoiceResult(id);
      if (expanded === id) { resultText = result.content.text ?? result.content.summary ?? 'Result saved in its execution context.'; resultSession = result.session_id; resultBlocks = result.content.output_files ?? result.content.content_blocks ?? []; await markConcurrentVoiceResultRead(id); }
    } catch (reason) { error = String(reason); }
  }
  export let sessionId: string | null = null;
  let error: string | null = null;
  $: requests = $concurrentVoiceStore.requests.filter(r => !sessionId || r.parent_session_id === sessionId);
  $: available = requests.filter(r => isVoiceRequestVisible(r, $concurrentVoiceStore.focus?.id) || (queueExpanded && expanded === r.id)).slice().reverse();
  $: latest = available[0];
  function status(request: ConcurrentVoiceRequest): string {
    if ($concurrentVoiceStore.speaking === request.id) return 'Speaking';
    if (request.pending_tasks?.length && request.work_status !== 'cancelled') return 'Task running';
    if (request.work_status === 'accepted') return 'Queued';
    if (request.work_status === 'running') return 'Working';
    if (request.work_status === 'completed') return request.delivery_status === 'played' ? 'Answered' : request.read_at != null ? 'Read' : 'Ready';
    return request.work_status.charAt(0).toUpperCase() + request.work_status.slice(1);
  }
  async function cancel(id: string): Promise<void> {
    try { await cancelConcurrentVoiceRequest(id); error = null; }
    catch (reason) { error = reason instanceof Error ? reason.message : 'Could not cancel this request.'; }
  }
  async function dismiss(id: string): Promise<void> {
    try {
      await dismissConcurrentVoiceResult(id);
      if (expanded === id) expanded = null;
      error = null;
    } catch (reason) { error = String(reason); }
  }
  async function review(sessionId: string): Promise<void> {
    error = null;
    const session = await chatStore.openSession(sessionId);
    if (!session) error = get(chatStore).error ?? 'Could not open this request’s work.';
  }
</script>

{#if latest}
  <section class="voice-requests" aria-label="Background requests">
    <div class="request-summary">
      <button type="button" class="expand" aria-expanded={queueExpanded} aria-label={`${queueExpanded ? 'Collapse' : 'Expand'} background requests (${available.length})`} onclick={() => { queueExpanded = !queueExpanded; if (!queueExpanded) expanded = null; optionsOpen = false; }}>
        <svg class="chevron" class:expanded={queueExpanded} width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 9 6 6 6-6" /></svg>
        <span class="request-status">{status(latest)}</span>
        <span class="latest-title">{latest.title}</span>
        <span class="count">{available.length}</span>
      </button>
      <details class="options" bind:open={optionsOpen}>
        <summary aria-label={`Options for ${latest.title}`} title="Request options"><svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><circle cx="5" cy="12" r="1.5" /><circle cx="12" cy="12" r="1.5" /><circle cx="19" cy="12" r="1.5" /></svg></summary>
        <div class="options-menu">
          <button aria-label="Continue this topic" onclick={() => { selectConcurrentVoiceContext(latest); optionsOpen = false; }}>Continue this topic</button>
          {#if latest.work_status === 'accepted' || latest.work_status === 'running' || (latest.pending_tasks?.length && latest.work_status !== 'cancelled')}
            <button aria-label="Cancel request" onclick={() => { void cancel(latest.id); optionsOpen = false; }}>Cancel request</button>
          {:else if latest.speech_text}
            <button aria-label="Read aloud" onclick={() => { readConcurrentVoiceResult(latest.id); optionsOpen = false; }}>Read aloud</button>
          {/if}
          {#if latest.result_message_id}<button aria-label="View result" onclick={() => view(latest.id)}>View result</button>{/if}
          <button aria-label="Review work" onclick={() => { void review(latest.branch_session_id); optionsOpen = false; }}>Review work</button>
          {#if latest.work_status !== 'accepted' && latest.work_status !== 'running'}
            <button aria-label="Dismiss" onclick={() => { void dismiss(latest.id); optionsOpen = false; }}>Dismiss</button>
          {/if}
          {#if $concurrentVoiceStore.focus}<button aria-label="New topic" onclick={() => { selectConcurrentVoiceContext(null); optionsOpen = false; }}>New topic</button>{/if}
        </div>
      </details>
    </div>
    {#if queueExpanded}
    <div class="request-list">
    {#if $concurrentVoiceStore.focus}
      <div class="context">Next voice topic: {$concurrentVoiceStore.focus.title}
        <button onclick={() => selectConcurrentVoiceContext(null)}>New topic</button>
      </div>
    {/if}
    {#each available as request (request.id)}
      <div class="request" data-request-id={request.id} data-work-status={request.work_status} data-delivery-status={request.delivery_status}>
        <span class="request-status" aria-label={`Request status: ${status(request)}`}>{status(request)}</span>
        <button class="topic" class:selected={$concurrentVoiceStore.focus?.id === request.id} aria-pressed={$concurrentVoiceStore.focus?.id === request.id} title="Continue this topic" onclick={() => selectConcurrentVoiceContext(request)}>{request.title}</button>
        {#if request.work_status === 'accepted' || request.work_status === 'running' || (request.pending_tasks?.length && request.work_status !== 'cancelled')}
          <button aria-label={`Cancel ${request.title}`} onclick={() => cancel(request.id)}>Cancel</button>
        {:else if request.speech_text}
          <button aria-label={`Read ${request.title}`} onclick={() => readConcurrentVoiceResult(request.id)}>Read aloud</button>
        {/if}
        {#if request.result_message_id}<button onclick={() => view(request.id)}>View result</button>{/if}
        <button onclick={() => review(request.branch_session_id)}>Review work</button>
        {#if expanded === request.id}<div class="full-result"><ChatMarkdown content={resultText} sessionId={resultSession} /><ChatContentBlocks blocks={resultBlocks} sessionId={resultSession} /></div>{/if}
        {#if request.work_status !== 'accepted' && request.work_status !== 'running'}
          <button aria-label={`Dismiss ${request.title}`} onclick={() => dismiss(request.id)}>Dismiss</button>
        {/if}
        {#if request.error}<p class="error">{request.error}</p>{/if}
      </div>
    {/each}
    </div>
    {/if}
    {#if error}<p role="alert" class="error">{error}</p>{/if}
    {#if $concurrentVoiceStore.error}<p role="status" class="error">{$concurrentVoiceStore.error}</p>{/if}
  </section>
{/if}

<style>
  .voice-requests { min-width: 0; border-bottom: 1px solid var(--border-soft); border-radius: calc(var(--radius-lg, 14px) - 1px) calc(var(--radius-lg, 14px) - 1px) 0 0; color: var(--text-primary); font-size: 0.75rem; }
  .request-summary { display: flex; align-items: center; gap: 0.25rem; min-width: 0; padding: 0 0.5rem; border-radius: inherit; }
  .request-summary:hover, .request-summary:focus-within { background: var(--accent-primary-soft); }
  .request-summary:has(:focus-visible) { outline: 2px solid var(--accent-primary); outline-offset: -2px; }
  .expand { display: flex; flex: 1; min-width: 0; align-items: center; gap: 0.5rem; height: 1.875rem; text-align: left; }
  .latest-title { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-secondary); }
  .chevron, .count { flex: none; color: var(--text-muted); }
  .chevron { display: block; transform-origin: center; transform: rotate(180deg); }
  .chevron.expanded { transform: rotate(0deg); }
  .count { font-variant-numeric: tabular-nums; }
  .options { position: relative; flex: none; }
  summary { cursor: pointer; display: grid; place-items: center; list-style: none; width: 2rem; height: 1.875rem; border-radius: 0.4rem; }
  summary::-webkit-details-marker { display: none; }
  .options-menu { position: absolute; right: 0; bottom: 100%; z-index: 30; min-width: 10rem; display: grid; max-height: 50dvh; overflow-y: auto; padding: 0.3rem; border: 1px solid var(--border-soft); border-radius: 0.6rem; background: var(--bg-elevated); color: var(--text-primary); box-shadow: var(--shadow-md); }
  .options-menu button { text-align: left; padding: 0.55rem 0.6rem; border-radius: 0.3rem; }
  button, summary { color: inherit; background: transparent; border: 0; font: inherit; }
  button:hover, summary:hover, summary:focus-visible { background: var(--accent-primary-soft); color: var(--text-primary); }
  .topic.selected { background: var(--accent-primary); color: var(--text-on-accent); }
  button:focus-visible, summary:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: -2px; }
  .expand:hover, .expand:focus-visible, summary:hover, summary:focus-visible { background: transparent; outline: none; }
  .voice-requests :global(::selection) { background: var(--accent-primary); color: var(--text-on-accent); }
  .request-list { max-height: min(15rem, 30dvh); overflow-y: auto; padding: 0 0.75rem 0.5rem; }
  .context { color: var(--text-secondary); padding: 0.4rem 0; }
  .request { display: flex; flex-wrap: wrap; align-items: baseline; gap: 0.4rem 0.65rem; padding: 0.5rem 0; border-top: 1px solid var(--border-soft); }
  .request-status { flex: none; font-weight: 600; }
  .topic { flex: 1 1 55%; text-align: left; overflow-wrap: anywhere; border-radius: 0.3rem; padding: 0 0.3rem; }
  button { cursor: pointer; }
  .request > button:not(.topic), .context button { text-decoration: underline; text-underline-offset: 0.2em; }
  .full-result { width: 100%; padding: 0.75rem 0; }
  .error { padding: 0.25rem 0.75rem; width: 100%; opacity: 0.8; }
</style>
