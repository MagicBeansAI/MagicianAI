<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { fly } from 'svelte/transition';
  import Button from '$lib/magician/components/generative/Button.svelte';

  export let stepId: string = '';
  export let summary: string = '';
  export let iterationsUsed: number = 0;
  export let durationMs: number = 0;

  // Parse detection type from summary if available
  // Summary format: "Loop detected: <detection_type> - <details>"
  $: detectionType = parseDetectionType(summary);
  $: recommendation = parseRecommendation(summary);

  function parseDetectionType(summary: string): string {
    if (summary.includes('state_loop') || summary.includes('same state')) {
      return 'state_loop';
    } else if (summary.includes('action_cycle') || summary.includes('repeating pattern')) {
      return 'action_cycle';
    } else if (summary.includes('no_progress') || summary.includes('no progress')) {
      return 'no_progress';
    }
    return 'unknown';
  }

  function parseRecommendation(summary: string): string {
    // Try to extract recommendation from summary
    // Recommendations are typically after "Recommendation:" or the second part of the message
    if (summary.includes('Recommendation:')) {
      return summary.split('Recommendation:')[1]?.trim() || getDefaultRecommendation(detectionType);
    }
    return getDefaultRecommendation(detectionType);
  }

  function getDefaultRecommendation(type: string): string {
    switch (type) {
      case 'state_loop':
        return 'The same page state was detected multiple times. Try modifying your goal or adding more specific instructions.';
      case 'action_cycle':
        return 'A repeating pattern of actions was detected. The task may need different approach or clearer success criteria.';
      case 'no_progress':
        return 'Multiple actions were taken but the state did not change. There may be an obstacle or the goal may not be achievable.';
      default:
        return 'The execution was stopped to prevent wasting resources. Please review the goal and try again.';
    }
  }

  function getDetectionTypeLabel(type: string): string {
    switch (type) {
      case 'state_loop': return 'State Loop';
      case 'action_cycle': return 'Action Cycle';
      case 'no_progress': return 'No Progress';
      default: return 'Loop Detected';
    }
  }

  function getDetectionTypeIcon(type: string): string {
    switch (type) {
      case 'state_loop': return '🔁';
      case 'action_cycle': return '🔄';
      case 'no_progress': return '⏹️';
      default: return '🔄';
    }
  }

  const dispatch = createEventDispatcher<{
    retry: void;
    stop: void;
    editGoal: void;
    dismiss: void;
  }>();

  function handleRetry() {
    dispatch('retry');
  }

  function handleStop() {
    dispatch('stop');
  }

  function handleEditGoal() {
    dispatch('editGoal');
  }

  function handleDismiss() {
    dispatch('dismiss');
  }

  function formatDuration(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
    return `${(ms / 60000).toFixed(1)}m`;
  }
</script>

<div
  class="loop-detected-alert"
  data-step-id={stepId}
  transition:fly={{ y: -10, duration: 200 }}
