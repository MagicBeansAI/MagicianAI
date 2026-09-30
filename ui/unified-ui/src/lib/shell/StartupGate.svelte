<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';

  export let enabled = true;
  const dispatch = createEventDispatcher<{ ready: void }>();
  let ready = !enabled;
  let failed = false;

  onMount(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let request: AbortController | undefined;

    function finish(): void {
      if (stopped) return;
      ready = true;
      dispatch('ready');
    }

    async function probe(): Promise<void> {
      request = new AbortController();
      const timeout = setTimeout(() => request?.abort(), 3000);
      try {
        const response = await fetch('/api/magician/v2/startup', {
          cache: 'no-store', signal: request.signal
        });
        // Older servers and static/public hosts have no startup endpoint.
        // Preserve their normal login/offline behavior rather than gating them.
        if (!response.ok) { finish(); return; }
        const status = await response.json();
        if (status?.service !== 'magician' || status.ready === true) { finish(); return; }
        if (status.status === 'failed') { failed = true; return; }
        if (status.status !== 'starting') { finish(); return; }
      } catch {
        finish();
        return;
      } finally {
        clearTimeout(timeout);
      }
      if (!stopped) timer = setTimeout(probe, 500);
    }

    if (enabled) void probe(); else finish();
    return () => { stopped = true; clearTimeout(timer); request?.abort(); };
  });
</script>

{#if ready}
  <slot />
{:else}
  <main class="startup" role="status" aria-live="polite">
    <h1>{failed ? 'Workspace could not start' : 'Starting your workspace…'}</h1>
    <p>{failed ? 'Check the service logs, then restart the service.' : 'Your workspace will open automatically when it is ready.'}</p>
  </main>
{/if}

<style>
  .startup { min-height: 100dvh; display: flex; flex-direction: column; justify-content: center; align-items: center; padding: 2rem; background: var(--bg-base, #101116); color: var(--text-primary, #eee); font-family: var(--font-primary, system-ui); text-align: center; }
  h1 { font-size: 1.5rem; }
  p { max-width: 32rem; color: var(--text-secondary, #b8bbc6); }
</style>
