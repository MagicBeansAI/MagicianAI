<!--
  /runtime — the unified runtime activity view.

  The `runtime` route already held `/runtime/resources` (the local
  resource governor) but had no index. This is it: the activity tree
  belongs at the top of the runtime section because it is the general
  answer to "what is this process doing right now", and the governor is
  one specific reading of that.

  All streaming, tree, and filter state lives in <ActivityView />.
-->
<script lang="ts">
  import ActivityView from './ActivityView.svelte';
</script>

<svelte:head>
  <title>Runtime activity · Magican</title>
</svelte:head>

<div class="runtime-page">
  <ActivityView />
</div>

<style>
  .runtime-page {
    display: flex;
    flex-direction: column;
    min-height: calc(100vh - 48px - var(--attention-bar-offset, 0px));
    width: 100%;
  }
</style>
