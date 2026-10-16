/** Work composes existing records; route state never substitutes for their identity. */
export const WORK_ROUTE = '/work';
export const WORK_VIEWS = ['all', 'needs-you', 'crew', 'programs', 'quality', 'operations'] as const;
export type WorkView = (typeof WORK_VIEWS)[number];

export function resolveWorkView(params: URLSearchParams): WorkView {
	const value = params.get('view');
	return WORK_VIEWS.includes(value as WorkView) ? value as WorkView : 'all';
}

export function workViewHref(view: WorkView): string {
	return view === 'all' ? WORK_ROUTE : `${WORK_ROUTE}?view=${view}`;
}

/** Quality reads its initial deep-link target on mount. A new target must
 * remount it, while pagination/filter changes in a task inspector keep state. */
export function workSelectionKey(params: URLSearchParams, kind: string): string {
	const view = resolveWorkView(params);
	return JSON.stringify([view, kind, params.get('agent') || '', view === 'all' && kind === 'tasks' ? params.get('thread') || '' : '',
		...(view === 'quality' ? ['task', 'run', 'proposal', 'campaign'].map(key => params.get(key) || '') : [])]);
}

/** Keep every legacy query value, including repeats, plan/run focus and monitor update. */
export function legacyTasksWorkHref(params: URLSearchParams): string {
	const next = new URLSearchParams(params);
	next.delete('view');
	return next.size ? `${WORK_ROUTE}?${next}` : WORK_ROUTE;
}

/** Old quality bookmarks keep all their targets and repeated query values. */
export function legacyRelentlessWorkHref(params: URLSearchParams): string {
	const next = new URLSearchParams(params);
	next.set('view', 'quality');
	return `${WORK_ROUTE}?${next}`;
}

export function workTaskHref(id: string, kind?: 'internal' | 'monitors'): string {
	const next = new URLSearchParams();
	if (kind) next.set('type', kind);
	next.set('selected', id);
	return `${WORK_ROUTE}?${next}`;
}

export function workKindHref(kind: 'tasks' | 'internal' | 'monitors'): string {
	return kind === 'tasks' ? WORK_ROUTE : `${WORK_ROUTE}?type=${kind}`;
}

