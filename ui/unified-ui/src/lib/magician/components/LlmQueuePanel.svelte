<script lang="ts">
  /**
   * LlmQueuePanel — viewer for the global LLM dispatch queue.
   *
   * Polls /api/llm/queue/snapshot on a timer (default 2s) and renders:
   *   - Header strip: workers busy/total + per-lane depth.
   *   - Filter bar: priority / state / task id substring.
   *   - Table: every job in the registry (pending + in-flight + recently
   *     completed/failed/tombstoned), with a Cancel button on pending rows.
   *
   * Themed against the global CSS variable system (--bg-*, --text-*,
   * --border-*, --accent-*) so the page picks up whatever dashboard theme
   * the user has chosen.
   *
   * See `docs/components/magician/llm-dispatch-queue.md`.
   */
  import { onDestroy, onMount } from 'svelte';

  import { createLlmQueueStore } from '../stores/llm-queue-store';
  import type { JobMeta, JobState, Priority } from '../../realtime/llm-queue';

  export let baseUrl = '';

  const store = createLlmQueueStore(baseUrl);
  const { snapshot, error, cancelJob } = store;

  let filterPriority: 'all' | Priority = 'all';
  let filterState: 'all' | JobState = 'all';
  let filterTaskId = '';

  onMount(() => store.start());
  onDestroy(() => store.stop());

  function flatten(snap: typeof $snapshot): JobMeta[] {
    if (!snap) return [];
    // Order: in-flight first, then pending (live work at top), then a
    // descending-time tail of recently terminated jobs.
    return [
      ...snap.registry.in_flight,
      ...snap.registry.pending,
      ...snap.registry.completed,
      ...snap.registry.failed,
      ...snap.registry.tombstoned,
    ];
  }

  function matchesFilters(meta: JobMeta): boolean {
    if (filterPriority !== 'all' && meta.priority !== filterPriority) return false;
    if (filterState !== 'all' && meta.state !== filterState) return false;
    if (
      filterTaskId &&
      !(meta.task_ref?.task_id ?? '').toLowerCase().includes(filterTaskId.toLowerCase())
    ) {
      return false;
    }
    return true;
  }

  function fmtMs(ms: number | undefined): string {
    if (ms === undefined || ms === null) return '—';
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
  }

  function fmtTokens(meta: JobMeta): string {
    if (!meta.tokens) return '—';
    return `${meta.tokens.prompt_tokens}+${meta.tokens.completion_tokens}`;
  }

  function stateClass(state: JobState): string {
    return `state state-${state}`;
  }

  function priorityClass(p: Priority): string {
    return `priority priority-${p}`;
  }

  async function handleCancel(jobId: string): Promise<void> {
    await cancelJob(jobId, 'viewer_cancel');
  }

  $: rows = flatten($snapshot).filter(matchesFilters);
  $: counts = computeCounts($snapshot);

  function computeCounts(snap: typeof $snapshot): {
    pending: number;
    in_flight: number;
    completed: number;
    failed: number;
    tombstoned: number;
  } {
    if (!snap) {
      return { pending: 0, in_flight: 0, completed: 0, failed: 0, tombstoned: 0 };
    }
    return {
      pending: snap.registry.pending.length,
      in_flight: snap.registry.in_flight.length,
      completed: snap.registry.completed.length,
      failed: snap.registry.failed.length,
      tombstoned: snap.registry.tombstoned.length,
    };
  }
</script>

