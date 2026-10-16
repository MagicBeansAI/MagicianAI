import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
import type { NoteSearchResults, NoteSearchHit } from '$lib/notes/noteSearch';
import type { SourceReference } from './workspace';

async function notesRequest<T>(path:string, signal:AbortSignal, body?:unknown):Promise<T> {
  const revision = getCurrentScopeCredentialRevision();
  const response = await timedFetch(`/api/magician/v2/notes${path}`, {
    method:body === undefined ? 'GET' : 'POST', signal,
    headers:scopedRequestHeaders({'Content-Type':'application/json'}),
    ...(body === undefined ? {} : {body:JSON.stringify(body)})
  });
  const data = await response.json();
  if (revision !== getCurrentScopeCredentialRevision()) throw new Error('Workspace changed; search again.');
  if (!response.ok) throw new Error(data.message ?? data.error ?? `Reference request failed (${response.status}).`);
  return data as T;
}
export async function findReferences(query:string, signal:AbortSignal):Promise<NoteSearchResults> {
  // /notes/file reads the configured primary root. Search that same provider so
  // identical relative paths from another provider cannot resolve to wrong bytes.
  const root = await notesRequest<{provider:string}>('/tree', signal);
  return notesRequest('/search', signal, {query, provider:root.provider, limit:12});
}
export async function readReference(hit:NoteSearchHit, signal:AbortSignal):Promise<SourceReference> {
  const note = await notesRequest<{provider:string;relative_path:string;title:string;markdown:string}>(`/file?path=${encodeURIComponent(hit.relative_path)}`, signal);
  if (note.provider !== hit.provider || note.relative_path !== hit.relative_path) throw new Error('The notes provider changed. Search and attach this reference again.');
  return {id:hit.source_ref, title:note.title, origin:hit.source_ref, content:note.markdown};
}
