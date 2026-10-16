import { scopedRequestHeaders, hasHydratedScopeBearer, getCurrentScopeCredentialRevision, getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';
export interface Stats { counts: Record<string, number>; cost_usd: number; input_tokens: number; output_tokens: number; cache_read_tokens: number; cache_write_tokens: number; model_latency: number[]; workflow_latency: number[] }
export interface DailyRow { day: number; kind: string; cohort: string; operation: string; model: string; identity: Record<string, unknown>; stats: Stats }
export interface Dashboard { rows: DailyRow[]; truncated: boolean; collected_since_ms: number | null; generated_at_ms: number; last_comparison_ms: number | null; capture_dropped: number; write_errors: number; read_ms: number }
export interface Answer { choice?: string; options?: string[]; noul?: number; score?: number }
export interface ReviewCase { id: string; at_ms: number; operation: string; primary: {required_questions: string[]; items: {item_id: string; response: {answers: Record<string,Answer>; model: {model:string}} | null}[]} | null; reference: {labels:Record<string,Record<string,unknown>>} | null; projection: Record<string,Record<string,string>> | null; reviews: Record<string,Record<string,unknown>>; review_revision: number; reference_status: string | null }
export async function metricsRequest<T>(path: string, signal?: AbortSignal, body?: unknown): Promise<T> {
 if (!(await hasHydratedScopeBearer())) throw new Error('Sign in as the owner to view decision evidence.');
 const revision=getCurrentScopeCredentialRevision(), scope=getCurrentScopeIdentity();
 const response = await fetch(`/api/magician/v2/plane/decision-metrics${path}`, {signal,method: body ? 'POST':'GET',headers: scopedRequestHeaders({'Accept':'application/json',...(body?{'Content-Type':'application/json'}:{})}),...(body?{body:JSON.stringify(body)}:{})});
 const value=await response.json().catch(()=>null);
 if(getCurrentScopeCredentialRevision()!==revision || getCurrentScopeIdentity().principal!==scope.principal || getCurrentScopeIdentity().workspace!==scope.workspace) throw new DOMException('Scope changed','AbortError');
 if(!response.ok) throw new Error(value?.message ?? 'Decision metrics are unavailable.');
 return value as T;
}
export function emptyStats(): Stats { return {counts:{},cost_usd:0,input_tokens:0,output_tokens:0,cache_read_tokens:0,cache_write_tokens:0,model_latency:[],workflow_latency:[]}; }
export function mergeStats(rows:DailyRow[]):Stats {
 const total=emptyStats();
 for(const {stats:s} of rows) addStats(total,s);
 return total;
}
function addStats(total:Stats,s:Stats):void {
 for(const [k,n] of Object.entries(s.counts))total.counts[k]=(total.counts[k]??0)+n;
 for(const k of ['cost_usd','input_tokens','output_tokens','cache_read_tokens','cache_write_tokens'] as const)total[k]+=s[k];
 for(const k of ['model_latency','workflow_latency'] as const)s[k].forEach((n,i)=>{total[k][i]=(total[k][i]??0)+n;});
}
/** One pass: a model table must not rescan every row for every cohort. */
export function groupStats(rows:DailyRow[]):Array<{row:DailyRow;stats:Stats}> {
 const groups=new Map<string,{row:DailyRow;stats:Stats}>();
 for(const row of rows){let group=groups.get(row.cohort);if(!group){group={row,stats:emptyStats()};groups.set(row.cohort,group);}addStats(group.stats,row.stats);}
 return [...groups.values()];
}
/** Bucket upper bounds; do not claim exact sample percentiles. */
export function percentile(hist:number[],p:number):number|null{
 const n=hist.reduce((sum,v)=>sum+Math.max(0,v),0);if(!n)return null;let seen=0;
 for(let i=0;i<hist.length;i++){seen+=Math.max(0,hist[i]);if(seen>=Math.ceil(n*p))return Math.ceil(1.12**(i+1));}return null;
}
export function rate(correct:number,n:number):string{
 if(!n)return 'No evidence';const p=correct/n,z=1.96,d=1+z*z/n,c=(p+z*z/(2*n))/d,r=z*Math.sqrt(p*(1-p)/n+z*z/(4*n*n))/d;
 return `${(p*100).toFixed(1)}% · ${correct}/${n} · 95% CI ${(100*(c-r)).toFixed(1)}–${(100*(c+r)).toFixed(1)}%`;
}
