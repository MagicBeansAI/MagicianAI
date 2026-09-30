// Meetings surface store — live capture indicator + shared types.
//
// ONE shared poller for `/meetings/active` (see sharedPoll.ts): the
// TopBar dot keeps it alive from every page at the idle cadence; the
// Observe page holds a `requestFastMeetingsPolling()` lease for snappy
// session controls instead of running its own interval, and mutations
// (listen/join/pause/stop) call `pollMeetingsActiveNow()`. Capture must
// never be hidden background behavior — the dot rides this store.

import { derived, type Readable } from 'svelte/store';
import { createSharedPoll } from './sharedPoll';

export interface ActiveMeetingSession {
	session_id: string;
	mode: 'passive' | 'attendee';
	status: string;
	thread_id: string | null;
	title: string | null;
	url: string | null;
	mic?: boolean;
	/// Capture paused ("Mute assistant" on the attendee rail).
	paused?: boolean;
	latest_summary?: string | null;
}

export interface RecentMeetingThread {
	thread_id: string;
	session_id: string;
	title: string | null;
	agent_id: string;
	updated_at: number;
}

/// Display label for an active capture row — one fallback chain everywhere a
/// session is named (Meetings page, Attention section).
export function meetingSessionLabel(s: ActiveMeetingSession): string {
	return s.title ?? s.url ?? s.thread_id ?? s.session_id;
}

/// One calendar event from `GET /meetings/upcoming`.
export interface UpcomingMeeting {
	event_id: string | null;
	title: string;
	start: string | null;
	end: string | null;
	meet_url: string | null;
	/// Within the event's time window right now (5-min early-join grace).
	live_now: boolean;
	/// Which gws account's calendar the event came from (multi-account merge).
	account?: string;
}

const activePoll = createSharedPoll<ActiveMeetingSession[]>({
	fetcher: async () => {
		// Bearer auth is auto-injected by installScopedApiFetch.
		const res = await fetch('/api/magician/v2/meetings/active');
		if (!res.ok) throw new Error(`HTTP ${res.status}`);
		const data = await res.json();
		return Array.isArray(data.active) ? data.active : [];
	},
	idleMs: 20_000,
	fastMs: 5_000
});

/** Active capture sessions (both rails); `null` until the first fetch lands. */
export const meetingsActive: Readable<ActiveMeetingSession[] | null> = activePoll.value;

export const meetingsActiveCount = derived(meetingsActive, (sessions) => sessions?.length ?? 0);

/** Refresh immediately after a mutation (listen/join/pause/resume/stop). */
export const pollMeetingsActiveNow = activePoll.pollNow;

/** Fast-cadence lease while a live-controls surface is open. Returns release. */
export const requestFastMeetingsPolling = activePoll.requestFast;
