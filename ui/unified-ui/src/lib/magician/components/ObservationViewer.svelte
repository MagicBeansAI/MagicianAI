<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import JsonViewer from './JsonViewer.svelte';
  import { timedFetch } from '$lib/shared/fetch';

  export let observation: {
    observationId: string;
    observationFilename?: string;
    pageStage: string;
    url?: string;
    hasScreenshot: boolean;
    timestamp: Date;
  } | null = null;

  export let executionId: string = '';

  const dispatch = createEventDispatcher();

  let screenshot: string | null = null;
  let screenshotLoading = false;
  let screenshotError: string | null = null;
  let rawJson: unknown = null;
  let rawJsonLoading = false;
  let rawJsonError: string | null = null;
  let activeTab: 'screenshot' | 'json' = 'screenshot';

  // Fetch screenshot on-demand when modal opens
  $: if (observation?.observationId && observation.hasScreenshot && executionId) {
    fetchScreenshot(observation.observationId);
  } else {
    screenshot = null;
    screenshotError = null;
  }

  // Reset JSON when observation changes
  $: if (observation?.observationId) {
    rawJson = null;
    rawJsonError = null;
    rawJsonLoading = false;
    // Auto-switch to JSON tab if no screenshot
    if (!observation.hasScreenshot) {
      activeTab = 'json';
    } else {
      activeTab = 'screenshot';
    }
  }

  async function fetchScreenshot(observationId: string) {
    screenshotLoading = true;
    screenshotError = null;
    try {
      const response = await timedFetch(`/api/magician/v2/executions/${executionId}/observations/${observationId}/screenshot`);
      if (!response.ok) {
        throw new Error(`Failed to fetch screenshot: ${response.status}`);
      }
      const data = await response.json();
      screenshot = data.screenshot;
    } catch (e) {
      screenshotError = e instanceof Error ? e.message : 'Unknown error';
    } finally {
      screenshotLoading = false;
    }
  }

  async function fetchRawJson() {
    if (!observation || !executionId || rawJson || rawJsonLoading) return;
    rawJsonLoading = true;
    rawJsonError = null;
    try {
      const response = await timedFetch(`/api/magician/v2/executions/${executionId}/observations/${observation.observationId}/json`);
      if (!response.ok) {
        throw new Error(`Failed to fetch observation JSON: ${response.status}`);
      }
      rawJson = await response.json();
    } catch (e) {
      rawJsonError = e instanceof Error ? e.message : 'Unknown error';
    } finally {
      rawJsonLoading = false;
    }
  }

  // Fetch JSON when JSON tab is selected
  $: if (activeTab === 'json' && observation && !rawJson && !rawJsonLoading) {
    fetchRawJson();
  }

  function close() {
    screenshot = null;
    screenshotError = null;
    rawJson = null;
    rawJsonError = null;
    dispatch('close');
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      close();
    }
  }

  function handleBackdropClick(event: MouseEvent) {
    if (event.target === event.currentTarget) {
      close();
    }
  }
</script>

<svelte:window on:keydown={handleKeydown} />

