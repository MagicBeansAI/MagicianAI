<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import Icon from '$lib/shared/icons/Icon.svelte';

  import {
    createLocalResourceGovernorStore,
    type ResourceGauge,
    type ResourceSeverity,
  } from '../stores/local-resource-governor-store';

  export let baseUrl = '';

  const store = createLocalResourceGovernorStore(baseUrl);
  const { snapshot, error } = store;

  onMount(() => store.start());
  onDestroy(() => store.stop());

  function severityLabel(severity: ResourceSeverity): string {
    if (severity === 'pressure') return 'pressure';
    if (severity === 'watch') return 'watch';
    return 'ok';
  }

  function severityClass(severity: ResourceSeverity): string {
    return `severity severity-${severity}`;
  }

  function formatValue(value: number, unit: string): string {
    if (unit === 'ms') return `${value}ms`;
    return `${value}`;
  }

  function formatLimit(resource: ResourceGauge): string {
    if (resource.soft_limit === null) return 'none';
    return formatValue(resource.soft_limit, resource.unit);
  }

  function formatCaptured(ms: number | undefined): string {
    if (!ms) return 'not loaded';
    return new Date(ms).toLocaleTimeString();
  }

  $: resources = $snapshot?.resources ?? [];
  $: pressure = $snapshot?.pressure ?? [];
  $: counters = $snapshot?.counters;
  $: latest = $snapshot?.latest_agent_loop;
</script>