<section class="queue-panel">
  <header class="queue-header">
    <div class="queue-header-title">
      <h2>Dispatch queue</h2>
      <p class="queue-header-sub">
        Workers, lanes, retries, cancellations. Live snapshot every 2s.
      </p>
    </div>
    {#if $snapshot}
      <div class="queue-header-meta">
        <div class="metric">
          <span class="metric-label">workers</span>
          <span class="metric-value">{$snapshot.workers_busy}/{$snapshot.workers_total}</span>
        </div>
        <div class="metric">
          <span class="metric-label">high</span>
          <span class="metric-value">{$snapshot.depth_high}/{$snapshot.capacity_high}</span>
        </div>
        <div class="metric">
          <span class="metric-label">normal</span>
          <span class="metric-value">{$snapshot.depth_normal}/{$snapshot.capacity_normal}</span>
        </div>
        <div class="metric">
          <span class="metric-label">background</span>
          <span class="metric-value">{$snapshot.depth_background}/{$snapshot.capacity_background}</span>
        </div>
        <div class="metric">
          <span class="metric-label">provider wait</span>
          <span class="metric-value" data-testid="provider-wait-count">{$snapshot.waiting_for_provider ?? 0}</span>
        </div>
        <div class="metric">
          <span class="metric-label">local prep wait</span>
          <span class="metric-value" data-testid="local-prep-wait-count">{$snapshot.waiting_for_local_prep ?? 0}</span>
        </div>
      </div>
    {/if}
  </header>

  {#if $error}
    <div class="error-banner">
      Failed to load snapshot: {$error}
    </div>
  {/if}

  <div class="counts-row">
    <span class="count count-pending">pending {counts.pending}</span>
    <span class="count count-inflight">in flight {counts.in_flight}</span>
    <span class="count count-completed">completed {counts.completed}</span>
    <span class="count count-failed">failed {counts.failed}</span>
    <span class="count count-tombstoned">tombstoned {counts.tombstoned}</span>
  </div>

  <div class="filter-bar">
    <label class="filter-field">
      <span class="filter-label">priority</span>
      <select bind:value={filterPriority}>
        <option value="all">all</option>
        <option value="high">high</option>
        <option value="normal">normal</option>
        <option value="background">background</option>
      </select>
    </label>
    <label class="filter-field">
      <span class="filter-label">state</span>
      <select bind:value={filterState}>
        <option value="all">all</option>
        <option value="pending">pending</option>
        <option value="waiting_for_provider">waiting for provider</option>
        <option value="waiting_for_local_prep">waiting for local prep</option>
        <option value="in_flight">in flight</option>
        <option value="completed">completed</option>
        <option value="failed">failed</option>
        <option value="tombstoned">tombstoned</option>
      </select>
    </label>
    <label class="filter-field filter-task">
      <span class="filter-label">task id</span>
      <input type="text" placeholder="contains…" bind:value={filterTaskId} />
    </label>
    <button type="button" class="refresh-btn" on:click={() => store.refresh()}>
      Refresh
    </button>
  </div>

  <div class="table-wrap">
    <table class="queue-table">
      <thead>
        <tr>
          <th>state</th>
          <th>priority</th>
          <th>operation</th>
          <th>provider</th>
          <th>model</th>
          <th>task</th>
          <th class="num">wait</th>
          <th class="num">provider wait</th>
          <th class="num">exec</th>
          <th class="num">tokens</th>
          <th class="num">attempts</th>
          <th>notes</th>
          <th class="actions"></th>
        </tr>
      </thead>
      <tbody>
        {#each rows as row (row.job_id)}
          <tr>
            <td><span class={stateClass(row.state)}>{row.state.replace(/_/g, ' ')}</span></td>
            <td><span class={priorityClass(row.priority)}>{row.priority}</span></td>
            <td class="op">{row.origin.operation}</td>
            <td>{row.provider ?? '—'}</td>
            <td class="model">{row.model ?? '—'}</td>
            <td class="task-id" title={row.task_ref?.task_id}>
              {row.task_ref?.task_id ?? '—'}
            </td>
            <td class="num">{fmtMs(row.wait_ms)}</td>
            <td class="num">{fmtMs(row.provider_wait_ms)}</td>
            <td class="num">{fmtMs(row.execution_ms)}</td>
            <td class="num">{fmtTokens(row)}</td>
            <td class="num">{row.attempts}</td>
            <td class="notes">
              {#if row.tombstone}
                <span class="tombstone-note"
                  >{row.tombstone.kind}{row.tombstone.reason
                    ? ` (${row.tombstone.reason})`
                    : ''}</span>
              {:else if row.error}
                <span class="error-note">{row.error_class ?? 'error'}: {row.error}</span>
              {:else}
                <span class="muted">—</span>
              {/if}
            </td>
            <td class="actions">
              {#if row.state === 'pending' || row.state === 'waiting_for_provider' || row.state === 'waiting_for_local_prep'}
                <button class="cancel-btn" on:click={() => handleCancel(row.job_id)}>
                  cancel
                </button>
              {/if}
            </td>
          </tr>
        {/each}
        {#if rows.length === 0}
          <tr>
            <td class="empty" colspan="13">No jobs match the current filters.</td>
          </tr>
        {/if}
      </tbody>
    </table>
  </div>
</section>

<style>
  .queue-panel {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    padding: 1.25rem;
    background: var(--bg-elevated, #fff);
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
    border-radius: var(--radius-lg, 14px);
    box-shadow: var(--shadow-md, 0 4px 14px rgba(0, 0, 0, 0.05));
    color: var(--text-primary, #1a1a1a);
    font-family: var(--font-primary);
  }

  .queue-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1.5rem;
    flex-wrap: wrap;
  }

  .queue-header-title h2 {
    margin: 0;
    font-size: 1.25rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .queue-header-sub {
    margin: 0.25rem 0 0;
    font-size: 0.85rem;
    color: var(--text-muted, #888);
  }

  .queue-header-meta {
    display: flex;
    gap: 1.25rem;
    flex-wrap: wrap;
  }

  .metric {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
  }

  .metric-label {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--text-muted, #888);
  }

  .metric-value {
    font-family: var(--font-mono);
    font-size: 14px;
    color: var(--text-primary, #1a1a1a);
  }

  .error-banner {
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--color-error, rgba(192, 64, 64, 0.4));
    background: var(--color-error-soft, rgba(192, 64, 64, 0.08));
    color: var(--color-error, #c04040);
    border-radius: var(--radius-md, 8px);
    font-size: 0.85rem;
  }

  .counts-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }

  .count {
    display: inline-flex;
    align-items: center;
    padding: 4px 10px;
    border-radius: var(--radius-full, 999px);
    background: var(--bg-soft, rgba(0, 0, 0, 0.04));
    color: var(--text-secondary, #555);
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.04em;
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
  }

  .count-inflight {
    color: var(--color-info, #1f6feb);
    border-color: var(--color-info-soft, rgba(31, 111, 235, 0.3));
  }
  .count-failed {
    color: var(--color-error, #c04040);
    border-color: var(--color-error-soft, rgba(192, 64, 64, 0.3));
  }
  .count-tombstoned {
    color: var(--color-warning, #b07a1e);
    border-color: var(--color-warning-soft, rgba(176, 122, 30, 0.3));
  }
  .count-completed {
    color: var(--color-success, #2f7d3a);
    border-color: var(--color-success-soft, rgba(47, 125, 58, 0.3));
  }

  .filter-bar {
    display: flex;
    align-items: flex-end;
    flex-wrap: wrap;
    gap: 0.75rem;
  }

  .filter-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .filter-label {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--text-muted, #888);
  }

  .filter-field select,
  .filter-field input {
    height: 28px;
    padding: 0 8px;
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
    border-radius: var(--radius-sm, 6px);
    background: var(--bg-elevated, #fff);
    color: var(--text-primary, #1a1a1a);
    font-family: var(--font-primary);
    font-size: 12px;
    outline: none;
    transition: border-color var(--transition-fast, 0.12s ease);
  }

  .filter-field select:focus,
  .filter-field input:focus {
    border-color: var(--accent-primary, #c2502a);
  }

  .filter-task {
    flex: 1;
    min-width: 180px;
  }

  .filter-task input {
    width: 100%;
  }

  .refresh-btn {
    height: 28px;
    padding: 0 12px;
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
    border-radius: var(--radius-sm, 6px);
    background: var(--bg-soft, rgba(0, 0, 0, 0.04));
    color: var(--text-primary, #1a1a1a);
    font-family: var(--font-primary);
    font-size: 12px;
    cursor: pointer;
    transition: background var(--transition-fast, 0.12s ease);
  }

  .refresh-btn:hover {
    background: var(--accent-primary-soft, rgba(0, 0, 0, 0.08));
  }

  .table-wrap {
    overflow-x: auto;
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
    border-radius: var(--radius-md, 10px);
    background: var(--bg-base, transparent);
  }

  .queue-table {
    width: 100%;
    border-collapse: collapse;
    font-family: var(--font-mono);
    font-size: 12px;
  }

  .queue-table thead th {
    text-align: left;
    padding: 8px 10px;
    font-weight: 500;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--text-muted, #888);
    border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
    background: var(--bg-soft, rgba(0, 0, 0, 0.02));
    position: sticky;
    top: 0;
    z-index: 1;
  }

  .queue-table tbody td {
    padding: 6px 10px;
    border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.05));
    color: var(--text-primary, #1a1a1a);
    vertical-align: middle;
  }

  .queue-table tbody tr:last-child td {
    border-bottom: 0;
  }

  .queue-table tbody tr:hover {
    background: var(--bg-soft, rgba(0, 0, 0, 0.02));
  }

  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .task-id,
  .model {
    max-width: 12rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .op {
    color: var(--text-secondary, #555);
  }

  .notes {
    color: var(--text-secondary, #555);
  }

  .tombstone-note {
    color: var(--color-warning, #b07a1e);
  }

  .error-note {
    color: var(--color-error, #c04040);
  }

  .muted {
    color: var(--text-muted, #999);
  }

  .actions {
    text-align: right;
  }

  .cancel-btn {
    padding: 2px 8px;
    border: 1px solid var(--color-error-soft, rgba(192, 64, 64, 0.3));
    background: transparent;
    color: var(--color-error, #c04040);
    font-family: var(--font-mono);
    font-size: 11px;
    border-radius: var(--radius-sm, 6px);
    cursor: pointer;
    transition: background var(--transition-fast, 0.12s ease);
  }

  .cancel-btn:hover {
    background: var(--color-error-soft, rgba(192, 64, 64, 0.08));
  }

  .state {
    display: inline-block;
    padding: 2px 8px;
    border-radius: var(--radius-full, 999px);
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    background: var(--bg-soft, rgba(0, 0, 0, 0.04));
    color: var(--text-secondary, #555);
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
  }

  .state-pending {
    color: var(--text-muted, #888);
  }
  .state-waiting_for_provider {
    color: var(--color-warning, #b07a1e);
    border-color: var(--color-warning-soft, rgba(176, 122, 30, 0.3));
    background: var(--color-warning-soft, rgba(176, 122, 30, 0.06));
  }
  .state-waiting_for_local_prep {
    color: var(--color-warning, #b07a1e);
    border-color: var(--color-warning-soft, rgba(176, 122, 30, 0.3));
    background: var(--color-warning-soft, rgba(176, 122, 30, 0.06));
  }
  .state-in_flight {
    color: var(--color-info, #1f6feb);
    border-color: var(--color-info-soft, rgba(31, 111, 235, 0.3));
    background: var(--color-info-soft, rgba(31, 111, 235, 0.06));
  }
  .state-completed {
    color: var(--color-success, #2f7d3a);
    border-color: var(--color-success-soft, rgba(47, 125, 58, 0.3));
    background: var(--color-success-soft, rgba(47, 125, 58, 0.06));
  }
  .state-failed {
    color: var(--color-error, #c04040);
    border-color: var(--color-error-soft, rgba(192, 64, 64, 0.3));
    background: var(--color-error-soft, rgba(192, 64, 64, 0.06));
  }
  .state-tombstoned {
    color: var(--color-warning, #b07a1e);
    border-color: var(--color-warning-soft, rgba(176, 122, 30, 0.3));
    background: var(--color-warning-soft, rgba(176, 122, 30, 0.06));
  }

  .priority {
    display: inline-block;
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--text-muted, #888);
  }
  .priority-high {
    color: var(--accent-primary, #c2502a);
  }

  .empty {
    text-align: center;
    padding: 1.5rem;
    color: var(--text-muted, #888);
  }
</style>
