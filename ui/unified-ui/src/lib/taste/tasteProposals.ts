import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { createSharedPoll } from '$lib/stores/sharedPoll';

export type ProposalKind = 'add' | 'retract';

export interface TasteProposal {
	id: string;
	/** `retract` asks to remove a directive already governing every run. */
	kind: ProposalKind;
	directive: string;
	/** Verbatim quotes the directive was inferred from. */
	evidence: string[];
	destination: string;
	confidence: number;
	source_session: string;
	proposed_at: string;
}

export interface CaptureStats {
	pending: number;
	approved: number;
	rejected: number;
	/** Null before the first decision — a ratio over no samples is not 0%. */
	accept_rate: number | null;
	/** True when accept-rate is below half over a meaningful sample. */
	needs_attention: boolean;
	corrupt_lines: number;
}

export interface TasteProposalsEnvelope {
	principal: string;
	workspace: string;
	/** False when capture is off, or this process serves no owner surface. */
	enabled: boolean;
	proposals: TasteProposal[];
	stats?: CaptureStats;
	capture_health?: { completed: number; empty: number; filed: number; failed_attempts: number; pending_retries: number; last_failure: string | null } | null;
	capture_health_unavailable?: boolean;
}

export interface DecisionResult {
	id: string;
	status: 'approved' | 'rejected';
	/**
	 * False on approve when the directive was already in the profile note —
	 * a repeat of a decision whose note write had already landed. Surfaced so
	 * the UI can say "already applied" rather than implying it just happened.
	 */
	placed: boolean;
}

const ENDPOINT = '/api/magician/v2/taste-proposals';

export async function fetchTasteProposals(): Promise<TasteProposalsEnvelope> {
	const response = await fetch(ENDPOINT, { headers: scopedRequestHeaders() });
	if (!response.ok) {
		throw new Error(`taste proposals unavailable (${response.status})`);
	}
	return (await response.json()) as TasteProposalsEnvelope;
}

async function decide(id: string, action: 'approve' | 'reject'): Promise<DecisionResult> {
	const response = await fetch(`${ENDPOINT}/${encodeURIComponent(id)}/${action}`, {
		method: 'POST',
		headers: scopedRequestHeaders()
	});
	if (response.status === 404) {
		// Someone decided it elsewhere, or the queue moved under us. Not an
		// error worth a red banner — the right response is to refresh.
		throw new Error('That proposal is no longer waiting.');
	}
	if (response.status === 409) {
		// An approved retraction whose line is no longer in the note, or
		// appears twice. The proposal stays pending by design; surface the
		// server's explanation rather than a generic failure, because the
		// owner needs to know nothing was removed.
		const body = (await response.json().catch(() => null)) as { error?: string } | null;
		throw new Error(body?.error ?? 'That line could not be removed; nothing was changed.');
	}
	if (!response.ok) {
		throw new Error(`could not ${action} (${response.status})`);
	}
	return (await response.json()) as DecisionResult;
}

export const approveTasteProposal = (id: string) => decide(id, 'approve');
export const rejectTasteProposal = (id: string) => decide(id, 'reject');

/**
 * Shared poll for the review queue.
 *
 * The panel is not a live-controls surface, but proposals arrive
 * **asynchronously** — the capture worker sweeps on an interval, so a queue
 * that only loads on mount shows the owner "nothing waiting" while proposals
 * sit unread until they happen to reload the page. That is the whole feature
 * failing quietly.
 *
 * Idle cadence is slow on purpose: the producer runs every 15 minutes, so
 * polling faster than that spends requests to learn nothing. The subscription
 * is module-level so several mounts share one timer, and the backoff inside
 * `createSharedPoll` handles a backend that is down without hammering it.
 */
export const tasteProposalsPoll = createSharedPoll<TasteProposalsEnvelope>({
	fetcher: fetchTasteProposals,
	idleMs: 60_000,
	fastMs: 15_000
});
