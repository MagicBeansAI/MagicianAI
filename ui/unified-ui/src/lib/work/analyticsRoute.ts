export const ANALYTICS_GROUPS = ['operation', 'agent_id', 'model', 'task_id', 'chat_session_id', 'profile', 'provider'] as const;
export interface AnalyticsRouteState {
 range: number; agent: string; task: string; operation: string; model: string; chat: string;
 group: (typeof ANALYTICS_GROUPS)[number]; tab: 'cost' | 'usage' | 'health';
}
export function readAnalyticsRoute(url: URL): AnalyticsRouteState {
 const p = url.searchParams, range = Number(p.get('range') ?? 7), group = p.get('group_by');
 return {
  range: [0, 1, 7, 14, 30].includes(range) ? range : 7,
  agent: p.get('agent_id') || '', task: p.get('task_id') || '', operation: p.get('operation') || '', model: p.get('model') || '', chat: p.get('chat_session_id') || '',
  group: ANALYTICS_GROUPS.includes(group as AnalyticsRouteState['group']) ? group as AnalyticsRouteState['group'] : 'operation',
  tab: url.hash === '#usage' ? 'usage' : url.hash === '#health' ? 'health' : 'cost'
 };
}
export function analyticsHref(state: AnalyticsRouteState, origin: URL): string {
 const p = new URLSearchParams(origin.searchParams);
 const values: Record<string, string | number | null> = {range: state.range === 7 ? null : state.range, agent_id: state.agent || null, task_id: state.task || null, operation: state.operation || null, model: state.model || null, chat_session_id: state.chat || null, group_by: state.group === 'operation' ? null : state.group};
 for (const [key,value] of Object.entries(values)) { if(value === null) p.delete(key); else p.set(key,String(value)); }
 return `/llm${p.size ? `?${p}` : ''}${state.tab === 'cost' ? '' : `#${state.tab}`}`;
}
