import { workReturnHref } from './routes';

export const WORK_SPECIALISTS = ['/llm', '/history', '/memory', '/budget', '/evals', '/apps', '/skills', '/runtime', '/events', '/observe'] as const;
export type WorkSpecialist = (typeof WORK_SPECIALISTS)[number];
const specialistChildren = new Set(['/budget/audit', '/budget/transactions', '/llm/queue', '/runtime/resources', '/observe/stats', '/skills/evolution']);
export function isWorkSpecialistPath(path: string): boolean {
 return WORK_SPECIALISTS.includes(path as WorkSpecialist) || specialistChildren.has(path) || /^\/budget\/tokens\/[^/]+$/.test(path) || /^\/apps\/[^/]+(?:\/.*)?$/.test(path);
}
/** Only send record filters to pages which actually consume those filters. */
export function workSpecialistHref(path: WorkSpecialist, origin: URL, context: {agent?: string; task?: string; execution?: string} = {}): string {
 const params = new URLSearchParams();
 if (['/llm', '/history', '/memory', '/events'].includes(path) && context.agent) params.set('agent_id', context.agent);
 if (['/llm', '/events'].includes(path) && context.task) params.set('task_id', context.task);
 if (path === '/events' && context.execution) params.set('execution_id', context.execution);
 const from = workReturnHref(`${origin.pathname}${origin.search}`) || workReturnHref(origin.searchParams.get('from'));
 if (from) params.set('from', from);
 return `${path}${params.size ? `?${params}` : ''}${path === '/llm' && (context.agent || context.task) ? '#usage' : ''}`;
}

/** Continue through a specialist's own subpages without dropping the Work item.
 * Explicit target context wins; callers leave history navigation alone. */
export function carryWorkSpecialistContext(origin: URL, target: URL): string | null {
 if (origin.origin !== target.origin || !isWorkSpecialistPath(origin.pathname) || !isWorkSpecialistPath(target.pathname) || target.searchParams.has('from')) return null;
 const from = workReturnHref(origin.searchParams.get('from'));
 if (!from) return null;
 const next = new URL(target);
 next.searchParams.set('from', from);
 return `${next.pathname}${next.search}${next.hash}`;
}
