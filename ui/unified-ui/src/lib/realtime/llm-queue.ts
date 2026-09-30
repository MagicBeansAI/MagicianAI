// TypeScript mirror of the magicllm dispatch types.
//
// Mirrors `LlmQueueEvent`, `JobMeta`, `QueueSnapshot` from
// `magicllm/src/dispatch/`. Used by the dev-workbench LlmQueuePanel.

export type Priority = 'high' | 'normal' | 'background';

export type JobState =
  | 'pending'
  | 'waiting_for_provider'
  | 'waiting_for_local_prep'
  | 'in_flight'
  | 'completed'
  | 'failed'
  | 'tombstoned';

export type ErrorClass =
  | 'network'
  | 'timeout'
  | 'server_5xx'
  | 'rate_limit'
  | 'provider_4xx'
  | 'content_policy'
  | 'parse_error'
  | 'cancelled'
  | 'unknown';

export type TombstoneKind =
  | 'task_cancelled'
  | 'task_cancelled_in_flight'
  | 'task_missing'
  | 'chat_session_ended'
  | 'explicit_cancel'
  | 'queue_shutdown'
  | 'deadline_exceeded'
  | 'queue_full'
  | 'process_restart';

export interface TombstoneReason {
  kind: TombstoneKind;
  reason?: string;
}

export interface TaskRef {
  task_id: string;
  agent_id?: string;
  chat_session_id?: string;
}

export interface JobOrigin {
  operation: string;
  caller?: string;
}

export interface TokenSummary {
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  reasoning_tokens: number;
}

export interface LocalPrepStat {
  blocks_processed: number;
  chars_in: number;
  chars_out: number;
  model: string;
  duration_ms: number;
}

export interface JobMeta {
  job_id: string;
  priority: Priority;
  task_ref?: TaskRef;
  origin: JobOrigin;
  provider?: string;
  model?: string;
  state: JobState;
  /** Unix millis. Use `new Date(submitted_at_ms)` for display. */
  submitted_at_ms: number;
  dispatched_at_ms?: number;
  completed_at_ms?: number;
  wait_ms?: number;
  /** Time waiting for the provider concurrency permit after worker pickup. */
  provider_wait_ms?: number;
  execution_ms?: number;
  tokens?: TokenSummary;
  tombstone?: TombstoneReason;
  error?: string;
  error_class?: ErrorClass;
  attempts: number;
  local_prep?: LocalPrepStat;
  idempotency_key?: string;
}

export interface RegistrySnapshot {
  pending: JobMeta[];
  in_flight: JobMeta[];
  completed: JobMeta[];
  failed: JobMeta[];
  tombstoned: JobMeta[];
}

export interface QueueSnapshot {
  workers_total: number;
  workers_busy: number;
  depth_high: number;
  depth_normal: number;
  depth_background: number;
  capacity_high: number;
  capacity_normal: number;
  capacity_background: number;
  waiting_for_provider: number;
  waiting_for_provider_high: number;
  waiting_for_provider_normal: number;
  waiting_for_provider_background: number;
  waiting_for_local_prep: number;
  registry: RegistrySnapshot;
}

export type LlmQueueEvent =
  | { event: 'submitted'; meta: JobMeta }
  | { event: 'waiting_for_provider'; meta: JobMeta; provider: string }
  | { event: 'waiting_for_local_prep'; meta: JobMeta }
  | { event: 'dispatched'; meta: JobMeta }
  | { event: 'attempt_done'; meta: JobMeta; success: boolean }
  | { event: 'requeued'; meta: JobMeta; cycle: number }
  | { event: 'completed'; meta: JobMeta }
  | { event: 'failed'; meta: JobMeta; error_class: ErrorClass }
  | { event: 'tombstoned'; meta: JobMeta; reason: TombstoneReason }
  | {
      event: 'provider_state_changed';
      provider: string;
      from: 'closed' | 'open' | 'half_open';
      to: 'closed' | 'open' | 'half_open';
      reason: string;
    }
  | { event: 'local_prep_skipped'; job_id: string; reason: string };
