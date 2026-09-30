/**
 * Realtime media session client — owns the lifecycle of a connection
 * to the backend `/api/magician/v2/media/sessions` registry.
 *
 * One singleton instance per browser tab. Construction is lazy — call
 * `ensureMediaSessionStarted()` from the app shell after scoped fetch
 * is installed, then reach for `mediaSessionStore` anywhere you need
 * the current session snapshot.
 *
 * The session sends a heartbeat every 60s, posts capability/permission
 * updates via PATCH, and emits whitelisted client lifecycle events via
 * POST `…/events`. The backend prunes silent sessions after ~3 minutes
 * so dropping the page on close (no beacon) is handled implicitly.
 */

import { browser } from '$app/environment';
import { get } from 'svelte/store';

import { detectBrowserCapabilities, detectSurfaceType } from './capabilities';
import { readBrowserPermissions } from './permissions';
import { mediaProvidersStore } from './providers';
import { mediaSessionStore } from './store';
import type {
	MediaEventType,
	MediaPermissions,
	RealtimeSession,
	SurfaceCapabilities,
	AudioStageOptions
} from './types';
import type { AudioSurface } from './audioSettings';

const HEARTBEAT_INTERVAL_MS = 60_000;
const BASE = '/api/magician/v2/media/sessions';

interface SessionEnvelope {
	session: RealtimeSession;
}

interface StartOptions {
	threadId?: string | null;
	displayLabel?: string;
	audioSurface?: AudioSurface;
	audioProfile?: string;
	audioStageOptions?: AudioStageOptions;
}

let heartbeatTimer: ReturnType<typeof setInterval> | null = null;
let pendingStart: Promise<RealtimeSession | null> | null = null;
let unloadListenerInstalled = false;

async function postJson<T>(url: string, body: unknown): Promise<T> {
	const response = await fetch(url, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!response.ok) {
		const text = await response.text().catch(() => '');
		throw new Error(`POST ${url} → ${response.status}: ${text}`);
	}
	return (await response.json()) as T;
}