/** Only these Work-owned pages may be used as a local return destination. */
export function isWorkPathname(pathname: string): boolean {
	return (pathname === WORK_ROUTE || pathname === '/work/') || pathname === '/work/crew/new'
		|| /^\/work\/crew\/[^/?#]+\/(?:edit|rules|memory\/[^/?#]+)$/.test(pathname);
}

export function workReturnHref(value: string | null, depth = 0): string | null {
	if (!value || value.length > 2000 || !value.startsWith('/work')) return null;
	try {
		const url = new URL(value, 'https://work.invalid');
		if (url.origin !== 'https://work.invalid' || !isWorkPathname(url.pathname)) return null;
		const returns = url.searchParams.getAll('from');
		if (returns.length > 1 || (returns.length && (depth >= 3 || !workReturnHref(returns[0], depth + 1)))) return null;
		return `${url.pathname}${url.search}`;
	} catch { return null; }
}

function withWorkReturn(href: string, origin?: URL): string {
	const from = origin ? workReturnHref(`${origin.pathname}${origin.search}`) : null;
	if (!from) return href;
	const url = new URL(href, 'https://work.invalid');
	url.searchParams.set('from', from);
	return `${url.pathname}${url.search}`;
}

export function workCrewEditorHref(agentId?: string, origin?: URL): string {
	return withWorkReturn(agentId ? `/work/crew/${encodeURIComponent(agentId)}/edit` : '/work/crew/new', origin);
}

export function workCrewRulesHref(agentId: string, origin?: URL): string {
	return withWorkReturn(`/work/crew/${encodeURIComponent(agentId)}/rules`, origin);
}

export function workMemoryTierHref(agentId: string, tier: string, goalId?: string, origin?: URL): string {
	const path = `/work/crew/${encodeURIComponent(agentId)}/memory/${encodeURIComponent(tier)}`;
	return withWorkReturn(goalId ? `${path}?${new URLSearchParams({ goal_id: goalId })}` : path, origin);
}

/** Compatibility only: preserve supported query state without mounting an old host. */
export function legacyCrewWorkHref(params: URLSearchParams, agentId?: string, surface?: 'rules' | 'memory', tier?: string): string {
	const next = new URLSearchParams(params);
	if (surface === 'rules' && agentId) return withQuery(workCrewRulesHref(agentId), next);
	if (surface === 'memory' && agentId && tier) return withQuery(workMemoryTierHref(agentId, tier), next);
	next.set('view', 'crew');
	if (agentId) next.set('agent', agentId);
	return `${WORK_ROUTE}?${next}`;
}

function withQuery(path: string, params: URLSearchParams): string {
	return params.size ? `${path}?${params}` : path;
}

export function legacyCrewEditorWorkHref(params: URLSearchParams): string {
	const next = new URLSearchParams(params);
	const edit = next.get('edit')?.trim();
	next.delete('edit');
	return withQuery(workCrewEditorHref(edit), next);
}

export function workQualityHref(
	target: { task?: string; run?: string; proposal?: string; campaign?: string },
	origin?: URL
): string {
	const params = new URLSearchParams({ view: 'quality' });
	for (const key of ['task', 'run', 'proposal', 'campaign'] as const) {
		if (target[key]) params.set(key, target[key]);
	}
	// Moving between quality records keeps the original work item's return path.
	const from = origin ? workReturnHref(origin.pathname === WORK_ROUTE && resolveWorkView(origin.searchParams) === 'quality'
		? origin.searchParams.get('from') : `${origin.pathname}${origin.search}`) : null;
	if (from) params.set('from', from);
	return `${WORK_ROUTE}?${params}`;
}

export function taskQualityHref(taskId: string, origin: URL): string {
	return workQualityHref({ task: taskId }, origin);
}

export function workAgentHref(agentId: string, origin?: URL): string {
	const params = new URLSearchParams({ view: 'crew', agent: agentId });
	const from = origin ? workReturnHref(`${origin.pathname}${origin.search}`) : null;
	if (from) params.set('from', from);
	return `${WORK_ROUTE}?${params}`;
}

/** A visible thread scope, never a different task identity or storage root. */
export function workThreadHref(threadId: string, params = new URLSearchParams()): string {
 const next = new URLSearchParams(params);
 next.delete('view'); next.delete('type');
 next.set('thread', threadId.trim().toLowerCase() || 'general');
 return `${WORK_ROUTE}?${next}`;
}

/** Read old routes stored in feed cards or messages without restoring retired pages. */
export function canonicalWorkHref(href: string): string {
 if (!href.startsWith('/') || href.startsWith('//')) return href;
 try {
  const url = new URL(href, 'https://work.invalid');
  if (url.origin !== 'https://work.invalid') return href;
  const path = url.pathname.replace(/\/+$/, '');
  let target: string;
  if (path === '/tasks') target = legacyTasksWorkHref(url.searchParams);
  else if (path === '/relentless') target = legacyRelentlessWorkHref(url.searchParams);
  else if (path === '/crew') target = legacyCrewWorkHref(url.searchParams);
  else if (path === '/crew/new') target = legacyCrewEditorWorkHref(url.searchParams);
  else if (path === '/internal-tasks' || path === '/debug/tasks' || path === '/harness') {
   const params = new URLSearchParams(url.searchParams);
   params.delete('view');
   if (path === '/harness') params.set('view', 'operations');
   else params.set('type', 'internal');
   target = `${WORK_ROUTE}?${params}`;
  } else {
   const thread = path.match(/^\/t\/([^/]+)\/tasks$/);
   const member = path.match(/^\/crew\/([^/]+)(?:\/(rules|memory\/([^/]+)))?$/);
   if (thread) target = workThreadHref(decodeURIComponent(thread[1]), url.searchParams);
   else if (member) target = legacyCrewWorkHref(url.searchParams, decodeURIComponent(member[1]),
    member[2] === 'rules' ? 'rules' : member[3] ? 'memory' : undefined,
    member[3] ? decodeURIComponent(member[3]) : undefined);
   else return href;
  }
  return target + url.hash;
 } catch { return href; }
}

/** Today may need to resolve an untyped legacy task to its internal kind.
 * Keep the original execution/plan/filter state during that resolution. */
export function resolvedWorkTaskHref(taskId: string, resolved: string, source?: string): string {
 const destination = new URL(resolved, 'https://work.invalid');
 if (source) {
  const original = new URL(source, 'https://work.invalid');
  if (original.origin === destination.origin &&
   (original.pathname === '/tasks' || ((original.pathname === '/work' || original.pathname === '/work/') && resolveWorkView(original.searchParams) === 'all'))) {
   const kind = destination.searchParams.get('type');
   destination.search = original.search;
   destination.searchParams.delete('view'); destination.searchParams.delete('type');
   if (kind) destination.searchParams.set('type', kind);
  }
 }
 destination.searchParams.set('selected', taskId);
 return `${destination.pathname}${destination.search}`;
}
export function isTaskSelectionHref(href: string): boolean {
 try {
  const url = new URL(href, 'https://work.invalid');
  return url.origin === 'https://work.invalid' && ((url.pathname === '/tasks' && url.searchParams.get('type') !== 'monitors') ||
   ((url.pathname === '/work' || url.pathname === '/work/') && resolveWorkView(url.searchParams) === 'all' &&
    !url.searchParams.has('view') && url.searchParams.has('selected') && url.searchParams.get('type') !== 'monitors'));
 } catch { return false; }
}
