<!--
  /llm/queue — Dispatch queue viewer.

  Mounts <LlmQueuePanel /> which polls /api/llm/queue/snapshot every 2s.
  Themed against the global CSS variables — picks up whatever dashboard
  theme the user has chosen from the app shell. The actual /llm page is
  for cost/latency observability; this page is operational — workers,
  retries, cancellations, dead-letter inspection.

  See `docs/components/magician/llm-dispatch-queue.md`.
-->
<script lang="ts">
  import LlmQueuePanel from '$lib/magician/components/LlmQueuePanel.svelte';
</script>

<svelte:head>
  <title>LLM dispatch queue · Magican</title>
</svelte:head>

<div class="queue-page">
  <LlmQueuePanel />
</div>

<style>
  .queue-page {
    display: flex;
    flex-direction: column;
    min-height: calc(100vh - 48px - var(--attention-bar-offset, 0px));
    padding: 1rem 1.25rem;
    /* Match the rest of the dashboard surfaces (cost dashboard at /llm,
       feed, attention, etc.) which constrain to 1320px. */
    width: 100%;
    max-width: 1320px;
    margin: 0 auto;
    box-sizing: border-box;
  }
</style>