{#if observation}
  <!-- svelte-ignore a11y-click-events-have-key-events -->
  <!-- svelte-ignore a11y-no-static-element-interactions -->
  <div
    class="modal-backdrop"
    on:click={handleBackdropClick}
  >
    <div class="modal-content" on:click|stopPropagation>
      <!-- Header -->
      <div class="modal-header">
        <div class="flex-1 min-w-0">
          <h2 class="ov-title">
            Page Observation
          </h2>
          <p class="ov-subtitle">
            {#if observation.observationFilename}
              <span class="ov-filename" title={observation.observationFilename}>{observation.observationFilename}</span>
            {:else}
              <span class="font-medium">{observation.pageStage}</span>
            {/if}
            {#if observation.url}
              <span class="mx-1 opacity-40">|</span>
              <span class="truncate max-w-md inline-block align-bottom" title={observation.url}>
                {observation.url}
              </span>
            {/if}
          </p>
        </div>
        <button
          class="close-button"
          on:click={close}
          aria-label="Close"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5" viewBox="0 0 20 20" fill="currentColor">
            <path fill-rule="evenodd" d="M4.293 4.293a1 1 0 011.414 0L10 8.586l4.293-4.293a1 1 0 111.414 1.414L11.414 10l4.293 4.293a1 1 0 01-1.414 1.414L10 11.414l-4.293 4.293a1 1 0 01-1.414-1.414L8.586 10 4.293 5.707a1 1 0 010-1.414z" clip-rule="evenodd" />
          </svg>
        </button>
      </div>

      <!-- Tabs -->
      <div class="tabs">
        {#if observation.hasScreenshot}
          <button
            class="tab"
            class:active={activeTab === 'screenshot'}
            on:click={() => activeTab = 'screenshot'}
          >
            Screenshot
          </button>
        {/if}
        <button
          class="tab"
          class:active={activeTab === 'json'}
          on:click={() => activeTab = 'json'}
        >
          Raw JSON
        </button>
      </div>

      <!-- Content -->
      <div class="modal-body">
        {#if activeTab === 'screenshot'}
          <div class="screenshot-container">
            {#if screenshotLoading}
              <div class="screenshot-placeholder">
                <div class="loading-spinner"></div>
                <span class="text-sm text-gray-500 mt-2">Loading screenshot...</span>
              </div>
            {:else if screenshotError}
              <div class="screenshot-placeholder error">
                <svg xmlns="http://www.w3.org/2000/svg" class="h-8 w-8 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
                </svg>
                <span class="text-sm text-red-500 mt-2">{screenshotError}</span>
              </div>
            {:else if screenshot}
              <img
                src={`data:image/png;base64,${screenshot}`}
                alt="Page screenshot"
                class="screenshot-image"
              />
            {:else if !observation.hasScreenshot}
              <div class="screenshot-placeholder">
                <svg xmlns="http://www.w3.org/2000/svg" class="h-8 w-8 text-gray-300" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 16l4.586-4.586a2 2 0 012.828 0L16 16m-2-2l1.586-1.586a2 2 0 012.828 0L20 14m-6-6h.01M6 20h12a2 2 0 002-2V6a2 2 0 00-2-2H6a2 2 0 00-2 2v12a2 2 0 002 2z" />
                </svg>
                <span class="text-sm text-gray-400 mt-2">No screenshot available</span>
              </div>
            {/if}
          </div>
        {:else if activeTab === 'json'}
          <div class="json-container">
            {#if rawJsonLoading}
              <div class="screenshot-placeholder">
                <div class="loading-spinner"></div>
                <span class="text-sm text-gray-500 mt-2">Loading observation data...</span>
              </div>
            {:else if rawJsonError}
              <div class="screenshot-placeholder error">
                <svg xmlns="http://www.w3.org/2000/svg" class="h-8 w-8 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                  <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
                </svg>
                <span class="text-sm text-red-500 mt-2">{rawJsonError}</span>
              </div>
            {:else if rawJson}
              <JsonViewer data={rawJson} />
            {:else}
              <div class="screenshot-placeholder">
                <span class="text-sm text-gray-400">No observation data available</span>
              </div>
            {/if}
          </div>
        {/if}
      </div>

      <!-- Footer -->
      <div class="modal-footer">
        <span class="text-xs" style="color: var(--text-tertiary, #9ca3af)">
          Captured at {observation.timestamp.toLocaleString()}
        </span>
        <span class="text-xs mx-2" style="color: var(--text-tertiary, #d1d5db)">|</span>
        <span class="text-xs font-mono" style="color: var(--text-tertiary, #9ca3af)">
          {observation.observationId}
        </span>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--bg-base, #000) 50%, transparent);
    backdrop-filter: blur(4px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal-content {
    background: var(--bg-card, #ffffff);
    border-radius: var(--radius-lg, 0.75rem);
    border: 1px solid var(--border-soft, #e5e7eb);
    box-shadow: var(--shadow-lg, 0 25px 50px -12px rgba(0,0,0,0.25));
    display: flex;
    flex-direction: column;
    max-width: 90vw;
    max-height: 90vh;
    width: 960px;
    animation: modalSlideIn 0.2s ease-out;
  }

  @keyframes modalSlideIn {
    from { opacity: 0; transform: translateY(-10px) scale(0.98); }
    to { opacity: 1; transform: translateY(0) scale(1); }
  }

  .modal-header {
    display: flex;
    align-items: flex-start;
    gap: 1rem;
    padding: 16px 20px;
    border-bottom: 1px solid var(--border-soft, #e5e7eb);
  }

  .ov-title {
    color: var(--text-primary, #111);
    font-family: var(--font-display);
    font-size: 1.1rem;
    font-weight: 600;
    margin: 0;
  }

  .ov-subtitle {
    color: var(--text-secondary, #6b7280);
    font-size: 0.8rem;
    margin-top: 2px;
  }

  .ov-filename {
    font-family: var(--font-mono);
    font-size: 0.75rem;
    color: var(--text-secondary, #6b7280);
    background: var(--bg-hover, #f3f4f6);
    padding: 1px 6px;
    border-radius: 3px;
  }

  .close-button {
    padding: 0.5rem;
    border-radius: 0.5rem;
    color: var(--text-tertiary, #9ca3af);
    background: none;
    border: none;
    cursor: pointer;
    transition: color 150ms, background-color 150ms;
  }

  .close-button:hover {
    color: var(--text-primary, #111);
    background: var(--bg-hover, #f3f4f6);
  }

  .tabs {
    display: flex;
    border-bottom: 1px solid var(--border-soft, #e5e7eb);
    flex-shrink: 0;
  }

  .tab {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 10px 16px;
    font-size: 0.8rem;
    font-weight: 500;
    color: var(--text-secondary, #6b7280);
    border: none;
    background: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    transition: color 150ms, border-color 150ms;
  }

  .tab:hover {
    color: var(--text-primary, #111);
  }

  .tab.active {
    color: var(--accent-primary, #2563eb);
    border-bottom-color: var(--accent-primary, #2563eb);
  }

  .modal-body {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  .screenshot-container {
    padding: 1rem;
    height: 100%;
    overflow: auto;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg-base, #f9fafb);
    min-height: 300px;
  }

  .json-container {
    height: 100%;
    display: flex;
    flex-direction: column;
  }

  .screenshot-placeholder {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 12rem;
    width: 100%;
  }

  .screenshot-placeholder.error {
    background: var(--bg-error, #fef2f2);
    border-radius: 0.5rem;
    padding: 1.5rem;
  }

  .loading-spinner {
    width: 2rem;
    height: 2rem;
    border: 3px solid var(--border-soft, #e5e7eb);
    border-top-color: var(--accent-primary, #3b82f6);
    border-radius: 50%;
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .screenshot-image {
    max-width: 100%;
    max-height: 100%;
    border-radius: 0.5rem;
    box-shadow: 0 10px 15px -3px rgba(0,0,0,0.1);
    border: 1px solid var(--border-soft, #e5e7eb);
    object-fit: contain;
  }

  .modal-footer {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 10px 16px;
    border-top: 1px solid var(--border-soft, #e5e7eb);
    background: var(--bg-base, #f9fafb);
    border-bottom-left-radius: var(--radius-lg, 0.75rem);
    border-bottom-right-radius: var(--radius-lg, 0.75rem);
    flex-shrink: 0;
  }
</style>
