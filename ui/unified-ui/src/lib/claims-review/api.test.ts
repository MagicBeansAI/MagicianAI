import { describe, expect, it } from 'vitest';
import { canConfirm, deliveryLabel, saveClaimDecision, saveCommitmentDecision, saveConversation, type Claim, type ConversationImport } from './api';
import { installFetchMock, jsonResponse } from '../../test/browser';

describe('Envoy review delivery', () => {
  const claim = { status: 'pending', transcript_key: 'envoy:s' } as Claim;
  it('acceptance is distinct from delivery and every unacknowledged state stays closed', () => {
    for (const status of ['prepared', 'dispatching', 'dispatch_unknown', 'failed']) {
      expect(canConfirm(claim, { status, channel: 'email', observed: false })).toBe(false);
    }
    expect(canConfirm(claim)).toBe(false);
    expect(canConfirm(claim, { status: 'provider_accepted', channel: 'email', observed: false })).toBe(true);
    expect(deliveryLabel({ status: 'provider_accepted', channel: 'email', observed: false })).toBe('Accepted by channel');
    expect(canConfirm({ ...claim, status: 'confirmed' }, { status: 'delivered', channel: 'email', observed: false })).toBe(false);
  });
});

describe('Claims Review write acknowledgements', () => {
  const at = '2026-09-27T10:00:00Z';
  const audience = { kind: 'account', id: 'account-1' };
  const claim = { claim_id: 'claim-1', stated_text: 'Delivery on Friday.', outward_act_ref: 'act-1',
    audience_ref: audience, stated_at: at, revision: 3 } as Claim;
  const command = { by: 'Alice', note: 'Checked the transcript', expected_revision: 3, decision_id: 'decision-1' };
  const signal = () => new AbortController().signal;
  function respond(data: unknown) { installFetchMock([{ method: 'POST', match: '/transcripts/', handle: () => jsonResponse(data) }]); }
  function receipt(verb: string, expected: number, resulting: number) {
    return { receipt_id: 'receipt-1', decision_id: command.decision_id, verb, expected_revision: expected,
      resulting_revision: resulting, request_fingerprint: 'fingerprint-1', disposition: 'applied', recorded_at: at };
  }
  function claimOutcome(action: 'confirm' | 'reject' = 'confirm') {
    return { claim: { ...claim, revision: 4, status: action === 'confirm' ? 'confirmed' : 'rejected',
      decided_by: 'Alice', decision_note: command.note, decided_at: at,
      assertion_use_ids: action === 'confirm' ? ['assertion-1'] : [] },
      receipt: { ...receipt(`${action}_claim`, 3, 4), claim_id: claim.claim_id, by: 'Alice' } };
  }
  it.each(['confirm', 'reject'] as const)('accepts an applied or replayed %s receipt', async (action) => {
    for (const disposition of ['applied', 'already_applied']) {
      const data = claimOutcome(action); data.receipt.disposition = disposition; respond(data);
      await expect(saveClaimDecision(claim.claim_id, action, command, signal())).resolves.toBeUndefined();
    }
  });
  it.each(['decision', 'target', 'verb', 'revision', 'reviewer', 'note', 'pending', 'assertions'])('rejects a claim acknowledgement with mismatched %s', async (field) => {
    const data = claimOutcome();
    if (field === 'decision') data.receipt.decision_id = 'another-decision';
    if (field === 'target') data.receipt.claim_id = data.claim.claim_id = 'another-claim';
    if (field === 'verb') data.receipt.verb = 'reject_claim';
    if (field === 'revision') data.claim.revision = 3;
    if (field === 'reviewer') data.claim.decided_by = data.receipt.by = 'Bob';
    if (field === 'note') data.claim.decision_note = 'Different note';
    if (field === 'pending') data.claim.status = 'pending';
    if (field === 'assertions') data.claim.assertion_use_ids = [];
    respond(data);
    await expect(saveClaimDecision(claim.claim_id, 'confirm', command, signal())).rejects.toThrow('incomplete response');
  });

  const term = { commitment_id: 'term-1', terms: claim.stated_text, direction: 'stated_by_us', revision: 1, status: 'unconfirmed' };
  function commitmentOutcome(confirm = false) {
    return { commitment: { ...term, audience, source_ref: claim.outward_act_ref, stated_at: at,
      revision: confirm ? 2 : 1, status: confirm ? 'confirmed' : 'unconfirmed',
      confirmed_by: confirm ? 'Alice' : null, confirmed_at: confirm ? at : null },
      receipt: { ...receipt(confirm ? 'confirm' : 'record', confirm ? 1 : 3, confirm ? 2 : 1),
        audience, commitment_id: term.commitment_id, by: confirm ? 'Alice' : null } };
  }
  const recordBody = { decision_id: command.decision_id, expected_claim_revision: 3 };
  const confirmBody = { decision_id: command.decision_id, expected_revision: 1, by: 'Alice', audience_kind: audience.kind, audience_id: audience.id };
  it('accepts separate commitment recording and confirmation receipts', async () => {
    respond(commitmentOutcome());
    await expect(saveCommitmentDecision('/claims/claim-1/commitment', recordBody, claim, undefined, signal())).resolves.toEqual({ status: 'unconfirmed' });
    respond(commitmentOutcome(true));
    await expect(saveCommitmentDecision('/commitments/term-1/confirm', confirmBody, claim, term, signal())).resolves.toEqual({ status: 'confirmed' });
  });
  it.each(['empty', 'decision', 'relationship', 'terms', 'source', 'confirmation instead of recording'])('refuses an unverifiable commitment recording: %s', async (field) => {
    const data = commitmentOutcome();
    if (field === 'decision') data.receipt.decision_id = 'different';
    if (field === 'relationship') data.commitment.audience = data.receipt.audience = { kind: 'account', id: 'other' };
    if (field === 'terms') data.commitment.terms = 'Different promise';
    if (field === 'source') data.commitment.source_ref = 'other-act';
    if (field === 'confirmation instead of recording') data.receipt.verb = 'confirm';
    respond(field === 'empty' ? {} : data);
    await expect(saveCommitmentDecision('/claims/claim-1/commitment', recordBody, claim, undefined, signal())).rejects.toThrow('incomplete response');
  });
  it('accepts an exact receipt replay after the commitment has advanced, without calling it unconfirmed', async () => {
    const recorded = commitmentOutcome();
    recorded.receipt.disposition = 'already_applied';
    recorded.commitment.revision = 2; recorded.commitment.status = 'confirmed';
    respond(recorded);
    await expect(saveCommitmentDecision('/claims/claim-1/commitment', recordBody, claim, undefined, signal())).resolves.toEqual({ status: 'confirmed' });
    const confirmed = commitmentOutcome(true);
    confirmed.receipt.disposition = 'already_applied';
    confirmed.commitment.revision = 3; confirmed.commitment.status = 'withdrawn';
    respond(confirmed);
    await expect(saveCommitmentDecision('/commitments/term-1/confirm', confirmBody, claim, term, signal())).resolves.toEqual({ status: 'withdrawn' });
  });
  it.each(['identity', 'reviewer', 'unconfirmed', 'withdrawn without a later revision'])('refuses a mismatched commitment confirmation: %s', async (field) => {
    const data = commitmentOutcome(true);
    if (field === 'identity') data.commitment.commitment_id = data.receipt.commitment_id = 'another-term';
    if (field === 'reviewer') data.commitment.confirmed_by = data.receipt.by = 'Bob';
    if (field === 'unconfirmed') data.commitment.status = 'unconfirmed';
    if (field === 'withdrawn without a later revision') data.commitment.status = 'withdrawn';
    respond(data);
    await expect(saveCommitmentDecision('/commitments/term-1/confirm', confirmBody, claim, term, signal())).rejects.toThrow('incomplete response');
  });

  const imported: ConversationImport = { transcript_key: 'owner-import:one', effective_speaker: 'Alice', attendees: ['Bob'],
    extracted_by: 'manual-import', occurred_at: at,
    utterances: [{ segment_key: 'statement', attribution: 'ours', speaker_id: 'Alice', spoken_text: claim.stated_text }] };
  function importOutcome() {
    return { outward_act_ref: 'act-1', utterances_seen: 1, skipped: [], claims: [{ ...claim, audience_ref: null,
      status: 'pending', revision: 1, transcript_key: imported.transcript_key, segment_key: 'statement',
      speaker: 'Alice', audience: ['Bob'], extracted_by: 'manual-import', stated_at: '2026-09-27T10:00:00+00:00' }] };
  }
  it('accepts an import and an identical retry whose claim has since been reviewed', async () => {
    for (const status of ['pending', 'confirmed', 'rejected']) {
      const data = importOutcome(); data.claims[0].status = status;
      data.claims[0].revision = status === 'pending' ? 1 : 2; respond(data);
      await expect(saveConversation(imported, signal())).resolves.toBeUndefined();
    }
  });
  it.each(['empty', 'missing statement', 'other import', 'changed words', 'changed recipient'])('refuses an unverifiable import: %s', async (field) => {
    const data = importOutcome();
    if (field === 'missing statement') data.claims = [];
    if (field === 'other import') data.claims[0].transcript_key = 'another-import';
    if (field === 'changed words') data.claims[0].stated_text = 'Different statement';
    if (field === 'changed recipient') data.claims[0].audience = ['Someone else'];
    respond(field === 'empty' ? {} : data);
    await expect(saveConversation(imported, signal())).rejects.toThrow('incomplete response');
  });
});
