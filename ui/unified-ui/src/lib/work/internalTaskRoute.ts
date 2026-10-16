import type { InternalTaskSortField, InternalTaskSortOrder } from '$lib/internalTasks/api';

export interface InternalTaskRouteState {
 limit: number; offset: number; sort: InternalTaskSortField; order: InternalTaskSortOrder;
 agent: string; status: string; query: string; selected: string | null; execution: string | null;
}
const sorts = new Set(['updated_at', 'created_at', 'title', 'agent_id', 'status']);
export function readInternalTaskRoute(params: URLSearchParams): InternalTaskRouteState {
 const limit = Number(params.get('limit') || 50);
 const offset = Number(params.get('offset') || 0);
 return {
  limit: [25, 50, 100, 250].includes(limit) ? limit : 50,
  offset: Number.isSafeInteger(offset) && offset >= 0 ? offset : 0,
  sort: sorts.has(params.get('sort') || '') ? params.get('sort') as InternalTaskSortField : 'updated_at',
  order: params.get('order') === 'asc' ? 'asc' : 'desc',
  agent: (params.get('agent_id') || '').trim(), status: (params.get('status') || '').trim(),
  query: (params.get('query') || '').trim(), selected: params.get('selected')?.trim() || null,
  execution: (params.get('execution_id') || params.get('execution'))?.trim() || null
 };
}
export function internalTaskWorkHref(state: InternalTaskRouteState, context = new URLSearchParams()): string {
 const params = new URLSearchParams(context);
 params.set('type', 'internal'); params.delete('view');
 const values: Record<string, string | number | null> = {
  limit: state.limit === 50 ? null : state.limit, offset: state.offset || null,
  sort: state.sort === 'updated_at' ? null : state.sort, order: state.order === 'desc' ? null : state.order,
  agent_id: state.agent || null, status: state.status || null, query: state.query || null,
  selected: state.selected, execution_id: state.selected ? state.execution : null
 };
 for (const [key, value] of Object.entries(values)) {
  if (value === null) params.delete(key); else params.set(key, String(value));
 }
 return `/work?${params}`;
}
