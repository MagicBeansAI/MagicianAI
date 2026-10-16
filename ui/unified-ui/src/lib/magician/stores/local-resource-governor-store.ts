import { writable, type Writable } from 'svelte/store';

const DEFAULT_POLL_MS = 2000;

export type ResourceSeverity = 'ok' | 'watch' | 'pressure';

export interface ResourceGauge {
  id: string;
  label: string;
  kind: string;
  unit: string;
  current: number;
  high_water: number;
  soft_limit: number | null;
  severity: ResourceSeverity;
  would_throttle_total: number;
  description: string;
}

export interface ResourcePressureSignal {
  resource_id: string;
  label: string;
  severity: ResourceSeverity;
  current: number;
  soft_limit: number;
  unit: string;
  advice: string;
}

export interface LocalResourceSummary {
  resource_count: number;
  pressure_count: number;
  watch_count: number;
  would_throttle_total: number;
}

export interface LocalResourceCounters {
  agent_loops_started_total: number;
  agent_loop_would_throttle_total: number;
  agent_loop_rejected_total?: number;
  agent_loop_rss_blocked_total?: number;
  agent_loop_per_agent_rejected_total?: number;
  agent_triggers_queued_total: number;
  agent_trigger_duplicates_total: number;
  agent_trigger_queue_full_total: number;
  agent_trigger_would_throttle_total: number;
  runtime_event_backlog_would_throttle_total: number;
  event_append_lock_pressure_observations: number;
  llm_direct_route_total: number;
  llm_direct_stream_route_total: number;
}

export interface ObservedAgentLoop {
  started_at_ms: number;
  active_after_start: number;
  agent_id?: string;
  task_id?: string;
  execution_id?: string;
}

export interface LocalResourceGovernorSnapshot {
  schema_version: number;
  captured_at_ms: number;
  mode: 'observe_only' | 'hard_admission';
  observe_only: boolean;
  admitted?: number;
  rejected?: number;
  rss_blocked?: number;
  per_agent_rejects?: number;
  status: ResourceSeverity;
  summary: LocalResourceSummary;
  resources: ResourceGauge[];
  pressure: ResourcePressureSignal[];
  counters: LocalResourceCounters;
  latest_agent_loop?: ObservedAgentLoop | null;
}

export interface LocalResourceGovernorStore {
  snapshot: Writable<LocalResourceGovernorSnapshot | null>;
  error: Writable<string | null>;
  start: () => void;
  stop: () => void;
  refresh: () => Promise<void>;
}

export function createLocalResourceGovernorStore(
  baseUrl = '',
  pollMs = DEFAULT_POLL_MS
): LocalResourceGovernorStore {
  const snapshot = writable<LocalResourceGovernorSnapshot | null>(null);
  const error = writable<string | null>(null);
  let timer: ReturnType<typeof setInterval> | null = null;

  async function fetchSnapshot() {
    try {
      const resp = await fetch(`${baseUrl}/api/local-resource-governor/snapshot`);
      if (!resp.ok) {
        throw new Error(`status ${resp.status}`);
      }
      const data = (await resp.json()) as LocalResourceGovernorSnapshot;
      snapshot.set(data);
      error.set(null);
    } catch (err) {
      error.set(err instanceof Error ? err.message : String(err));
    }
  }

  function start() {
    if (timer) return;
    void fetchSnapshot();
    timer = setInterval(() => {
      void fetchSnapshot();
    }, pollMs);
  }

  function stop() {
    if (!timer) return;
    clearInterval(timer);
    timer = null;
  }

  return {
    snapshot,
    error,
    start,
    stop,
    refresh: fetchSnapshot,
  };
}