<section class="governor-panel">
  <header class="governor-header">
    <div class="governor-title">
      <span class="eyebrow">Runtime · Observe-only</span>
      <div class="title-row">
        <h2>Local resources</h2>
        {#if $snapshot}
          <span class={severityClass($snapshot.status)}>{severityLabel($snapshot.status)}</span>
        {/if}
      </div>
      <p>
        Process-local pressure signals for high-agent-count runs. Limits are soft and only
        recorded as would-throttle counters.
      </p>
    </div>
    <div class="header-actions">
      <a
        class="action-button action-button--outline action-button--sm"
        href="/runtime"
        title="Back to Runtime activity stream and execution tree"
      >
        <Icon name="chevron-left" size={13} />
        <span>Activity</span>
      </a>
      <button
        type="button"
        class="action-button action-button--outline action-button--sm"
        on:click={() => store.refresh()}
        disabled={$snapshot === null}
        title="Refresh local resource signals"
        aria-label="Refresh local resource signals"
      >
        <Icon name="rotate-ccw" size={13} />
        <span>Refresh</span>
      </button>
    </div>
  </header>

  {#if $error}
    <div class="error-banner">
      Failed to load local resource snapshot: {$error}
    </div>
  {/if}

  <div class="summary-grid">
    <div class="metric-tile">
      <span class="metric-label">mode</span>
      <strong>{$snapshot?.mode?.replace('_', ' ') ?? 'loading'}</strong>
      <span class="metric-sub">no enforcement</span>
    </div>
    <div class="metric-tile">
      <span class="metric-label">pressure</span>
      <strong>{$snapshot?.summary.pressure_count ?? 0}</strong>
      <span class="metric-sub">{$snapshot?.summary.watch_count ?? 0} watch</span>
    </div>
    <div class="metric-tile">
      <span class="metric-label">would throttle</span>
      <strong>{$snapshot?.summary.would_throttle_total ?? 0}</strong>
      <span class="metric-sub">observe-only count</span>
    </div>
    <div class="metric-tile">
      <span class="metric-label">updated</span>
      <strong>{formatCaptured($snapshot?.captured_at_ms)}</strong>
      <span class="metric-sub">polls every 2s</span>
    </div>
  </div>

  <section class="pressure-strip">
    <div class="section-heading">
      <h3>Pressure signals</h3>
      <span>{pressure.length}</span>
    </div>
    {#if pressure.length > 0}
      <div class="pressure-list">
        {#each pressure as item (item.resource_id)}
          <article class="pressure-row">
            <div>
              <span class={severityClass(item.severity)}>{severityLabel(item.severity)}</span>
              <strong>{item.label}</strong>
              <p>{item.advice}</p>
            </div>
            <div class="pressure-value">
              {formatValue(item.current, item.unit)}
              <span>/ {formatValue(item.soft_limit, item.unit)}</span>
            </div>
          </article>
        {/each}
      </div>
    {:else}
      <div class="empty-state">No local pressure signals in the current snapshot.</div>
    {/if}
  </section>

  <div class="table-wrap">
    <table class="resource-table">
      <thead>
        <tr>
          <th>resource</th>
          <th>kind</th>
          <th class="num">current</th>
          <th class="num">high water</th>
          <th class="num">soft limit</th>
          <th class="num">would throttle</th>
          <th>status</th>
        </tr>
      </thead>
      <tbody>
        {#each resources as resource (resource.id)}
          <tr>
            <td>
              <strong>{resource.label}</strong>
              <span>{resource.description}</span>
            </td>
            <td>{resource.kind}</td>
            <td class="num">{formatValue(resource.current, resource.unit)}</td>
            <td class="num">{formatValue(resource.high_water, resource.unit)}</td>
            <td class="num">{formatLimit(resource)}</td>
            <td class="num">{resource.would_throttle_total}</td>
            <td><span class={severityClass(resource.severity)}>{severityLabel(resource.severity)}</span></td>
          </tr>
        {/each}
        {#if resources.length === 0}
          <tr>
            <td class="empty" colspan="7">Waiting for the first governor snapshot.</td>
          </tr>
        {/if}
      </tbody>
    </table>
  </div>

  <div class="details-grid">
    <section class="detail-block">
      <div class="section-heading">
        <h3>Counters</h3>
      </div>
      <dl class="counter-grid">
        <div>
          <dt>agent loops started</dt>
          <dd>{counters?.agent_loops_started_total ?? 0}</dd>
        </div>
        <div>
          <dt>queued triggers</dt>
          <dd>{counters?.agent_triggers_queued_total ?? 0}</dd>
        </div>
        <div>
          <dt>duplicate triggers</dt>
          <dd>{counters?.agent_trigger_duplicates_total ?? 0}</dd>
        </div>
        <div>
          <dt>queue full drops</dt>
          <dd>{counters?.agent_trigger_queue_full_total ?? 0}</dd>
        </div>
        <div>
          <dt>direct llm routes</dt>
          <dd>{counters?.llm_direct_route_total ?? 0}</dd>
        </div>
        <div>
          <dt>streaming direct routes</dt>
          <dd>{counters?.llm_direct_stream_route_total ?? 0}</dd>
        </div>
      </dl>
    </section>

    <section class="detail-block">
      <div class="section-heading">
        <h3>Latest agent loop</h3>
      </div>
      {#if latest}
        <dl class="latest-grid">
          <div>
            <dt>started</dt>
            <dd>{formatCaptured(latest.started_at_ms)}</dd>
          </div>
          <div>
            <dt>active after start</dt>
            <dd>{latest.active_after_start}</dd>
          </div>
          <div>
            <dt>agent</dt>
            <dd>{latest.agent_id ?? 'none'}</dd>
          </div>
          <div>
            <dt>task</dt>
            <dd>{latest.task_id ?? 'none'}</dd>
          </div>
          <div class="wide">
            <dt>execution</dt>
            <dd>{latest.execution_id ?? 'none'}</dd>
          </div>
        </dl>
      {:else}
        <div class="empty-state compact">No agent loop has started since this process booted.</div>
      {/if}
    </section>
  </div>
</section>

<style>
  .governor-panel {
    display: flex;
    flex-direction: column;
    gap: 1.15rem;
    color: var(--text-primary, #1a1a1a);
    font-family: var(--font-primary);
  }

  .governor-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1.25rem;
    flex-wrap: wrap;
    margin-bottom: 0.25rem;
  }

  .governor-title {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    max-width: 720px;
  }

  .title-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
  }

  .title-row h2 {
    font-size: 1.55rem;
    font-weight: 700;
    line-height: 1.2;
    margin: 0;
  }

  .eyebrow,
  .metric-label,
  dt {
    color: var(--text-tertiary, #777);
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  h2,
  h3,
  p,
  dl {
    margin: 0;
  }

  h3 {
    font-size: 0.95rem;
    line-height: 1.2;
  }

  .governor-title p,
  .pressure-row p,
  .metric-sub {
    color: var(--text-secondary, #555);
    font-size: 0.85rem;
    line-height: 1.45;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    flex-wrap: wrap;
    justify-content: flex-end;
    margin-top: 0.25rem;
  }

  :global(.action-button) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 0.38rem;
    max-width: 100%;
    min-height: 2.1rem;
    padding: 0.42rem 0.82rem;
    border: 1px solid transparent;
    border-radius: 8px;
    font: inherit;
    font-size: 0.82rem;
    font-weight: 500;
    line-height: 1.2;
    cursor: pointer;
    text-decoration: none;
    transition:
      background 0.15s ease,
      border-color 0.15s ease,
      color 0.15s ease,
      opacity 0.15s ease,
      transform 0.15s ease,
      box-shadow 0.15s ease;
  }
  :global(.action-button):hover:not(:disabled) {
    transform: translateY(-1px);
  }
  :global(.action-button):disabled {
    cursor: default;
    opacity: 0.56;
  }
  :global(.action-button--outline) {
    background: var(--bg-card);
    border-color: var(--border-soft);
    color: var(--text-secondary);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
  }
  :global(.action-button--outline:hover:not(:disabled)) {
    background: var(--bg-soft);
    color: var(--text-primary);
    border-color: var(--border-default);
  }
  :global(.action-button--sm) {
    min-height: 1.85rem;
    padding: 0.3rem 0.65rem;
    font-size: 0.76rem;
  }

  .severity {
    display: inline-flex;
    align-items: center;
    width: fit-content;
    border-radius: 999px;
    padding: 0.18rem 0.48rem;
    font-size: 0.72rem;
    font-weight: 700;
    line-height: 1.2;
    text-transform: uppercase;
    letter-spacing: 0;
    border: 1px solid transparent;
  }

  .severity-ok {
    color: var(--success-foreground, #14532d);
    background: color-mix(in srgb, var(--success, #22c55e) 14%, transparent);
    border-color: color-mix(in srgb, var(--success, #22c55e) 28%, transparent);
  }

  .severity-watch {
    color: var(--warning-foreground, #7c2d12);
    background: color-mix(in srgb, var(--warning, #f59e0b) 16%, transparent);
    border-color: color-mix(in srgb, var(--warning, #f59e0b) 32%, transparent);
  }

  .severity-pressure {
    color: var(--danger-foreground, #7f1d1d);
    background: color-mix(in srgb, var(--danger, #ef4444) 16%, transparent);
    border-color: color-mix(in srgb, var(--danger, #ef4444) 32%, transparent);
  }

  .error-banner,
  .empty-state {
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    padding: 0.8rem 0.9rem;
    color: var(--text-secondary);
    background: var(--bg-card);
    font-size: 0.88rem;
  }

  .error-banner {
    border-color: color-mix(in srgb, var(--danger, #ef4444) 35%, transparent);
    background: color-mix(in srgb, var(--danger, #ef4444) 9%, var(--bg-card));
    color: var(--danger-foreground, #7f1d1d);
  }

  .empty-state.compact {
    padding: 0.7rem 0.8rem;
  }

  .summary-grid,
  .details-grid {
    display: grid;
    gap: 0.75rem;
  }

  .summary-grid {
    grid-template-columns: repeat(4, minmax(0, 1fr));
  }

  .details-grid {
    grid-template-columns: minmax(0, 1.1fr) minmax(0, 0.9fr);
  }

  .metric-tile,
  .detail-block,
  .pressure-strip {
    border: 1px solid var(--border-soft);
    border-radius: 9px;
    background: var(--bg-card);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
  }

  .metric-tile {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.85rem;
    min-width: 0;
    transition: border-color 0.15s ease, transform 0.15s ease;
  }
  .metric-tile:hover {
    border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
    transform: translateY(-1px);
  }

  .metric-tile strong {
    font-size: 1.25rem;
    font-weight: 800;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
    color: var(--text-primary);
    overflow-wrap: anywhere;
  }

  .metric-sub {
    font-size: 0.76rem;
    color: var(--text-muted);
  }

  .pressure-strip,
  .detail-block {
    padding: 0.95rem;
  }

  .section-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }

  .section-heading span {
    color: var(--text-tertiary, #777);
    font-size: 0.78rem;
  }

  .pressure-list {
    display: grid;
    gap: 0.55rem;
  }

  .pressure-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    background: var(--bg-soft);
    padding: 0.75rem;
  }

  .pressure-row > div:first-child {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    min-width: 0;
  }

  .pressure-value {
    color: var(--text-primary, #1a1a1a);
    font-weight: 800;
    white-space: nowrap;
  }

  .pressure-value span {
    color: var(--text-tertiary, #777);
    font-weight: 600;
  }

  .table-wrap {
    overflow-x: auto;
    border: 1px solid var(--border-soft);
    border-radius: 9px;
    background: var(--bg-card);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
    scrollbar-width: thin;
  }

  .resource-table {
    width: 100%;
    min-width: 860px;
    border-collapse: collapse;
    font-size: 0.84rem;
  }

  .resource-table th,
  .resource-table td {
    padding: 0.68rem 0.75rem;
    border-bottom: 1px solid var(--border-soft);
    text-align: left;
    vertical-align: top;
  }

  .resource-table th {
    color: var(--text-muted);
    background: color-mix(in srgb, var(--text-primary) 3%, var(--bg-card));
    font-size: 0.7rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .resource-table td:first-child {
    min-width: 240px;
  }

  .resource-table td:first-child strong,
  .resource-table td:first-child span {
    display: block;
  }

  .resource-table td:first-child span {
    margin-top: 0.2rem;
    color: var(--text-secondary, #555);
    font-size: 0.78rem;
    line-height: 1.35;
  }

  .resource-table .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .resource-table tr:last-child td {
    border-bottom: 0;
  }

  .resource-table .empty {
    text-align: center;
    color: var(--text-secondary, #555);
  }

  .counter-grid,
  .latest-grid {
    display: grid;
    gap: 0.65rem 0.9rem;
  }

  .counter-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }

  .latest-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .latest-grid .wide {
    grid-column: 1 / -1;
  }

  dd {
    margin: 0.18rem 0 0;
    color: var(--text-primary, #1a1a1a);
    font-size: 0.9rem;
    font-weight: 700;
    overflow-wrap: anywhere;
  }

  @media (max-width: 900px) {
    .governor-header,
    .pressure-row {
      flex-direction: column;
    }

    .header-actions {
      justify-content: flex-start;
    }

    .summary-grid,
    .details-grid {
      grid-template-columns: 1fr;
    }

    .counter-grid,
    .latest-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
