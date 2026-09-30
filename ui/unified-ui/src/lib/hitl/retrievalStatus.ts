/**
 * Automatic verification-code retrieval status (secure HITL P6).
 *
 * The runtime's resolver publishes `VerificationRetrievalStatus` on the
 * scoped realtime feed for one `otp` ask — never the code, never the
 * message — and `GET /api/magician/v2/hitl/{id}/retrieval` answers the same
 * for a prompt that opens after the fact. The prompt shows it as a single
 * line so the owner knows whether to wait a moment or type the code now.
 */
import { v2Events } from '$lib/realtime/v2-websocket';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type RetrievalState = 'none' | 'waiting' | 'code_used' | 'ambiguous' | 'unavailable' | 'stopped';

export interface RetrievalStatus {
	correlationId: string;
	status: RetrievalState;
	sources: string[];
	reason: string | null;
}

const STATES: RetrievalState[] = ['none', 'waiting', 'code_used', 'ambiguous', 'unavailable', 'stopped'];

export function parseRetrievalStatus(raw: unknown, correlationId: string): RetrievalStatus {
	const record = typeof raw === 'object' && raw !== null ? (raw as Record<string, unknown>) : {};
	const status = typeof record.status === 'string' && (STATES as string[]).includes(record.status)
		? (record.status as RetrievalState)
		: 'none';
	return {
		correlationId,
		status,
		sources: Array.isArray(record.sources)
			? record.sources.filter((s): s is string => typeof s === 'string')
			: [],
		reason: typeof record.reason === 'string' ? record.reason : null
	};
}

export async function fetchRetrievalStatus(correlationId: string): Promise<RetrievalStatus> {
	try {
		const response = await fetch(`/api/magician/v2/hitl/${encodeURIComponent(correlationId)}/retrieval`, {
			headers: scopedRequestHeaders({ Accept: 'application/json' }),
			cache: 'no-store'
		});
		if (!response.ok) return parseRetrievalStatus(null, correlationId);
		return parseRetrievalStatus(await response.json(), correlationId);
	} catch {
		return parseRetrievalStatus(null, correlationId);
	}
}

/**
 * Follow one ask's retrieval status: the current state once (the feed may
 * have said its piece before the prompt opened), then every status event
 * the resolver publishes for it, and the current state again whenever the
 * feed reconnects (events missed meanwhile). Returns the unsubscribe.
 */
export function subscribeRetrievalStatus(
	correlationId: string,
	onStatus: (status: RetrievalStatus) => void
): () => void {
	let live = true;
	const refresh = async () => {
		const next = await fetchRetrievalStatus(correlationId);
		if (live) onStatus(next);
	};
	void refresh();
	const unsubscribeEvents = v2Events.subscribe((events) => {
		for (const event of events) {
			if (String(event.event_type) !== 'VerificationRetrievalStatus') continue;
			const data = (event as { data?: Record<string, unknown> }).data ?? {};
			if (data.correlation_id !== correlationId) continue;
			if (live) onStatus(parseRetrievalStatus(data, correlationId));
		}
	});
	let sawConnected = false;
	const unsubscribeConnection = v2Events.connectionStatus.subscribe((status) => {
		if (status !== 'connected') return;
		// The first `connected` is the state already fetched above; a later
		// one is a reconnect with a gap to close.
		if (sawConnected) void refresh();
		sawConnected = true;
	});
	return () => {
		live = false;
		unsubscribeEvents();
		unsubscribeConnection();
	};
}

const SOURCE_WORDS: Record<string, string> = {
	gmail: 'email',
	agentmail: 'the service inbox',
	messages: 'Messages',
	android_notification: 'your phone'
};

/** The one line the prompt shows; `null` when there is nothing to say. */
export function retrievalStatusLine(status: RetrievalStatus): string | null {
	const sources = status.sources.map((s) => SOURCE_WORDS[s] ?? s);
	const where = sources.length === 0 ? '' : ` from ${sources.join(' or ')}`;
	switch (status.status) {
		case 'waiting':
			return `Waiting for the verification code${where}… you can also type it below.`;
		case 'code_used':
			return 'Code received and used.';
		case 'ambiguous':
			return 'More than one code arrived — enter the right one below.';
		case 'unavailable':
			return status.sources.length === 0
				? null
				: 'Automatic retrieval could not find the code — enter it below.';
		default:
			return null;
	}
}
