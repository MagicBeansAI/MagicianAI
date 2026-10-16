import { workReturnHref } from './routes';
/** The App runner owns this manifest marker; a title or digest cannot identify
 * the installation. Keep a workspace-directory fallback when it is absent. */
export function appTaskHref(task: {id: string; ui_thread_id?: string | null; created_by?: string | null}, origin: URL, details?: {task: Record<string,unknown>}): string {
 const manifest = details?.task.manifest;
 const record = manifest && typeof manifest === 'object' && !Array.isArray(manifest) ? manifest as Record<string,unknown> : {};
 const thread = typeof record.ui_thread_id === 'string' ? record.ui_thread_id : task.ui_thread_id;
 const owner = typeof record.created_by === 'string' ? record.created_by : task.created_by;
 const installation = thread?.startsWith('app:') ? thread.slice(4) : '';
 const path = /^task_app_[0-9a-f]{64}$/.test(task.id) && owner === 'app_action' && /^[A-Za-z0-9_.-]{1,128}$/.test(installation) ? `/apps/${encodeURIComponent(installation)}` : '/apps';
 const from = workReturnHref(`${origin.pathname}${origin.search}`);
 return `${path}${from ? `?${new URLSearchParams({from})}` : ''}`;
}
