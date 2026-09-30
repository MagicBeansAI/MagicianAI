import { get } from 'svelte/store';
import { scopeIdentityStore, scopedRequestHeaders, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
import { v2Events, getV2EventSequence } from '$lib/realtime/v2-websocket';
import { parseAppEntityChangeEvent } from './appSurfaceRuntime';
import { MagicianAppsClient } from '../../../../../sdk/typescript/src/client';
export { AppLiveCollection, compareAppTimestamps } from '../../../../../sdk/typescript/src/live-collection';
export type { AppLiveCollectionState, AppLiveCollectionOptions } from '../../../../../sdk/typescript/src/live-collection';
import type { AppLiveCollection } from '../../../../../sdk/typescript/src/live-collection';

/** Freeze transport authority for the lifetime of one app/scope collection. */
export function scopedAppsClient(): MagicianAppsClient {
  const identity = () => { const value = get(scopeIdentityStore); return JSON.stringify([value.principal, value.workspace]); };
  const scope = identity();
  const credentialRevision = getCurrentScopeCredentialRevision();
  const current = () => scope === identity() && credentialRevision === getCurrentScopeCredentialRevision();
  const headers = scopedRequestHeaders();
  return new MagicianAppsClient({ origin: window.location.origin, fetch: async (input, init) => {
    if (!current()) throw new Error('The workspace or session changed.');
    const merged = new Headers(init?.headers);
    headers.forEach((value, name) => merged.set(name, value));
    const response = await fetch(input, { ...init, headers: merged });
    if (!current()) {
      await response.body?.cancel();
      throw new Error('The workspace or session changed.');
    }
    return response;
  } });
}

export async function fetchAppCollectionBinding(installationId: string, signal: AbortSignal): Promise<number> {
  const response = await fetch(`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}`, {
    headers: scopedRequestHeaders(), signal,
  });
  if (!response.ok) throw new Error(`The app installation could not be loaded (${response.status}).`);
  const value = await response.json();
  if (value.installation_id !== installationId || value.lifecycle?.status !== 'enabled'
    || !Number.isSafeInteger(value.active_surface_revision) || value.active_surface_revision < 1) {
    throw new Error('An enabled app with an active surface is required.');
  }
  return value.active_surface_revision;
}

/** Shared visible-page live driver. Websocket notifications wake durable catch-up;
 * reconnect/visibility and a slow fallback poll also recover dropped notifications. */
export function watchAppLiveCollection(collection: AppLiveCollection, installationId: string,
  options: { reopen?: () => void } = {}): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let lastEvent = 0;
  let failures = 0;
  let busy = false;
  let requested = false;
  const schedule = (delay = 50) => {
    if (stopped || document.hidden) return;
    if (busy) { requested = true; return; }
    clearTimeout(timer);
    timer = setTimeout(tick, delay);
  };
  const tick = async () => {
    if (stopped || document.hidden || busy) return;
    busy = true;
    requested = false;
    try { await collection.synchronize(); failures = 0; }
    catch (cause) {
      if ((cause as { reasonCode?: string })?.reasonCode === 'app_surface_stale' && options.reopen) {
        stopped = true;
        options.reopen();
      } else failures = Math.min(failures + 1, 5);
    }
    finally {
      busy = false;
      schedule(failures ? Math.min(1000 * 2 ** failures, 30000)
        : requested || collection.state.moreChanges ? 50 : 15000);
    }
  };
  const offEvents = v2Events.subscribe((events) => {
    let wake = false;
    for (const event of events) {
      const sequence = getV2EventSequence(event);
      if (sequence <= lastEvent) continue;
      if (lastEvent && sequence > lastEvent + 1) wake = true;
      lastEvent = sequence;
      if (parseAppEntityChangeEvent(event, installationId)) wake = true;
    }
    if (wake) schedule();
  });
  const offConnection = v2Events.connectionStatus.subscribe((status) => {
    if (status === 'connected') schedule();
  });
  if (v2Events.getConnectionState() === 'CLOSED') v2Events.connectGlobal();
  const visibility = () => { if (document.hidden) clearTimeout(timer); else schedule(); };
  document.addEventListener('visibilitychange', visibility);
  schedule();
  return () => {
    stopped = true;
    clearTimeout(timer);
    offEvents(); offConnection();
    document.removeEventListener('visibilitychange', visibility);
    collection.dispose();
  };
}
