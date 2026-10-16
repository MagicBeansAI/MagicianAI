import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, it, vi } from 'vitest';

const asset = (name: string) => readFileSync(resolve(process.cwd(), '../../magician_data_v3/system/claims_review/app/surfaces', name), 'utf8');

it('pages the app with the source cursor, renders full words, and refreshes selected detail', async () => {
  const priorPath = window.location.pathname;
  window.history.replaceState(null, '', '/apps/installations/claims/surfaces/review');
  document.body.innerHTML = new DOMParser().parseFromString(asset('review.html'), 'text/html').body.innerHTML;
  const bridge = window as typeof window & { __magicianSurfaceBridgeReply(reply: unknown): void };
  const requests: any[] = [];
  const exact = 'Every word matters.\n'.repeat(600) + 'THE END';
  let page = { page_id: 'current', after_claim_id: null as string | null, next_cursor: 'source-cursor' as string | null, claim_ids_json: '["claim-1"]' };
  let claim = { claim_id: 'claim-1', claim_text: exact, speaker_name: 'Envoy', status: 'pending', expected_revision: 1 };
  let transcriptId = 'old-detail';
  vi.spyOn(window, 'postMessage').mockImplementation(({ request }: any) => {
    requests.push(request);
    let result: any = {};
    if (request.method === 'query_data') {
      const rows = request.payload.entity === 'claim_sync_page' ? [page]
        : request.payload.entity === 'claim_summary' ? [claim]
        : [{ claim_id: claim.claim_id, transcript_id: transcriptId, claim_text: 'Stale detail words' }];
      result = { envelope: { value: rows.map((fields) => ({ fields })) } };
    }
    queueMicrotask(() => bridge.__magicianSurfaceBridgeReply({ request_id: request.request_id, result }));
  });
  try {
    new Function('window', asset('review.js'))(window);
    const node = (id: string) => document.getElementById(id) as HTMLButtonElement;
    await vi.waitFor(() => expect(node('claim-detail').textContent).toContain('old-detail'));
    expect(document.querySelector('.claim-copy')?.textContent).toBe(exact);
    node('next-claims').click();
    await vi.waitFor(() => expect(requests.some((r) => r.method === 'launch_action')).toBe(true));
    expect(requests.find((r) => r.method === 'launch_action').payload.input).toEqual({ limit: 20, status: 'all', after_claim_id: 'source-cursor' });
    page = { ...page, after_claim_id: 'source-cursor', next_cursor: null, claim_ids_json: '["claim-21"]' };
    claim = { ...claim, claim_id: 'claim-21', claim_text: 'Later page statement' };
    node('refresh').click();
    await vi.waitFor(() => expect(document.querySelector('.claim-copy')?.textContent).toBe('Later page statement'));
    const reads = requests.filter((r) => r.payload.entity === 'claim_summary');
    expect(reads.at(-1).payload.predicate.nodes[0]).toEqual({ kind: 'in', field: 'claim_id', values: ['claim-21'] });
    expect(node('next-claims').disabled).toBe(true);
    expect(node('first-claims').disabled).toBe(false);
    transcriptId = 'fresh-detail';
    node('refresh').click();
    await vi.waitFor(() => expect(node('claim-detail').textContent).toContain('fresh-detail'));
    expect(node('claim-detail').textContent).not.toContain('old-detail');
  } finally { vi.restoreAllMocks(); document.body.innerHTML = ''; window.history.replaceState(null, '', priorPath); }
});
