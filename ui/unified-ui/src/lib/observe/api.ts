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
	realtime_profile?: string;
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

export interface WhatsAppRingProfile {
	id: string;
	label: string;
	available?: boolean;
	unavailable_reason?: string | null;
}

export interface WhatsAppEngineChoice {
	id: string;
	label: string;
	available: boolean;
	unavailable_reason: string | null;
}

type WhatsAppCatalogProfile = {
	profile_id: string;
	label: string;
	available?: boolean;
	unavailable_reason?: string | null;
	mode?: string;
	topology?: string;
};

export const WHATSAPP_DIRECT_PROFILE = 'voice_realtime_default';
export const WHATSAPP_MOUTH_PROFILE = 'voice_realtime_openai_backend';

export interface WhatsAppRingView {
	phase: string;
	surface: 'ringing' | 'ongoing' | 'none';
	profile: string;
	profiles: WhatsAppRingProfile[];
	url: string;
	title: string;
	caller: string | null;
	session_id: string | null;
	error: string | null;
	starting: boolean;
	leaving: boolean;
}

export type WhatsAppCallAction = 'join' | 'listen' | 'cancel' | 'leave' | 'stop';

const WHATSAPP_REALTIME_2 = WHATSAPP_MOUTH_PROFILE;
const WHATSAPP_LIVE_1 = 'voice_realtime_gpt_live_1';

export function emptyWhatsAppRing(): WhatsAppRingView {
	return {
		phase: 'idle',
		surface: 'none',
		profile: WHATSAPP_REALTIME_2,
		profiles: [
			{ id: WHATSAPP_REALTIME_2, label: 'GPT Realtime 2', available: true },
			{ id: WHATSAPP_LIVE_1, label: 'GPT Live 1', available: true }
		],
		url: 'https://web.whatsapp.com/',
		title: 'WhatsApp call',
		caller: null,
		session_id: null,
		error: null,
		starting: false,
		leaving: false
	};
}

export const WHATSAPP_BROWSER_BAR_KEY = 'magician.whatsapp.browserBar';

/** The browser call buttons stay visible until this page turns them off. */
export function readWhatsAppBrowserBar(storage: Pick<Storage, 'getItem'>): boolean {
	try {
		const raw = storage.getItem(WHATSAPP_BROWSER_BAR_KEY);
		if (raw == null) return true;
		return raw !== '0' && raw !== 'false';
	} catch {
		return true;
	}
}

export function writeWhatsAppBrowserBar(enabled: boolean, storage: Pick<Storage, 'setItem'>): void {
	storage.setItem(WHATSAPP_BROWSER_BAR_KEY, enabled ? '1' : '0');
}

/** Tell every open tab whether to show the WhatsApp call buttons. */
export function notifyWhatsAppBrowserBar(enabled: boolean): void {
	const host = (globalThis as { window?: { dispatchEvent?: (event: Event) => boolean } }).window;
	if (!host || typeof host.dispatchEvent !== 'function') return;
	host.dispatchEvent(
		new CustomEvent('magician-whatsapp-browser-bar', {
			detail: { enabled }
		})
	);
}

/** Tell the open WhatsApp tab to click Accept, or to hang up. The Magican
 * content script on this page forwards the event. The server post still
 * starts or stops the voice. */
export function notifyWhatsAppTab(
	decision: 'join' | 'listen' | 'leave' | 'cancel',
	profile?: string
): void {
	const host = (globalThis as { window?: { dispatchEvent?: (event: Event) => boolean } }).window;
	if (!host || typeof host.dispatchEvent !== 'function') return;
	host.dispatchEvent(
		new CustomEvent('magician-whatsapp-act', {
			detail: { decision, profile: profile || '' }
		})
	);
}

/** Buttons for the call that is actually on screen. Idle has none. */
export function whatsAppCallActions(
	phase: string,
	surface: WhatsAppRingView['surface'] = 'none'
): WhatsAppCallAction[] {
	switch (phase) {
		case 'incoming':
			// A ring from the previous server has no surface. Join still works
			// there. Listen and Cancel need the server that knows those decisions.
			return surface === 'none' ? ['join'] : ['join', 'listen', 'cancel'];
		case 'ongoing':
			return ['listen', 'cancel'];
		case 'approved':
			return ['leave'];
		case 'listening':
			return ['stop'];
		case 'leaving':
			return ['leave'];
		case 'failed':
			if (surface === 'ringing') return ['join', 'listen'];
			if (surface === 'ongoing') return ['listen'];
			return [];
		default:
			return [];
	}
}

export function whatsAppRingSummary(
	view: Pick<WhatsAppRingView, 'phase' | 'caller' | 'surface' | 'starting'>,
	missing: boolean
): string {
	if (missing) return 'This Magician process does not handle WhatsApp calls yet.';
	const who = view.caller || 'Someone';
	switch (view.phase) {
		case 'incoming':
			return `${who} is calling on WhatsApp. Join answers and Magician talks. Listen answers and transcribes.`;
		case 'ongoing':
			return `${who} is on a WhatsApp call. Listen transcribes it.`;
		case 'approved':
			return view.starting ? 'Starting the voice on this call.' : 'Magician is on the call.';
		case 'listening':
			return view.starting ? 'Starting to listen.' : 'Magician is listening to this call.';
		case 'leaving':
			return 'Leaving the call.';
		case 'cancelled':
			return 'Cancelling the WhatsApp call.';
		case 'failed':
			return 'Magician could not join that call.';
		default:
			return 'Waiting for a WhatsApp call in this Chrome. Set the Chrome microphone to BlackHole 16ch.';
	}
}

