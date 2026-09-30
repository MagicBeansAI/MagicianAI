// Screen-observation status store.
//
// ONE shared poller for `/screen/observe/status` (see sharedPoll.ts):
// the TopBar's 👁 live dot keeps it alive from every page — the dot
// doubles as the rail's "visible while active" privacy posture — and
// the Observe page holds a `requestFastObservePolling()` lease for its
// live card instead of running its own interval. Mutations
// (start/stop/retarget, the ⇧⌥W chord's HUD) call
// `pollObserveStatusNow()`. Backend: magician's screen_observe rail
// (docs/components/magician/screen-capture-and-ask.md).

import type { Readable } from 'svelte/store';
import { createSharedPoll } from './sharedPoll';

export interface ObserveStatusInfo {
	observe_id: string;
	status: 'observing' | 'stopped' | 'failed' | 'idle';
	purpose?: string;
	mode?: 'notes' | 'watch';
	watch_for?: string | null;
	thread_id?: string;
	started_at_ms?: number;
	note_count?: number;
	alert_count?: number;
	/** Heard-audio transcript finals (when audio capture is on). */
	transcript_count?: number;
	/** Audio capture source: `none` | `system` | `mic` | `both`. */
	audio_source?: string;
	/** Canonical resolved Listening profile and provider. */
	audio_profile?: string | null;
	stt_provider?: string | null;
	/** Stable-screen high-detail read enabled for this observation. */
	deep_observation?: boolean;
	/** Seconds the same screen must remain stable before deep read. */
	deep_dwell_s?: number;
	/** Minimum seconds between deep reads of the same stable screen. */
	deep_repeat_s?: number;
	deep_note_count?: number;
	latest_summary?: string | null;
}

const statusPoll = createSharedPoll<ObserveStatusInfo | null>({
	fetcher: async () => {
		// Bearer auth is auto-injected by installScopedApiFetch.
		const res = await fetch('/api/magician/v2/screen/observe/status');
		// Non-OK (e.g. a pre-rebuild backend without the rail) throws so the
		// shared poll backs off instead of hammering a 404.
		if (!res.ok) throw new Error(`HTTP ${res.status}`);
		const data = (await res.json()) as ObserveStatusInfo;
		return data?.status ? data : null;
	},
	idleMs: 10_000,
	fastMs: 5_000
});

/** Latest observation status; `null` when idle or before the first fetch. */
export const observeStatus: Readable<ObserveStatusInfo | null> = statusPoll.value;

/** Refresh immediately after a mutation (start/stop/retarget). */
export const pollObserveStatusNow = statusPoll.pollNow;

/** Fast-cadence lease while a live-controls surface is open. Returns release. */
export const requestFastObservePolling = statusPoll.requestFast;
