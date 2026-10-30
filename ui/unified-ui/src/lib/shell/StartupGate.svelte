<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import SettingsSkeleton from '$lib/settings/SettingsSkeleton.svelte';

  export let enabled = true;
  /** Show the Settings skeleton while waiting, not the "Starting" message. */
  export let skeleton = false;
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

    // A probe in flight is never aborted to start another: its catch reads an
    // abort as an unreachable host and opens the gate.
    let probing = false;

    async function probe(): Promise<void> {
      probing = true;
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
        probing = false;
      }
      if (!stopped) timer = setTimeout(probe, 500);
    }

    // A hidden native panel (the desktop notch) can have its timers or web
    // process suspended while it waits, and then show "Starting" long after
    // the service is ready. Re-check as soon as it is visible or focused.
    function recheck(): void {
      if (stopped || ready || failed || probing || document.visibilityState === 'hidden') return;
      clearTimeout(timer);
      void probe();
    }

    if (enabled) void probe(); else finish();
    document.addEventListener('visibilitychange', recheck);
    window.addEventListener('focus', recheck);
    return () => {
      stopped = true;
      clearTimeout(timer);
      request?.abort();
      document.removeEventListener('visibilitychange', recheck);
      window.removeEventListener('focus', recheck);
    };
  });
</script>

{#if ready}
  <slot />
{:else if skeleton && !failed}
  <SettingsSkeleton />
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
