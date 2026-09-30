import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type ClaimStatus = 'pending' | 'confirmed' | 'rejected';
export interface Claim {
  claim_id: string; stated_text: string; speaker: string; audience: string[];
  status: ClaimStatus; revision: number; transcript_key: string; segment_key: string;
  outward_act_ref: string; extracted_by: string; awaiting: string; stated_at: string;
  evidence_refs: string[]; decided_by?: string; decision_note?: string; decided_at?: string;
  audience_ref?: { kind: string; id: string };
}
export interface Delivery { status: string; channel: string; observed: boolean }
export interface ClaimPage {
  claims: Claim[]; count: number; pending: number;
  counts: Record<ClaimStatus, number>; delivery: Record<string, Delivery>; next_cursor: string | null;
}
export interface Commitment {
  commitment_id: string; terms: string; status: string; direction: string; revision: number;
  confirmed_by?: string;
}
export interface ConversationImport {
  transcript_key: string; effective_speaker: string; attendees: string[];
  extracted_by: string; occurred_at: string; audience_kind?: string; audience_id?: string;
  utterances: Array<{ segment_key: string; attribution: 'ours'; speaker_id: string; spoken_text: string }>;
}
export interface PendingConfirmation {
  decision_id: string; expected_revision: number; by: string; note: string | null;
  prepared_at: string;
}
const ROOT = '/api/magician/v2/transcripts';

export async function claimsRequest<T>(path: string, signal: AbortSignal, body?: unknown): Promise<T> {
  const response = await timedFetch(`${ROOT}${path}`, {
    method: body === undefined ? 'GET' : 'POST', signal,
    headers: scopedRequestHeaders(body === undefined ? undefined : { 'Content-Type': 'application/json' }),
    ...(body === undefined ? {} : { body: JSON.stringify(body) })
  });
  const data = await response.json().catch(() => null);
  if (!response.ok) {
    throw new Error(data?.message ?? data?.error?.message ?? (typeof data?.error === 'string' ? data.error : `Claims Review could not complete this request (${response.status}).`));
  }
  if (!data || typeof data !== 'object' || Array.isArray(data)) {
    throw new Error('Claims Review received an incomplete response. Retry the same request to check its outcome.');
  }
  return data as T;
}

export async function pendingConfirmation(claim: Claim, signal: AbortSignal): Promise<PendingConfirmation | null> {
  const data = await claimsRequest<{ confirmation: PendingConfirmation | null }>(`/claims/${encodeURIComponent(claim.claim_id)}/pending-confirmation`, signal);
  const saved = data.confirmation;
  if (saved === null) return null;
  if (!saved || typeof saved.decision_id !== 'string' || !saved.decision_id.trim()
      || !Number.isSafeInteger(saved.expected_revision) || saved.expected_revision !== claim.revision
      || typeof saved.by !== 'string' || !saved.by.trim()
      || (saved.note !== null && typeof saved.note !== 'string')) {
    throw new Error('The saved confirmation could not be verified. Refresh the claim before trying again.');
  }
  return saved;
}

export async function listClaims(state: ClaimStatus | 'all', text: string, after: string | undefined, signal: AbortSignal): Promise<ClaimPage> {
  const query = new URLSearchParams({ state, text, limit: '40' });
  if (after) query.set('after_claim_id', after);
  const page = await claimsRequest<ClaimPage>(`/claims?${query}`, signal);
  if (!page || !Array.isArray(page.claims) || !page.counts || !page.delivery ||
      page.claims.some((c) => !c.claim_id || !Number.isSafeInteger(c.revision) || typeof c.stated_text !== 'string' || !Array.isArray(c.audience))) {
    throw new Error('Claims Review received an incomplete response. Refresh after updating the Magician service.');
  }
  return page;
}

export function deliveryLabel(delivery?: Delivery): string {
  if (!delivery) return 'Delivery not recorded';
  if (delivery.observed) return 'Observed conversation';
  return ({ prepared: 'Not sent yet', dispatching: 'Awaiting send receipt', provider_accepted: 'Accepted by channel', delivered: 'Delivered', dispatch_unknown: 'Send outcome unknown', failed: 'Send failed', corrected: 'Corrected', retracted: 'Retracted' } as Record<string, string>)[delivery.status] ?? 'Delivery unknown';
}

export function canConfirm(claim: Claim, delivery?: Delivery): boolean {
  return claim.status === 'pending' && (!claim.transcript_key.startsWith('envoy:') ||
    delivery?.status === 'provider_accepted' || delivery?.status === 'delivered');
}

export interface ClaimDecisionRequest {
  by: string; note: string | null; expected_revision: number; decision_id: string;
}

export function decisionRequest(claim: Claim, by: string, note: string): ClaimDecisionRequest {
  return { by: by.trim(), note: note.trim() || null, expected_revision: claim.revision, decision_id: crypto.randomUUID() };
}

const INCOMPLETE_WRITE = 'Claims Review received an incomplete response for this request. Retry the same request to check its outcome.';
function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}
function named(value: unknown): value is string { return typeof value === 'string' && !!value.trim(); }
function revision(value: unknown): value is number { return Number.isSafeInteger(value) && Number(value) > 0; }
function sameTime(value: unknown, expected: unknown): boolean {
  return typeof value === 'string' && typeof expected === 'string'
    && Number.isFinite(Date.parse(value)) && Date.parse(value) === Date.parse(expected);
}
function sameAudience(value: unknown, expected: Claim['audience_ref']): boolean {
  return !!expected && record(value) && value.kind === expected.kind && value.id === expected.id;
}
function receiptMatches(value: unknown, id: unknown, verb: string, expected: unknown, resulting: number): value is Record<string, unknown> {
  return record(value) && named(id) && value.decision_id === id && value.verb === verb
    && revision(expected) && value.expected_revision === expected && revision(resulting)
    && value.resulting_revision === resulting && named(value.receipt_id) && named(value.request_fingerprint)
    && (value.disposition === 'applied' || value.disposition === 'already_applied')
    && typeof value.recorded_at === 'string' && Number.isFinite(Date.parse(value.recorded_at));
}

