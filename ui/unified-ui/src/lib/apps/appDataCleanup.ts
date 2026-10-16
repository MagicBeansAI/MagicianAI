import { get } from 'svelte/store';
import { scopeIdentityStore, scopedRequestHeaders, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';

export type AppCleanupStatus = 'preview' | 'running' | 'paused' | 'checkpointing' | 'completed' | 'cancelled';
export interface AppCleanupSelection { entity: string; timestamp_field: string; before: string }
export interface AppCleanupJob {
  job_ref: string; installation_id: string; selection: AppCleanupSelection; status: AppCleanupStatus;
  preview_digest: string; matching_records: number; matching_payload_bytes: number;
  deleted_records: number; kept_changed_records: number; kept_referenced_records: number; remaining_records: number;
  observed_at: string; expires_at: string; updated_at: string;
}
export interface AppCleanupOptions {
  installation_id: string;
  entities: { entity: string; timestamp_fields: string[] }[];
  latest_job: AppCleanupJob | null;
}
export interface AppCleanupClient {
  options(signal: AbortSignal): Promise<AppCleanupOptions>;
  preview(selection: AppCleanupSelection, signal: AbortSignal): Promise<AppCleanupJob>;
  control(job: AppCleanupJob, operation: 'confirm' | 'pause' | 'resume' | 'cancel', signal: AbortSignal): Promise<AppCleanupJob>;
  advance(job: AppCleanupJob, signal: AbortSignal): Promise<AppCleanupJob>;
}

/** The date picker represents midnight in the user's local timezone. */
export function cleanupCutoff(date: string): string {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) throw new Error('Choose a valid cutoff date.');
  const value = new Date(`${date}T00:00:00`);
  const [year, month, day] = date.split('-').map(Number);
  if (!Number.isFinite(value.getTime()) || value.getFullYear() !== year || value.getMonth() !== month - 1 || value.getDate() !== day) {
    throw new Error('Choose a valid cutoff date.');
  }
  return value.toISOString();
}

export function parseAppCleanupJob(value: unknown, installationId: string): AppCleanupJob {
  const job = value as AppCleanupJob;
  const counts = ['matching_records','matching_payload_bytes','deleted_records','kept_changed_records','kept_referenced_records','remaining_records'] as const;
  if (!job || job.installation_id !== installationId || typeof job.job_ref !== 'string' || !job.job_ref.startsWith('app-cleanup:')
    || !/^blake3:[a-f0-9]{64}$/.test(job.preview_digest)
    || !['preview','running','paused','checkpointing','completed','cancelled'].includes(job.status)
    || counts.some(key => !Number.isSafeInteger(job[key]) || job[key] < 0)
    || !job.selection || typeof job.selection.entity !== 'string' || typeof job.selection.timestamp_field !== 'string'
    || ![job.selection.before,job.observed_at,job.expires_at,job.updated_at].every(date => typeof date === 'string' && Number.isFinite(Date.parse(date)))
    || job.deleted_records + job.kept_changed_records + job.kept_referenced_records + job.remaining_records !== job.matching_records
    || (job.status === 'completed' && job.remaining_records !== 0)) {
    throw new Error('The cleanup response did not match this app or its progress.');
  }
  return job;
}

/** Owner maintenance transport. It is not added to the sandboxed app SDK. */
export function createAppCleanupClient(installationId: string): AppCleanupClient {
  const identity = () => { const scope = get(scopeIdentityStore); return JSON.stringify([scope.principal,scope.workspace]); };
  const scope = identity(); const revision = getCurrentScopeCredentialRevision();
  const headers = scopedRequestHeaders();
  const current = () => identity() === scope && getCurrentScopeCredentialRevision() === revision;
  const root = `/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/data-cleanup`;
  const request = async (path: string, signal: AbortSignal, body?: unknown): Promise<unknown> => {
    if (!current()) throw new Error('The workspace or session changed. Reopen data cleanup.');
    const requestHeaders = new Headers(headers);
    if (body !== undefined) requestHeaders.set('Content-Type','application/json');
    const response = await fetch(root + path, { method: body === undefined ? 'GET' : 'POST', headers: requestHeaders, signal,
      ...(body === undefined ? {} : {body: JSON.stringify(body)}) });
    if (!current()) { await response.body?.cancel(); throw new Error('The workspace or session changed.'); }
    const value = await response.json();
    if (!current() || signal.aborted) throw new Error('The workspace or session changed. Reopen data cleanup.');
    if (!response.ok) throw new Error(value.message ?? value.error ?? `Cleanup request failed (${response.status}).`);
    return value;
  };
  const parse = (value: unknown) => parseAppCleanupJob(value,installationId);
  return {
    async options(signal) {
      const value = await request('',signal) as AppCleanupOptions;
      if (value.installation_id !== installationId || !Array.isArray(value.entities)
        || value.entities.some(entry => typeof entry.entity !== 'string' || !Array.isArray(entry.timestamp_fields)
          || entry.timestamp_fields.some(field => typeof field !== 'string'))) throw new Error('The app cleanup options could not be read.');
      return {...value, latest_job: value.latest_job ? parse(value.latest_job) : null};
    },
    async preview(selection,signal) { return parse(await request('/preview',signal,selection)); },
    async control(job,operation,signal) {
      parse(job);
      return parse(await request(`/${encodeURIComponent(job.job_ref)}/control`,signal,{ operation,
        ...(operation === 'confirm' ? {preview_digest:job.preview_digest,confirmation:'delete_older_app_data'} : {}) }));
    },
    async advance(job,signal) { parse(job); return parse(await request(`/${encodeURIComponent(job.job_ref)}/advance`,signal,{})); },
  };
}
