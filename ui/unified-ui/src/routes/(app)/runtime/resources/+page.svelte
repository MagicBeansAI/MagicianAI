<!--
  /runtime/resources — Observe-only local resource governor viewer.

  Mounts <LocalResourceGovernorPanel /> which polls
  /api/local-resource-governor/snapshot every 2s.
-->
<script lang="ts">
  import LocalResourceGovernorPanel from '$lib/magician/components/LocalResourceGovernorPanel.svelte';
</script>

<svelte:head>
  <title>Local resources · Magican</title>
</svelte:head>

<div class="resources-page">
  <LocalResourceGovernorPanel />
</div>

<style>
  .resources-page {
    display: flex;
    flex-direction: column;
    min-height: calc(100vh - 48px - var(--attention-bar-offset, 0px));
    padding: 1rem 1.25rem;
    width: 100%;
    max-width: 1320px;
    margin: 0 auto;
    box-sizing: border-box;
  }
</style>