async function patchJson<T>(url: string, body: unknown): Promise<T> {
	const response = await fetch(url, {
		method: 'PATCH',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!response.ok) {
		const text = await response.text().catch(() => '');
		throw new Error(`PATCH ${url} → ${response.status}: ${text}`);
	}
	return (await response.json()) as T;
}

async function registerSession(opts: StartOptions): Promise<RealtimeSession | null> {
	if (!browser) return null;
	const capabilities = detectBrowserCapabilities();
	const permissions = await readBrowserPermissions();
	const surface_type = detectSurfaceType();
	// Fetch the backend provider snapshot up front so the initial
	// session registration already advertises provider TTS / realtime
	// voice when those are configured. A second PATCH would leave a
	// brief window during which the UI thinks the only TTS path is
	// browser-native and routes accordingly.
	const providers = await mediaProvidersStore.refresh();
	if (providers.tts) capabilities.provider_tts = true;
	// Realtime voice ("live call") used to auto-promote whenever the
	// backend advertised the provider — which meant the tray's
	// `TrayLiveVoiceBanner` lit up on every new session even when the
	// user hadn't asked for a live call. Default it to OFF in the
	// desktop tray (Tauri); browser surfaces keep the old auto-enable
	// since live call is the primary affordance there. Tray users opt
	// in via the live-voice toggle in the voice settings, which PATCHes
	// the session's `capabilities.realtime_voice` to true.
	const isTauri =
		typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
	const userOptedIn =
		typeof window !== 'undefined'
		&& typeof window.localStorage !== 'undefined'
		&& window.localStorage.getItem('media.live_voice.auto_enable') === 'true';
	if (providers.realtime_voice && (!isTauri || userOptedIn)) {
		capabilities.realtime_voice = true;
	}
	try {
		const result = await postJson<SessionEnvelope>(BASE, {
			thread_id: opts.threadId ?? null,
			surface_type,
			transport: 'sse',
			capabilities,
			permissions,
			user_agent: typeof navigator !== 'undefined' ? navigator.userAgent : null,
			display_label: opts.displayLabel ?? null,
			audio_surface: opts.audioSurface ?? null,
			audio_profile: opts.audioProfile ?? null,
			audio_stage_options: opts.audioStageOptions ?? {}
		});
		mediaSessionStore.set(result.session);
		return result.session;
	} catch (error) {
		mediaSessionStore.setError(toErrorMessage(error));
		return null;
	}
}

function installUnloadListener(): void {
	if (!browser || unloadListenerInstalled) return;
	const handler = () => {
		const current = get(mediaSessionStore).session;
		if (!current) return;
		// `fetch` with `keepalive` keeps the request in flight after
		// the document unloads. We DELETE the session so the backend
		// stops listing it immediately rather than waiting for the
		// stale-prune sweep.
		try {
			const url = `${BASE}/${encodeURIComponent(current.session_id)}`;
			fetch(url, { method: 'DELETE', keepalive: true });
		} catch {
			// ignore — the backend will GC the session via heartbeat timeout
		}
	};
	window.addEventListener('pagehide', handler);
	window.addEventListener('beforeunload', handler);
	unloadListenerInstalled = true;
}

function clearHeartbeat(): void {
	if (heartbeatTimer !== null) {
		clearInterval(heartbeatTimer);
		heartbeatTimer = null;
	}
}

function scheduleHeartbeat(): void {
	clearHeartbeat();
	if (!browser) return;
	heartbeatTimer = setInterval(() => {
		const current = get(mediaSessionStore).session;
		if (!current) return;
		void postJson<SessionEnvelope>(
			`${BASE}/${encodeURIComponent(current.session_id)}/heartbeat`,
			{}
		)
			.then((envelope) => mediaSessionStore.set(envelope.session))
			.catch((error) => {
				// If heartbeat fails, the session may have been pruned. Try
				// to re-register on the next tick so a transient network
				// blip doesn't permanently silence the surface.
				mediaSessionStore.setError(toErrorMessage(error));
				void ensureMediaSessionStarted({ threadId: current.thread_id ?? null });
			});
	}, HEARTBEAT_INTERVAL_MS);
}

export async function ensureMediaSessionStarted(
	opts: StartOptions = {}
): Promise<RealtimeSession | null> {
	if (!browser) return null;
	const existing = get(mediaSessionStore).session;
	if (existing) {
		// If the thread changed, patch the existing session implicitly
		// via re-register. Keeps the session_id stable on the client
		// when possible, but a thread change is rare enough that we
		// just start fresh.
		if (opts.threadId && existing.thread_id !== opts.threadId) {
			await disconnectMediaSession();
		} else {
			return existing;
		}
	}
	if (pendingStart) return pendingStart;
	const startedPromise = registerSession(opts).then((session) => {
		// If `disconnectMediaSession()` ran while this register call
		// was in flight, `pendingStart` was nulled out under us — that
		// is a "user intent to stop" signal we must respect. Drop the
		// freshly-registered session on the floor (with a best-effort
		// DELETE so the backend doesn't carry a ghost record until the
		// 180s prune sweep) instead of re-populating the store.
		const cancelled = pendingStart !== startedPromise;
		pendingStart = null;
		if (cancelled && session) {
			void fetch(
				`${BASE}/${encodeURIComponent(session.session_id)}`,
				{ method: 'DELETE', keepalive: true }
			).catch(() => {});
			mediaSessionStore.clear();
			return null;
		}
		if (session) {
			scheduleHeartbeat();
			installUnloadListener();
		}
		return session;
	});
	pendingStart = startedPromise;
	return pendingStart;
}

export async function updateMediaSessionCapabilities(
	capabilities: SurfaceCapabilities
): Promise<void> {
	const current = get(mediaSessionStore).session;
	if (!current) return;
	try {
		const result = await patchJson<SessionEnvelope>(
			`${BASE}/${encodeURIComponent(current.session_id)}`,
			{ capabilities }
		);
		mediaSessionStore.set(result.session);
	} catch (error) {
		mediaSessionStore.setError(toErrorMessage(error));
	}
}

export async function updateMediaSessionPermissions(
	permissions: MediaPermissions
): Promise<void> {
	const current = get(mediaSessionStore).session;
	if (!current) return;
	try {
		const result = await patchJson<SessionEnvelope>(
			`${BASE}/${encodeURIComponent(current.session_id)}`,
			{ permissions }
		);
		mediaSessionStore.set(result.session);
	} catch (error) {
		mediaSessionStore.setError(toErrorMessage(error));
	}
}

export async function publishMediaEvent(
	eventType: MediaEventType,
	payload: Record<string, unknown> = {}
): Promise<void> {
	const current = get(mediaSessionStore).session;
	if (!current) return;
	try {
		await postJson<{ accepted: boolean }>(
			`${BASE}/${encodeURIComponent(current.session_id)}/events`,
			{
				event_type: eventType,
				payload
			}
		);
	} catch (error) {
		// Errors on telemetry events are silenced — they're best-effort
		// observability, never required for the user-facing flow.
		// eslint-disable-next-line no-console
		console.warn('[media-session] event post failed', eventType, error);
	}
}

export async function disconnectMediaSession(): Promise<void> {
	const current = get(mediaSessionStore).session;
	// Drop any in-flight registration so its `.then()` doesn't
	// re-schedule a heartbeat or re-populate the store after we've
	// torn down. Without this, a `disconnectMediaSession()` call
	// racing a slow `POST /sessions` quietly resurrects the session.
	pendingStart = null;
	clearHeartbeat();
	if (!current) return;
	try {
		await fetch(
			`${BASE}/${encodeURIComponent(current.session_id)}`,
			{ method: 'DELETE' }
		);
	} catch {
		// best effort
	}
	mediaSessionStore.clear();
}

function toErrorMessage(error: unknown): string {
	if (error instanceof Error) return error.message;
	if (typeof error === 'string') return error;
	try {
		return JSON.stringify(error);
	} catch {
		return 'unknown media session error';
	}
}
