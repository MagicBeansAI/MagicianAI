import type {
	RecentMeetingThread,
	UpcomingMeeting
} from '$lib/stores/meetingsStore';

export interface UpcomingMeetingFailure {
	account?: string;
	error?: string;
}

export interface UpcomingMeetingsResult {
	events: UpcomingMeeting[];
	errors: UpcomingMeetingFailure[];
}

export interface WatchSession {
	id: string;
	title?: string;
	updated_at?: number;
}

export interface AmbientStatus {
	enabled: boolean;
	denylist: string[];
	total_signals: number;
	total_rejected: number;
	last_signal_at: string | null;
	paired: boolean;
	buffered_signals?: number;
	accepted_today?: number;
	pages_today?: number;
	distinct_pages_today?: number;
	origins_today?: number;
	bytes_today?: {
		batch_raw_bytes?: number;
		signal_payload_bytes?: number;
		signal_metadata_bytes?: number;
		dom_estimated_bytes?: number;
	};
	worker?: {
		running?: boolean;
		last_run_status?: string | null;
		current_run_id?: string | null;
	};
	pending?: {
		clusters_due?: number;
		clusters_failed?: number;
		review_pending?: number;
	};
	llm_today?: {
		calls?: number;
		cost_usd?: number;
	};
}

export interface StartMeetingListenInput {
	title: string | null;
	url: string | null;
	mic: boolean;
}

export interface JoinMeetingInput {
	url: string;
	title: string | null;
}

export interface StartScreenObservationInput {
	purpose?: string;
	mode: 'notes' | 'watch';
	watch_for?: string;
	deep_observation: boolean;
	audio?: 'system' | 'mic' | 'both';
}

async function responseError(response: Response, fallback: string): Promise<Error> {
	const body = await response.json().catch(() => null);
	return new Error(body?.error ?? fallback);
}

export async function fetchRecentMeetings(): Promise<RecentMeetingThread[]> {
	const response = await fetch('/api/magician/v2/meetings');
	if (!response.ok) throw new Error(`HTTP ${response.status}`);
	const body = await response.json();
	return Array.isArray(body?.recent) ? body.recent : [];
}

export async function fetchUpcomingMeetings(
	forceRefresh = false
): Promise<UpcomingMeetingsResult> {
	const suffix = forceRefresh ? '?refresh=true' : '';
	const response = await fetch(`/api/magician/v2/meetings/upcoming${suffix}`);
	if (!response.ok) throw new Error(`HTTP ${response.status}`);
	const body = await response.json();
	return {
		events: Array.isArray(body?.events) ? body.events : [],
		errors: Array.isArray(body?.errors) ? body.errors : []
	};
}

export async function startMeetingListen(input: StartMeetingListenInput): Promise<void> {
	const response = await fetch('/api/magician/v2/meetings/listen', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(input)
	});
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
}

export async function joinMeeting(input: JoinMeetingInput): Promise<void> {
	const response = await fetch('/api/magician/v2/meetings/join', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(input)
	});
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
}

export async function setMeetingPaused(sessionId: string, paused: boolean): Promise<void> {
	const verb = paused ? 'resume' : 'pause';
	const response = await fetch(
		`/api/magician/v2/meetings/${encodeURIComponent(sessionId)}/${verb}`,
		{ method: 'POST' }
	);
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
}

export async function stopMeetingSession(sessionId: string): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/meetings/${encodeURIComponent(sessionId)}/stop`,
		{ method: 'POST', signal: AbortSignal.timeout(10_000) }
	);
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
}

export async function fetchRecentWatchSessions(): Promise<WatchSession[]> {
	const response = await fetch('/api/magician/v2/chat/sessions?ui_thread_id=screen-watch');
	if (!response.ok) throw new Error(`HTTP ${response.status}`);
	const body = await response.json();
	return (Array.isArray(body?.sessions) ? body.sessions : []).slice(0, 20);
}

export async function startScreenObservation(
	input: StartScreenObservationInput
): Promise<void> {
	const response = await fetch('/api/magician/v2/screen/observe/start', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(input)
	});
	if (!response.ok) {
		throw await responseError(response, `start failed (${response.status})`);
	}
}

export async function stopScreenObservation(): Promise<void> {
	const response = await fetch('/api/magician/v2/screen/observe/stop', { method: 'POST' });
	if (!response.ok) {
		throw await responseError(response, `stop failed (${response.status})`);
	}
}

export async function retargetScreenObservation(
	input: Record<string, unknown>
): Promise<void> {
	const response = await fetch('/api/magician/v2/screen/observe/retarget', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(input)
	});
	if (!response.ok) {
		throw await responseError(response, `retarget failed (${response.status})`);
	}
}

export async function fetchAmbientStatus(): Promise<AmbientStatus> {
	const response = await fetch('/api/magician/v2/ambient/status');
	if (!response.ok) throw new Error(`HTTP ${response.status}`);
	return (await response.json()) as AmbientStatus;
}

export async function saveAmbientConfig(enabled: boolean, denylist: string[]): Promise<void> {
	const response = await fetch('/api/magician/v2/ambient/config', {
		method: 'PUT',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ enabled, denylist })
	});
	if (!response.ok) {
		throw await responseError(response, `config update failed (${response.status})`);
	}
}

export async function enrollAmbientBrowser(label = 'browser'): Promise<string | null> {
	const response = await fetch('/api/magician/v2/ambient/enroll', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ label })
	});
	if (!response.ok) {
		throw await responseError(response, `pairing failed (${response.status})`);
	}
	const body = await response.json().catch(() => ({}));
	return typeof body?.token === 'string' ? body.token : null;
}