/** HTTP success alone cannot discard an immutable decision retry. */
export async function saveClaimDecision(claimId: string, action: 'confirm' | 'reject', body: ClaimDecisionRequest, signal: AbortSignal): Promise<void> {
  const data = await claimsRequest<Record<string, unknown>>(`/claims/${encodeURIComponent(claimId)}/${action}`, signal, body);
  const claim = data.claim;
  const receipt = data.receipt;
  if (!receiptMatches(receipt, body.decision_id, `${action}_claim`, body.expected_revision, body.expected_revision + 1)
      || receipt.claim_id !== claimId || receipt.by !== body.by || !record(claim)
      || claim.claim_id !== claimId || claim.revision !== receipt.resulting_revision
      || claim.status !== (action === 'confirm' ? 'confirmed' : 'rejected')
      || claim.decided_by !== body.by || (claim.decision_note ?? null) !== body.note
      || !sameTime(claim.decided_at, receipt.recorded_at)
      || (action === 'confirm' && (!Array.isArray(claim.assertion_use_ids) || !claim.assertion_use_ids.length
        || !claim.assertion_use_ids.every(named)))
      || (action === 'reject' && claim.assertion_use_ids != null
        && (!Array.isArray(claim.assertion_use_ids) || claim.assertion_use_ids.length !== 0))) {
    throw new Error(INCOMPLETE_WRITE);
  }
}

/** Bind the acknowledgement to the relationship and exact term being reviewed. */
export async function saveCommitmentDecision(path: string, body: Record<string, unknown>, claim: Claim, term: Commitment | undefined, signal: AbortSignal): Promise<{ status: string }> {
  const data = await claimsRequest<Record<string, unknown>>(path, signal, body);
  const receipt = data.receipt;
  const commitment = data.commitment;
  const expected = term ? body.expected_revision : body.expected_claim_revision;
  const resulting = term && typeof expected === 'number' ? expected + 1 : 1;
  if (!receiptMatches(receipt, body.decision_id, term ? 'confirm' : 'record', expected, resulting)
      || !sameAudience(receipt.audience, claim.audience_ref) || !record(commitment)
      || !named(commitment.commitment_id) || receipt.commitment_id !== commitment.commitment_id
      || !sameAudience(commitment.audience, claim.audience_ref)
      || !revision(commitment.revision) || commitment.revision < resulting
      || typeof commitment.status !== 'string' || !['unconfirmed', 'confirmed', 'superseded', 'withdrawn'].includes(commitment.status)
      || (commitment.revision > resulting && receipt.disposition !== 'already_applied')) {
    throw new Error(INCOMPLETE_WRITE);
  }
  if (term) {
    if (commitment.commitment_id !== term.commitment_id || commitment.terms !== term.terms
        || receipt.by !== body.by || commitment.confirmed_by !== body.by || commitment.status === 'unconfirmed'
        || (commitment.revision === resulting && commitment.status !== 'confirmed')
        || !sameTime(commitment.confirmed_at, receipt.recorded_at)) throw new Error(INCOMPLETE_WRITE);
  } else if (commitment.terms !== claim.stated_text || commitment.source_ref !== claim.outward_act_ref
      || commitment.direction !== 'stated_by_us' || (receipt.by ?? null) !== null
      || !sameTime(commitment.stated_at, claim.stated_at)
      || (commitment.revision === 1 && commitment.status !== 'unconfirmed')) {
    throw new Error(INCOMPLETE_WRITE);
  }
  // A replay returns the current term, which may have advanced since its receipt.
  return { status: commitment.status };
}

/** Manual imports contain only our nonempty statements; every one must be accounted for. */
export async function saveConversation(body: ConversationImport, signal: AbortSignal): Promise<void> {
  const data = await claimsRequest<Record<string, unknown>>('/ingest', signal, body);
  if (!named(data.outward_act_ref) || data.utterances_seen !== body.utterances.length
      || !Array.isArray(data.claims) || data.claims.length !== body.utterances.length
      || !Array.isArray(data.skipped) || data.skipped.length !== 0) throw new Error(INCOMPLETE_WRITE);
  const seen = new Set<string>();
  const segments = new Set<string>();
  for (const claim of data.claims) {
    const utterance = record(claim) && body.utterances.find((one) => one.segment_key === claim.segment_key);
    if (!record(claim) || !utterance || !named(claim.claim_id) || seen.has(claim.claim_id) || segments.has(utterance.segment_key)
        || claim.transcript_key !== body.transcript_key || claim.outward_act_ref !== data.outward_act_ref
        || claim.stated_text !== utterance.spoken_text || claim.speaker !== utterance.speaker_id
        || claim.extracted_by !== body.extracted_by || !sameTime(claim.stated_at, body.occurred_at)
        || !Array.isArray(claim.audience) || JSON.stringify(claim.audience) !== JSON.stringify(body.attendees)
        || !revision(claim.revision) || !['pending', 'confirmed', 'rejected'].includes(String(claim.status))
        || (body.audience_id ? !sameAudience(claim.audience_ref, { kind: body.audience_kind!, id: body.audience_id })
          : claim.audience_ref != null)) throw new Error(INCOMPLETE_WRITE);
    seen.add(claim.claim_id);
    segments.add(utterance.segment_key);
  }
}