>
  <div class="alert-header">
    <div class="alert-icon">
      <span class="icon">{getDetectionTypeIcon(detectionType)}</span>
    </div>
    <div class="alert-title">
      <h3>{getDetectionTypeLabel(detectionType)}</h3>
      <span class="subtitle">Execution stopped after {iterationsUsed} iterations ({formatDuration(durationMs)})</span>
    </div>
    <Button iconOnly variant="outline" size="sm" title="Dismiss" on:click={handleDismiss}>
      <svg slot="icon" xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <line x1="18" y1="6" x2="6" y2="18"></line>
        <line x1="6" y1="6" x2="18" y2="18"></line>
      </svg>
    </Button>
  </div>

  <div class="alert-body">
    <p class="recommendation">{recommendation}</p>

    {#if summary && summary !== recommendation}
      <details class="details-section">
        <summary>Show details</summary>
        <p class="details-content">{summary}</p>
      </details>
    {/if}
  </div>

  <div class="alert-actions">
    <Button label="Stop" variant="outline" size="sm" on:click={handleStop}>
      <svg slot="icon" xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <rect x="3" y="3" width="18" height="18" rx="2" ry="2"></rect>
      </svg>
    </Button>
    <Button label="Edit Goal" variant="outline" size="sm" on:click={handleEditGoal}>
      <svg slot="icon" xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"></path>
        <path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"></path>
      </svg>
    </Button>
    <Button label="Retry" variant="primary" size="sm" on:click={handleRetry}>
      <svg slot="icon" xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <polyline points="23 4 23 10 17 10"></polyline>
        <polyline points="1 20 1 14 7 14"></polyline>
        <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"></path>
      </svg>
    </Button>
  </div>
</div>

<style>
  /* ===== SOFT MACHINE THEME - Loop Detected Alert ===== */

  .loop-detected-alert {
    background: var(--bg-card, #ffffff);
    border: 2px solid var(--color-warning, #d4a34a);
    border-radius: var(--radius-lg, 20px);
    padding: var(--space-lg, 1.5rem);
    box-shadow: var(--shadow-md, 0 4px 20px rgba(45, 42, 38, 0.08));
    margin: var(--space-md, 1rem) 0;
  }

  .alert-header {
    display: flex;
    align-items: flex-start;
    gap: var(--space-md, 1rem);
    margin-bottom: var(--space-md, 1rem);
  }

  .alert-icon {
    flex-shrink: 0;
    width: 44px;
    height: 44px;
    background: var(--color-warning-soft, #fdf6e8);
    border-radius: var(--radius-md, 12px);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .alert-icon .icon {
    font-size: 22px;
  }

  .alert-title {
    flex: 1;
  }

  .alert-title h3 {
    margin: 0;
    font-size: 1rem;
    font-weight: 600;
    color: var(--color-warning, #d4a34a);
  }

  .alert-title .subtitle {
    font-size: 0.75rem;
    color: var(--text-muted, #8a847a);
    margin-top: 0.25rem;
    display: block;
  }

  /* Dismiss button now uses GAUI Button component */

  .alert-body {
    margin-bottom: var(--space-lg, 1.5rem);
  }

  .recommendation {
    margin: 0;
    font-size: 0.9rem;
    color: var(--text-body, #4a4540);
    line-height: 1.6;
  }

  .details-section {
    margin-top: var(--space-md, 1rem);
    font-size: 0.8rem;
  }

  .details-section summary {
    cursor: pointer;
    color: var(--accent-plum, #8b7ec8);
    font-weight: 500;
  }

  .details-section summary:hover {
    text-decoration: underline;
  }

  .details-content {
    margin-top: var(--space-sm, 0.5rem);
    padding: var(--space-sm, 0.5rem) var(--space-md, 1rem);
    background: var(--bg-soft, #f3f0ea);
    border-radius: var(--radius-sm, 8px);
    font-family: var(--font-mono);
    font-size: 0.75rem;
    color: var(--text-body, #4a4540);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .alert-actions {
    display: flex;
    gap: var(--space-sm, 0.5rem);
    justify-content: flex-end;
  }

  /* Action buttons now use GAUI Button component */

  /* ── Retro 16-bit Dark Theme ── */
  :global([data-theme="retro-16bit"]) .loop-detected-alert {
    border-radius: 0;
    border-color: #ffb000;
    font-family: var(--font-mono);
  }

  :global([data-theme="retro-16bit"]) .alert-icon {
    border-radius: 0;
    background: #000;
    border: 1px solid #ffb000;
  }

  :global([data-theme="retro-16bit"]) .details-content {
    border-radius: 0;
  }

  /* ── Retro 16-bit Light Theme ── */
  :global([data-theme="retro-16bit-light"]) .loop-detected-alert {
    border-radius: 0;
    border-color: #1a1a1a;
    font-family: var(--font-mono);
  }

  :global([data-theme="retro-16bit-light"]) .alert-icon {
    border-radius: 0;
    background: #f5f5f0;
    border: 2px solid #1a1a1a;
  }

  :global([data-theme="retro-16bit-light"]) .details-content {
    border-radius: 0;
  }
</style>
