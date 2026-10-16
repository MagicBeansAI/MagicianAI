import { terminal, type Run, type ProposedDraft } from './api';

export type RunGroup = 'review' | 'attention' | 'active' | 'complete' | 'stopped';
export function runGroup(run: Run): RunGroup {
  if (run.status === 'budget_exhausted' && run.budget_continuation?.available) return 'attention';
  if (run.status === 'waiting_owner') return 'attention';
  if (run.capture_handoff?.capture?.status === 'awaiting_review') return 'review';
  if (run.status === 'accepted') {
    if (run.handoff || run.owner_approved === false) return 'complete';
    return run.routing?.route === 'human_review' && run.owner_approved === null ? 'review' : 'active';
  }
  return terminal(run.status) ? 'stopped' : 'active';
}
export function runLabel(run: Run): string {
  const group = runGroup(run);
  if (group === 'attention' && run.status === 'budget_exhausted') return 'Review continuation';
  if (group === 'review') return run.capture_handoff?.capture?.status === 'awaiting_review' ? 'Review capture' : 'Review draft';
  if (group === 'attention') return 'Needs input';
  if (run.handoff) return run.handoff.kind === 'capture' ? 'Captured' : 'Completed';
  if (run.owner_approved === false) return 'Handoff declined';
  if (run.status === 'accepted') return 'Preparing handoff';
  return run.status.replaceAll('_', ' ');
}
export function matchesSearch(query: string, ...values: Array<string | undefined | null>): boolean {
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  const haystack = values.filter(Boolean).join(' ').toLocaleLowerCase();
  return terms.every(term => haystack.includes(term));
}
export function proposalCriteria(proposal: ProposedDraft['proposal']): Array<{id:string; instructions:string; required:boolean}> {
  return proposal.criteria.map((value, index) => {
    const criterion = value as {id?:string;must_pass?:boolean;specification?:{kind?:string;instructions?:string;assertions?:unknown[]}};
    return {id:criterion.id ?? `Criterion ${index + 1}`, required:criterion.must_pass === true,
      instructions:criterion.specification?.instructions ?? `${criterion.specification?.kind ?? 'Declared'} check — inspect the complete contract for its assertions.`};
  });
}

export interface SourceReference {
  id: string;
  title: string;
  origin: string;
  content: string;
}
// Metadata travels with the exact source content, rather than a mutable link.
export function referenceSources(references: SourceReference[]): Record<string, string> {
  return Object.fromEntries(references.map(ref => [ref.id, `Reference: ${ref.title}\nOrigin: ${ref.origin}\n\n${ref.content}`]));
}
