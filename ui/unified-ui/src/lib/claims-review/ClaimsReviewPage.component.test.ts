import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import { installFetchMock as installRoutes, jsonResponse, type MockFetchRoute } from '../../test/browser';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import ClaimsReviewPage from './ClaimsReviewPage.svelte';

const claim = { claim_id: 'claim-1', stated_text: 'The office opens at nine.', speaker: 'envoy', audience: ['telegram:42'], status: 'pending', revision: 3, transcript_key: 'envoy:session-1', segment_key: 'message-1', outward_act_ref: 'act-1', extracted_by: 'envoy-reply-capture', stated_at: '2026-09-27T10:00:00Z', evidence_refs: [] };
function page(status = 'provider_accepted') { return { claims: [claim], count: 1, pending: 1, counts: { pending: 1, confirmed: 0, rejected: 0 }, delivery: { 'claim-1': { status, channel: 'room', observed: false } }, next_cursor: null }; }
function installFetchMock(routes: MockFetchRoute[]) {
  return installRoutes([
    { method: 'GET', match: '/pending-confirmation', handle: () => jsonResponse({ confirmation: null }) },
    ...routes
  ]);
}

describe('Claims Review', () => {
  it('shows uncertain delivery without allowing confirmation', async () => {
    installFetchMock([{ match: '/transcripts/claims', handle: () => jsonResponse(page('dispatch_unknown')) }]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    expect(screen.getByRole('button', { name: 'Confirm statement' })).toBeDisabled();
    expect(screen.getAllByText('Send outcome unknown').length).toBeGreaterThan(0);
    expect(screen.getByRole('button', { name: 'Reject' })).toBeEnabled();
  });

  it('requires a named decision, binds its revision, and retries the same id after response loss', async () => {
    const bodies: unknown[] = [];
    installFetchMock([
      { method: 'POST', match: '/confirm', handle: ({ init }) => { bodies.push(JSON.parse(String(init?.body))); return jsonResponse({ message: 'Connection interrupted' }, { status: 503 }); } },
      { match: '/transcripts/claims', handle: () => jsonResponse(page()) }
    ]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm statement' }));
    expect(await screen.findByRole('button', { name: 'Save decision' })).toBeDisabled();
    await fireEvent.input(screen.getByLabelText('Your name'), { target: { value: 'Alice' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save decision' }));
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Connection interrupted'));
    await fireEvent.click(screen.getByRole('button', { name: 'Retry decision' }));
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[0]).toEqual(bodies[1]);
    expect(bodies[0]).toMatchObject({ by: 'Alice', expected_revision: 3 });
  });

  it('recovers an interrupted confirmation after remount without applying it until the owner retries', async () => {
    const bodies: any[] = [];
    let saved: any = null;
    installRoutes([
      { method: 'GET', match: '/pending-confirmation', handle: () => jsonResponse({ confirmation: saved }) },
      { method: 'POST', match: '/confirm', handle: ({ init }) => {
        const body = JSON.parse(String(init?.body)); bodies.push(body);
        saved = { ...body, prepared_at: '2026-09-27T10:01:00Z' };
        return jsonResponse({ error: 'Write interrupted' }, { status: 503 });
      } },
      { match: '/transcripts/claims', handle: () => jsonResponse(page()) }
    ]);
    const first = render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm statement' }));
    await fireEvent.input(await screen.findByLabelText('Your name'), { target: { value: 'Alice' } });
    await fireEvent.input(screen.getByLabelText(/Review note/), { target: { value: 'Checked original words' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save decision' }));
    await screen.findByRole('alert');
    first.unmount();
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
    await screen.findByRole('button', { name: 'Retry decision' });
    expect(screen.getByRole('status')).toHaveTextContent('previous confirmation was interrupted');
    expect(screen.getByLabelText('Your name')).toHaveValue('Alice');
    expect(screen.getByLabelText('Your name')).toBeDisabled();
    expect(bodies).toHaveLength(1);
    await fireEvent.click(screen.getByRole('button', { name: 'Retry decision' }));
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[1]).toEqual(bodies[0]);
  });

  it.each(['html', 'empty object', 'error object'])('retains the exact decision when a successful HTTP response contains %s', async (kind) => {
    const bodies: any[] = [];
    installFetchMock([
      { method: 'POST', match: '/confirm', handle: ({ init }) => {
        bodies.push(JSON.parse(String(init?.body)));
        return kind === 'html' ? new Response('<html>temporarily unavailable</html>', { status: 200 })
          : jsonResponse(kind === 'empty object' ? {} : { error: 'Temporarily unavailable' });
      } },
      { match: '/transcripts/claims', handle: () => jsonResponse(page()) }
    ]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm statement' }));
    await fireEvent.input(await screen.findByLabelText('Your name'), { target: { value: 'Alice' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save decision' }));
    await screen.findByRole('alert');
    expect(screen.getByRole('alert')).toHaveTextContent('incomplete response');
    expect(screen.queryByText(/Statement confirmed\./)).not.toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Retry decision' }));
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[1]).toEqual(bodies[0]);
  });

  it('does not offer a new decision when saved-confirmation recovery fails', async () => {
    installRoutes([
      { method: 'GET', match: '/pending-confirmation', handle: () => jsonResponse({ error: 'Recovery unavailable' }, { status: 503 }) },
      { match: '/transcripts/claims', handle: () => jsonResponse(page()) }
    ]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
    await screen.findByRole('alert');
    expect(screen.getByRole('alert')).toHaveTextContent('Recovery unavailable');
    expect(screen.queryByRole('button', { name: 'Save decision' })).not.toBeInTheDocument();
  });

  it('shows a load failure instead of claiming the queue is empty', async () => {
    installFetchMock([{ match: '/transcripts/claims', handle: () => jsonResponse({ message: 'Register unavailable' }, { status: 500 }) }]);
    render(ClaimsReviewPage);
    await screen.findByRole('alert');
    expect(screen.getByRole('alert')).toHaveTextContent('Register unavailable');
    expect(screen.queryByText('You’re all caught up')).not.toBeInTheDocument();
  });
  it('clears selected records and decisions immediately when the workspace changes', async () => {
    let reads = 0;
    installFetchMock([{ match: '/transcripts/claims', handle: () => {
      reads += 1;
      return reads === 1 ? jsonResponse(page()) : new Promise<Response>(() => {});
    }}]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm statement' }));
    scopeIdentityStore.observe('another-owner', 'another-workspace');
    await waitFor(() => expect(screen.queryByText('The exact words')).not.toBeInTheDocument());
    expect(screen.queryByRole('button', { name: 'Save decision' })).not.toBeInTheDocument();
    expect(screen.queryByText(claim.stated_text)).not.toBeInTheDocument();
  });

  it('records a possible commitment separately and binds confirmation to the displayed term', async () => {
    const writes: Array<{ url: string; body: any }> = [];
    const confirmed = { ...claim, status: 'confirmed', audience_ref: { kind: 'account', id: 'account-1' } };
    const term = { commitment_id: 'term-1', terms: claim.stated_text, status: 'unconfirmed', revision: 4 };
    installFetchMock([
      { method: 'POST', match: '/transcripts/', handle: ({ url, init }) => {
        const body = JSON.parse(String(init?.body)); writes.push({ url, body });
        const confirming = url.endsWith('/confirm');
        const at = '2026-09-27T10:01:00Z';
        const commitmentId = confirming ? term.commitment_id : 'new-term';
        return jsonResponse({
          commitment: { ...term, commitment_id: commitmentId, audience: confirmed.audience_ref,
            source_ref: claim.outward_act_ref, stated_at: claim.stated_at, direction: 'stated_by_us',
            status: confirming ? 'confirmed' : 'unconfirmed', revision: confirming ? 5 : 1,
            confirmed_by: confirming ? body.by : null, confirmed_at: confirming ? at : null },
          receipt: { receipt_id: `receipt-${writes.length}`, decision_id: body.decision_id, commitment_id: commitmentId,
            audience: confirmed.audience_ref, verb: confirming ? 'confirm' : 'record',
            expected_revision: confirming ? body.expected_revision : body.expected_claim_revision,
            resulting_revision: confirming ? 5 : 1, by: confirming ? body.by : null,
            request_fingerprint: 'fingerprint-1', recorded_at: at, disposition: 'applied' }
        });
      } },
      { match: '/transcripts/commitments?', handle: () => jsonResponse({ commitments: [term] }) },
      { match: '/transcripts/claims', handle: () => jsonResponse({ ...page(), claims: [confirmed] }) }
    ]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'Record a possible commitment' }));
    await fireEvent.input(screen.getByLabelText('Your name'), { target: { value: 'Alice' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Record unconfirmed commitment' }));
    await screen.findByRole('button', { name: 'Review commitment' });
    expect(writes).toHaveLength(1);
    expect(writes[0].body).toMatchObject({ expected_claim_revision: 3 });
    await fireEvent.click(screen.getByRole('button', { name: 'Review commitment' }));
    await fireEvent.click(screen.getByRole('button', { name: /^Confirm commitment$/ }));
    await waitFor(() => expect(writes).toHaveLength(2));
    expect(writes[1].body).toMatchObject({ audience_kind: 'account', audience_id: 'account-1', expected_revision: 4, by: 'Alice' });
    expect(writes[1].body.decision_id).not.toBe(writes[0].body.decision_id);
    await screen.findByText('Your confirmation is saved. Current commitment status: confirmed.');
  });

  it('keeps a commitment request unchanged when HTTP success has no matching receipt', async () => {
    const bodies: unknown[] = [];
    const confirmed = { ...claim, status: 'confirmed', audience_ref: { kind: 'account', id: 'account-1' } };
    installFetchMock([
      { method: 'POST', match: '/commitment', handle: ({ init }) => { bodies.push(JSON.parse(String(init?.body))); return jsonResponse({}); } },
      { match: '/transcripts/claims', handle: () => jsonResponse({ ...page(), claims: [confirmed] }) }
    ]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'Record a possible commitment' }));
    await fireEvent.input(screen.getByLabelText('Your name'), { target: { value: 'Alice' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Record unconfirmed commitment' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('incomplete response');
    expect(screen.queryByText(/Possible commitment recorded/)).not.toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Retry commitment decision' }));
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[1]).toEqual(bodies[0]);
  });

  it('ignores an older commitment read after a newer read has completed', async () => {
    let finishOld!: (response: Response) => void;
    let reads = 0;
    const confirmed = { ...claim, status: 'confirmed', audience_ref: { kind: 'account', id: 'account-1' } };
    installFetchMock([
      { match: '/transcripts/commitments?', handle: () => ++reads === 1
        ? new Promise<Response>((resolve) => { finishOld = resolve; })
        : jsonResponse({ commitments: [{ commitment_id: 'term-1', terms: 'Current term', status: 'confirmed', revision: 5, confirmed_by: 'Alice' }] }) },
      { match: '/transcripts/claims', handle: () => jsonResponse({ ...page(), claims: [confirmed] }) }
    ]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('button', { name: 'View commitments' }));
    await waitFor(() => expect(reads).toBe(1));
    await fireEvent.click(screen.getByRole('button', { name: 'View commitments' }));
    await screen.findByText('Confirmed by Alice');
    finishOld(jsonResponse({ commitments: [{ commitment_id: 'term-1', terms: 'Stale term', status: 'unconfirmed', revision: 4 }] }));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(screen.queryByText('Stale term')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Review commitment' })).not.toBeInTheDocument();
    expect(screen.getByText('Confirmed by Alice')).toBeInTheDocument();
  });

  it('retries an interrupted import unchanged and gives edited details a separate conversation key', async () => {
    const bodies: Array<Record<string, any>> = [];
    installFetchMock([
      { method: 'POST', match: '/transcripts/ingest', handle: ({ init }) => {
        bodies.push(JSON.parse(String(init?.body)));
        return jsonResponse({ error: 'Response lost' }, { status: 503 });
      } },
      { match: '/transcripts/claims', handle: () => jsonResponse(page()) }
    ]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('tab', { name: 'Add a conversation' }));
    await fireEvent.input(screen.getByLabelText('Who spoke for your side?'), { target: { value: 'Alice' } });
    await fireEvent.input(screen.getByLabelText('When was it said?'), { target: { value: '2026-09-27T10:00' } });
    await fireEvent.input(screen.getByLabelText(/Who heard it/), { target: { value: 'Bob' } });
    await fireEvent.input(screen.getByLabelText('Exact words'), { target: { value: 'Original statement' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add for review' }));
    await screen.findByRole('alert');
    expect(screen.getByLabelText('Exact words')).toBeDisabled();
    await fireEvent.click(screen.getByRole('button', { name: 'Retry conversation' }));
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[1]).toEqual(bodies[0]);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Start a separate import' })).toBeEnabled());
    await fireEvent.click(screen.getByRole('button', { name: 'Start a separate import' }));
    expect(screen.getByRole('status')).toHaveTextContent('previous attempt may already be in the review queue');
    await fireEvent.input(screen.getByLabelText('Exact words'), { target: { value: 'Corrected statement' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add for review' }));
    await waitFor(() => expect(bodies).toHaveLength(3));
    expect(bodies[2].transcript_key).not.toBe(bodies[0].transcript_key);
    expect(bodies[2].utterances[0].spoken_text).toBe('Corrected statement');
  });

  it('retains the conversation and retry key when HTTP success does not account for the imported statement', async () => {
    const bodies: any[] = [];
    installFetchMock([
      { method: 'POST', match: '/transcripts/ingest', handle: ({ init }) => {
        const body = JSON.parse(String(init?.body)); bodies.push(body);
        return jsonResponse({ outward_act_ref: 'act-1', utterances_seen: 1, claims: [], skipped: [] });
      } },
      { match: '/transcripts/claims', handle: () => jsonResponse(page()) }
    ]);
    render(ClaimsReviewPage);
    await screen.findByText('The exact words');
    await fireEvent.click(screen.getByRole('tab', { name: 'Add a conversation' }));
    await fireEvent.input(screen.getByLabelText('Who spoke for your side?'), { target: { value: 'Alice' } });
    await fireEvent.input(screen.getByLabelText('When was it said?'), { target: { value: '2026-09-27T10:00' } });
    await fireEvent.input(screen.getByLabelText(/Who heard it/), { target: { value: 'Bob' } });
    await fireEvent.input(screen.getByLabelText('Exact words'), { target: { value: 'Original statement' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add for review' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('incomplete response');
    expect(screen.getByLabelText('Exact words')).toHaveValue('Original statement');
    expect(screen.getByLabelText('Exact words')).toBeDisabled();
    await fireEvent.click(screen.getByRole('button', { name: 'Retry conversation' }));
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[1]).toEqual(bodies[0]);
  });

});
