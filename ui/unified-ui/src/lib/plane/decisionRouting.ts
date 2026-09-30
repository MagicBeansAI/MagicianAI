import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
export interface ModelRoute { primary: string; backup: string | null }
export interface LocalityRoutes { local: ModelRoute; cloud: ModelRoute }
export interface RoutingModel { name: string; model: string; remote: boolean }
export interface OperationRouting {
 name: string; routing: LocalityRoutes | null;
 configured_local: string[]; configured_cloud: string[];
 active_local: string[]; active_cloud: string[];
 allow_remote_when_local: boolean;
 threshold_names: string[];
 thresholds_by_model: Record<string, Record<string, number>>;
}
export interface RoutingSettings {
 contract_version: number; revision: string; pending: boolean; enabled: boolean;
 models: RoutingModel[]; operations: OperationRouting[];
}
export interface RoutingUpdate {
 revision: string; operation: string; routing: LocalityRoutes;
 allow_remote_when_local: boolean;
 thresholds_by_model: Record<string, Record<string, number>>;
}
export async function decisionRouting(update?: RoutingUpdate): Promise<RoutingSettings> {
 const response = await fetch('/api/magician/v2/plane/decision-routing', {
  method: update ? 'PUT' : 'GET',
  headers: scopedRequestHeaders({ Accept: 'application/json', ...(update ? {'Content-Type': 'application/json'} : {}) }),
  ...(update ? {body: JSON.stringify(update)} : {})
 });
 const body = await response.json().catch(() => null);
 if (!response.ok) throw new Error(body?.message ?? 'Decision Engine routing is unavailable.');
 if (!body || typeof body.revision !== 'string' || !Array.isArray(body.models) || !Array.isArray(body.operations)) {
  throw new Error('The server did not confirm Decision Engine routing.');
 }
 return body as RoutingSettings;
}