function normalizeWhatsAppRing(body: unknown): WhatsAppRingView {
	const fallback = emptyWhatsAppRing();
	const record = body && typeof body === 'object' ? (body as Record<string, unknown>) : {};
	const profiles = Array.isArray(record.profiles)
		? record.profiles
				.map((item): WhatsAppRingProfile | null => {
					const row = item && typeof item === 'object' ? (item as Record<string, unknown>) : {};
					const id = typeof row.id === 'string' ? row.id : '';
					const label = typeof row.label === 'string' ? row.label : '';
					if (!id || !label) return null;
					const reason = row.unavailable_reason;
					return {
						id,
						label,
						available: row.available !== false,
						unavailable_reason: typeof reason === 'string' && reason ? reason : null
					};
				})
				.filter((item): item is WhatsAppRingProfile => item !== null)
		: [];
	const surface = record.surface === 'ringing' || record.surface === 'ongoing' ? record.surface : 'none';
	return {
		phase: typeof record.phase === 'string' && record.phase ? record.phase : fallback.phase,
		surface,
		profile:
			typeof record.profile === 'string' && record.profile ? record.profile : fallback.profile,
		profiles: profiles.length > 0 ? profiles : fallback.profiles,
		url: typeof record.url === 'string' && record.url ? record.url : fallback.url,
		title: typeof record.title === 'string' && record.title ? record.title : fallback.title,
		caller: typeof record.caller === 'string' && record.caller.trim() ? record.caller.trim() : null,
		session_id: typeof record.session_id === 'string' ? record.session_id : null,
		error: typeof record.error === 'string' && record.error ? record.error : null,
		starting: record.starting === true,
		leaving: record.leaving === true
	};
}

/** Drop the browser WebRTC engine. It cannot play into the Chrome call. */
export function whatsAppMouthCorrection(profile: string, phase: string): string | null {
	if (profile !== WHATSAPP_DIRECT_PROFILE) return null;
	if (phase === 'approved' || phase === 'listening' || phase === 'leaving') return null;
	return WHATSAPP_MOUTH_PROFILE;
}

export function whatsAppLiveOnObserve(phase: string, surface: WhatsAppRingView['surface']): boolean {
	switch (phase) {
		case 'incoming':
		case 'ongoing':
		case 'approved':
		case 'listening':
		case 'leaving':
			return true;
		case 'failed':
			return surface === 'ringing' || surface === 'ongoing';
		default:
			return false;
	}
}

export function whatsAppShowsOnActiveList(
	ring: Pick<WhatsAppRingView, 'phase' | 'surface'>,
	sessions: readonly { url?: string | null }[]
): boolean {
	if (!whatsAppLiveOnObserve(ring.phase, ring.surface)) return false;
	if (ring.phase !== 'approved' && ring.phase !== 'listening' && ring.phase !== 'leaving') {
		return true;
	}
	return !sessions.some((session) => (session.url || '').includes('web.whatsapp.com'));
}

export function whatsAppSessionBadge(phase: string, agentName: string): string {
	switch (phase) {
		case 'incoming':
			return 'Incoming WhatsApp call';
		case 'ongoing':
			return 'WhatsApp call';
		case 'approved':
			return `Joined as ${agentName}`;
		case 'listening':
			return 'Passive listening';
		case 'leaving':
			return 'Leaving WhatsApp call';
		default:
			return 'WhatsApp call';
	}
}

/** Composer catalog when it has loaded; otherwise the ring's own list. */
export function whatsAppEngineChoices(
	catalog: readonly WhatsAppCatalogProfile[] | null | undefined,
	ringProfiles: readonly WhatsAppRingProfile[],
	currentProfile: string,
	options?: { mouthsOnly?: boolean }
): WhatsAppEngineChoice[] {
	const mouthsOnly = options?.mouthsOnly === true;
	const fromCatalog = (catalog ?? [])
		.filter((profile) => profile.mode !== 'translation' && profile.profile_id && profile.label)
		.filter((profile) => !mouthsOnly || profile.topology !== 'direct_peer_to_peer')
		.map((profile) => ({
			id: profile.profile_id,
			label: profile.label,
			available: profile.available !== false,
			unavailable_reason: profile.unavailable_reason ?? null
		}));
	const listed =
		fromCatalog.length > 0
			? fromCatalog
			: ringProfiles
					.filter((profile) => profile.id && profile.label)
					.filter((profile) => !mouthsOnly || profile.id !== WHATSAPP_DIRECT_PROFILE)
					.map((profile) => ({
						id: profile.id,
						label: profile.label,
						available: profile.available !== false,
						unavailable_reason: profile.unavailable_reason ?? null
					}));
	if (
		currentProfile &&
		!(mouthsOnly && currentProfile === WHATSAPP_DIRECT_PROFILE) &&
		!listed.some((choice) => choice.id === currentProfile)
	) {
		listed.push({
			id: currentProfile,
			label: currentProfile,
			available: true,
			unavailable_reason: null
		});
	}
	return listed;
}

export async function fetchWhatsAppRing(): Promise<WhatsAppRingView | null> {
	const response = await fetch('/api/magician/v2/meetings/whatsapp-ring');
	if (response.status === 404) return null;
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
	return normalizeWhatsAppRing(await response.json());
}

export async function decideWhatsAppRing(input: {
	decision: string;
	profile?: string;
}): Promise<WhatsAppRingView> {
	const response = await fetch('/api/magician/v2/meetings/whatsapp-ring', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(input)
	});
	if (response.status === 404) {
		throw new Error('This Magician process does not handle WhatsApp calls yet.');
	}
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
	return normalizeWhatsAppRing(await response.json());
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
