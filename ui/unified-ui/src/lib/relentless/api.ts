import { timedFetch } from '$lib/shared/fetch';
import { getCurrentScopeCredentialRevision, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export interface Pagination {total:number;limit:number;offset:number;has_more:boolean}
export function pagePath(path:string,page:number,limit:number,filters:Record<string,string>={}):string {
  const query=new URLSearchParams({limit:String(limit),offset:String((page-1)*limit)});
  for(const [key,value] of Object.entries(filters))if(value.trim())query.set(key,value.trim());
  return `${path}?${query}`;
}
export interface RunSummary {
  id:string;goal:string;task_id:string;started_at:string;status:string;
  group:'review'|'attention'|'active'|'complete'|'stopped';label:string;
  attempt_count:number;max_attempts:number;spend:Run['spend'];
}
export interface ProposalSummary {goal:string;origin:ProposedDraft['origin'];reference_count:number;maximum_cost_microusd:number}
export interface CatalogPage {
  items:({kind:'run';item:RunSummary}|{kind:'proposal';item:ProposalSummary}|{kind:'unavailable';item:{id:string;reason:string}})[];
  pagination:Pagination;counts:Record<string,number>;agents:string[];
}
export interface Profile {
  name: string; model: string; family: string; maximum_request_microusd: number;
  supports_json?: boolean | null;
}
export interface ArtifactFiles { files: Record<string,string>; binary_files?: Record<string,{media_type:string;bytes:number;content_hash:string}>; }
export interface Score { criterion_id: string; outcome: string; score: number | null; confidence: number | null; }
export interface TaskContext {
  task_id: string; context_hash: string; title: string; goal: string;
  agent_id: string; ui_thread_id: string; can_start: boolean; reason: string | null;
  pending_run_id: string | null; pending_proposal_hash: string | null;
  policy?: {id:string;max_runs:number;expires_at:string;contract_hash:string;proposal:ProposedDraft['proposal'];capture_handoff?:CaptureHandoffTarget|null} | null;
  policy_runs?: number;
}

export interface CaptureHandoffTarget {campaign_id:string;version_hash:string;identity_id:string;}
export interface WorkView {
  goal_id:string|null;pagination:Pagination;
  targets:{task_id:string;context_hash:string;title:string;agent_id?:string;goal_id:string|null;due_date:string|null;task_status:string;
    policy:{id:string;max_runs:number;runs_accepted:number;expires_at:string;expired:boolean;context_changed:boolean;maximum_cost_microusd:number}|null;
    run_count:number; runs:{id:string;contract_hash:string;status:string;handoff:{kind:string}|null;spend:{settled_microusd:number;reserved_microusd:number}}[];
  }[];
}

export interface CampaignCapture {
  id:string;binding_hash:string;status:'awaiting_review'|'captured'|'rejected';reason:string;
  audit_required:boolean;
  binding:{run_id:string;artifact_hash:string;identity_id:string;recipient:string;task_id:string;artifact_files:Record<string,string>};
}
export interface CampaignView {
  capture_count:number;
  state:{campaign:{id:string;version_hash:string;revoked_at:string|null;spec:{name:string;targets:{task_id:string;identity_id:string}[];maximum_acts:number;expires_at:string;[key:string]:unknown}};
    captures:Record<string,CampaignCapture>;simulated_clean_approvals:number;simulated_clean_approvals_by_identity:Record<string,number>;production_clean_approvals:number;automatic_capture_suspended:boolean;live_dispatch_enabled:boolean};
  targets:{task_id:string;identity_id:string;title?:string;due_date?:string|null;context_changed?:boolean;unavailable?:string}[];
}
export interface CampaignCatalog {
  campaigns:CampaignView[];pagination:Pagination;
  identities:{id:string;recipient:string;counterparty:string;verified:boolean}[];
}
export interface GuidancePolicyView {
  policy:{id:string;version_hash:string;guidance:{title:string;workflow:string[];decision_points:string[];verification:string[];failure_modes:string[]};
    spec:{name:string;candidate_id:string;expires_at:string;maximum_adopted_runs:number;maximum_evaluation_cost_microusd:number;maximum_canary_cost_microusd:number;
      disable_after_regressions:number;unknown_monitor_disables_adoption:boolean;monitor_nonacceptance_is_regression:boolean;
      targets:{task_id:string;task_context_hash:string;baseline_contract_hash:string;minimum_quality:number|null}[];[key:string]:unknown};
    evaluation:{id:string;input_hash:string}[];canary:{id:string;input_hash:string}[]};
  status:string;reason:string|null;procedure_id:string|null;regressions:number;expired:boolean;adoption_enabled:boolean;
  evaluation_spend:{settled_microusd:number;reserved_microusd:number};canary_spend:{settled_microusd:number;reserved_microusd:number};
  reservations:Record<string,{run_id:string;phase:string;case_id:string;variant:string;contract_hash:string}>;
  observations:Record<string,{run_id:string;complete:boolean;feasible:boolean;quality:number|null;accepted:boolean;operator_redirects:number;actual_cost_microusd:number;evidence_hash:string}>;
  adoptions:Record<string,{run_id:string;guided_contract_hash:string}>;
  monitored:Record<string,{run_id:string;regressed:boolean;unknown:boolean;evidence_hash:string}>;
}
export interface GuidanceEvidenceView {evidence:Record<string,unknown>;content_hash:string;sealed:boolean}
export interface GuidanceProposalView {candidate_id:string;guidance:GuidancePolicyView['policy']['guidance'];training_run_ids:string[];agent_id:string|null;task_id:string|null;execution_id:string|null}
export interface Verdict { status: string; scores: Score[]; findings: { criterion_id: string; location: string; detail: string }[]; }
export interface Attempt { number: number; artifact: { hash: string; files: Record<string,string> } | null; checkpoint?: { attention_id: string } | null; failure?: string | null; verdicts: Record<string,Verdict>; review?: { criteria: {criterion_id:string;outcome:string;reasons:string[]}[] }; decided: boolean; }
export interface ProposedDraft {
  proposal: {goal:string;actor_profile:string;critic_profiles:string[];sources:Record<string,string>;criteria:unknown[];budget:{max_cost_microusd:number;max_attempts:number;max_wall_clock_secs:number;plateau_attempts:number};[key:string]:unknown};
  proposal_hash:string;
  origin:{request_id:string;agent_id:string;task_id:string;execution_id:string};
}
export interface Run {
  id: string; status: string; reason: string | null; attention_id: string | null;
  started_at: string; deadline_at: string; source_epoch: number;
  generation?:number; contract_hash?:string;
  effective_budget?:{max_attempts:number;max_cost_microusd:number;max_wall_clock_secs:number;plateau_attempts:number};
  exhausted_limits?:{cost:boolean;time:boolean;attempts:boolean};
  budget_continuation?:{available:boolean;reason:string|null};
  budget_usage?:{cost:{allowance_microusd:number;committed_microusd:number;remaining_microusd:number|null};attempts:{allowance:number;used:number;remaining:number|null};time:{remaining_secs:number|null};cumulative:{elapsed_wall_clock_secs:number}};
  budget_resets?:{id:string;approved_by:string;approved_at:string;reset:{cost_microusd?:number;wall_clock_secs?:number;attempts?:number};attempts_before:number;spend_before:Run['spend']}[];
  admission: { task_id: string };
  contract: { goal: string; budget: { max_attempts: number; max_cost_microusd: number }; criteria: { id: string; kind: string; must_pass: boolean }[]; };
  attempts: Attempt[]; decisions: { kind: string; reason: string; accepted_hash: string | null }[];
  retained: { champion: { artifact_hash: string; quality_score: number | null } | null; best_draft: { artifact_hash: string; quality_score: number | null } | null };
  spend: { settled_microusd: number; reserved_microusd: number };
  routing: {artifact_hash:string;route:string;reason:string} | null;
  owner_approved: boolean | null;
  handoff: {artifact_hash:string;kind:'internal_draft'|'capture'} | null;
  capture_handoff?:{target?:CaptureHandoffTarget;capture?:CampaignCapture|null;available:boolean;unavailable_reason?:string|null}|null;
}
export interface RunReportView {
  report:{run_id:string;status:string;generation:number;open_items:string[];
    output:{artifact_hash:string;attempt_number:number;locally_accepted:boolean;role:string}|null};
  content_hash:string;markdown:string;derived_from_journal:boolean;
  capture_handoff:Run['capture_handoff'];
}
export const terminal = (status: string) => ['accepted','plateau','budget_exhausted','cancelled','failed'].includes(status);
export const usd = (microusd: number) => (microusd / 1_000_000).toLocaleString(undefined, {style:'currency',currency:'USD',maximumFractionDigits:4});
/** Apply only to limits: a measured zero cost is still $0. */
export const costLimit = (microusd?: number | null) => !microusd ? 'Unlimited' : usd(microusd);
export const countLimit = (value?: number | null) => !value ? 'Unlimited' : String(value);


export async function request<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  const revision = getCurrentScopeCredentialRevision();
  const response = await timedFetch(`/api/magician/v2/relentless${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: scopedRequestHeaders({'Content-Type':'application/json'}),
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal
  });
  const data = await response.json().catch(() => {
    throw new Error(response.ok ? 'Server returned an invalid JSON response.' : `Request failed (${response.status}).`);
  });
  if (revision !== getCurrentScopeCredentialRevision()) throw new Error('Workspace changed; refresh this view.');
  if (!response.ok) throw new Error(data.error ?? data.message ?? `Request failed (${response.status})`);
  return data as T;
}

export async function requestArtifactFile(path: string, signal?: AbortSignal): Promise<Blob> {
  const revision = getCurrentScopeCredentialRevision();
  const response = await timedFetch(`/api/magician/v2/relentless${path}`, {
    method:'GET', headers:scopedRequestHeaders(), signal,
  });
  if (!response.ok) throw new Error(`Artifact download failed (${response.status}).`);
  const blob = await response.blob();
  if (revision !== getCurrentScopeCredentialRevision()) throw new Error('Workspace changed; refresh this view.');
  return blob;
}
