// Lightweight store for the LlmQueuePanel viewer.
//
// Polls /api/llm/queue/snapshot on a timer; future enhancement plugs into
// the realtime event channel for live diffs.

import { writable, type Writable } from 'svelte/store';

import type { QueueSnapshot } from '../../realtime/llm-queue';

const DEFAULT_POLL_MS = 2000;

export interface LlmQueueStore {
  snapshot: Writable<QueueSnapshot | null>;
  error: Writable<string | null>;
  start: () => void;
  stop: () => void;
  refresh: () => Promise<void>;
  cancelJob: (jobId: string, reason: string) => Promise<boolean>;
}

export function createLlmQueueStore(
  baseUrl = '',
  pollMs = DEFAULT_POLL_MS
): LlmQueueStore {
  const snapshot = writable<QueueSnapshot | null>(null);
  const error = writable<string | null>(null);
  let timer: ReturnType<typeof setInterval> | null = null;

  async function fetchSnapshot() {
    try {
      const resp = await fetch(`${baseUrl}/api/llm/queue/snapshot`);
      if (!resp.ok) {
        throw new Error(`status ${resp.status}`);
      }
      const data = (await resp.json()) as QueueSnapshot;
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
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
  }

  async function cancelJob(jobId: string, reason: string): Promise<boolean> {
    try {
      const resp = await fetch(
        `${baseUrl}/api/llm/queue/cancel/${encodeURIComponent(jobId)}`,
        {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ reason }),
        }
      );
      if (!resp.ok) return false;
      const body = (await resp.json()) as { cancelled?: boolean };
      await fetchSnapshot();
      return Boolean(body.cancelled);
    } catch {
      return false;
    }
  }

  return {
    snapshot,
    error,
    start,
    stop,
    refresh: fetchSnapshot,
    cancelJob,
  };
}
