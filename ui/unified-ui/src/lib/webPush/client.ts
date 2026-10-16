/**
 * Page side of HITL web push: support state, enable/disable from Settings,
 * re-sync on app load, the bearer mirror for the service worker, and the
 * window half of "open this ask" when a notification is clicked.
 *
 * The service worker is registered only here, once the reader enables
 * browser notifications (`kit.serviceWorker.register` is off), so pages that
 * never opt in — and the native desktop webview, which has its own notch —
 * never get one. Docs: docs/components/unified-ui/web-push.md.
 */
import { getCurrentScopeBearerToken, scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { clearMirroredBearer, writeMirroredBearer } from './credentialMirror';
import { WEB_PUSH_OPEN_MESSAGE, safeWebPath, urlBase64ToUint8Array } from './protocol';
import {
	WEB_PUSH_CONFIG_PATH,
	WEB_PUSH_SUBSCRIPTION_PATH,
	browserLabel,
	subscriptionBody
} from './subscriptionBody';

export type WebPushState =
	| 'unsupported'
	| 'desktop-app'
	| 'server-disabled'
	| 'blocked'
	| 'off'
	| 'on';

export interface WebPushConfig {
	enabled: boolean;
	vapid_public_key: string | null;
}

const OPT_IN_KEY = 'magician:web-push:enabled';
const WORKER_URL = '/service-worker.js';

function isDesktopApp(): boolean {
	return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/** True when this browser can receive web push at all. */
export function webPushSupported(): boolean {
	if (typeof window === 'undefined' || typeof navigator === 'undefined') return false;
	if (window.isSecureContext === false) return false;
	return 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;
}

function optedIn(): boolean {
	try {
		return localStorage.getItem(OPT_IN_KEY) === '1';
	} catch {
		return false;
	}
}

function setOptedIn(value: boolean): void {
	try {
		if (value) localStorage.setItem(OPT_IN_KEY, '1');
		else localStorage.removeItem(OPT_IN_KEY);
	} catch {
		// The subscription itself still stands; re-sync just won't run.
	}
}

async function readError(response: Response, fallback: string): Promise<Error> {
	const text = await response.text().catch(() => '');
	try {
		const body = JSON.parse(text) as { message?: unknown; detail?: unknown; error?: unknown };
		for (const message of [body.message, body.detail, body.error]) {
			if (typeof message === 'string' && message.trim()) return new Error(message);
		}
	} catch {
		// Fall through to the status-derived message.
	}
	return new Error(`${fallback} (${response.status})`);
}

export async function fetchWebPushConfig(): Promise<WebPushConfig> {
	const response = await fetch(WEB_PUSH_CONFIG_PATH, { cache: 'no-store' });
	if (response.status === 404) return { enabled: false, vapid_public_key: null };
	if (!response.ok) throw await readError(response, 'Web push settings are unavailable');
	const body = (await response.json()) as Partial<WebPushConfig>;
	const key = typeof body.vapid_public_key === 'string' ? body.vapid_public_key.trim() : '';
	return { enabled: body.enabled === true && key.length > 0, vapid_public_key: key || null };
}

async function existingRegistration(): Promise<ServiceWorkerRegistration | undefined> {
	return navigator.serviceWorker.getRegistration('/').catch(() => undefined);
}

async function registerWorker(): Promise<ServiceWorkerRegistration> {
	await navigator.serviceWorker.register(WORKER_URL, {
		// Vite serves the dev worker as an ES module; the build emits a classic script.
		type: import.meta.env.DEV ? 'module' : 'classic',
		scope: '/'
	});
	return navigator.serviceWorker.ready;
}

function sameKey(current: ArrayBuffer | null | undefined, wanted: Uint8Array): boolean {
	if (!current) return false;
	const bytes = new Uint8Array(current);
	return bytes.length === wanted.length && bytes.every((byte, index) => byte === wanted[index]);
}

async function ensureSubscription(
	registration: ServiceWorkerRegistration,
	vapidPublicKey: string
): Promise<PushSubscription> {
	const key = urlBase64ToUint8Array(vapidPublicKey);
	const existing = await registration.pushManager.getSubscription();
	if (existing && sameKey(existing.options?.applicationServerKey, key)) return existing;
	// A rotated server key invalidates the old endpoint's encryption.
	if (existing) await existing.unsubscribe().catch(() => false);
	return registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
}

async function putSubscription(subscription: PushSubscription): Promise<void> {
	const body = subscriptionBody(subscription, browserLabel(navigator.userAgent));
	if (!body) throw new Error('The browser returned a push subscription without keys.');
	const response = await fetch(WEB_PUSH_SUBSCRIPTION_PATH, {
		method: 'PUT',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!response.ok) throw await readError(response, 'Could not register this browser');
}

async function mirrorCurrentBearer(): Promise<void> {
	const token = getCurrentScopeBearerToken();
	if (token) await writeMirroredBearer(token);
	else await clearMirroredBearer();
}

/** The Settings control's state for this browser. */
export async function readWebPushState(config: WebPushConfig | null): Promise<WebPushState> {
	if (isDesktopApp()) return 'desktop-app';
	if (!webPushSupported()) return 'unsupported';
	if (config && !config.enabled) return 'server-disabled';
	if (Notification.permission === 'denied') return 'blocked';
	if (Notification.permission !== 'granted' || !optedIn()) return 'off';
	const registration = await existingRegistration();
	const subscription = await registration?.pushManager.getSubscription().catch(() => null);
	return subscription ? 'on' : 'off';
}

/**
 * Turn browser notifications on. Call from the click handler: the permission
 * prompt must follow a user gesture, so it is the first thing awaited.
 */
export async function enableWebPush(config: WebPushConfig): Promise<WebPushState> {
	if (isDesktopApp()) return 'desktop-app';
	if (!webPushSupported()) return 'unsupported';
	if (!config.enabled || !config.vapid_public_key) return 'server-disabled';
	const permission =
		Notification.permission === 'granted' ? 'granted' : await Notification.requestPermission();
	if (permission === 'denied') return 'blocked';
	if (permission !== 'granted') return 'off';
	const registration = await registerWorker();
	const subscription = await ensureSubscription(registration, config.vapid_public_key);
	await putSubscription(subscription);
	setOptedIn(true);
	await mirrorCurrentBearer();
	return 'on';
}

/** Turn browser notifications off for this browser and forget its endpoint server-side. */
export async function disableWebPush(): Promise<WebPushState> {
	setOptedIn(false);
	await clearMirroredBearer();
	if (!webPushSupported()) return 'unsupported';
	const registration = await existingRegistration();
	const subscription = await registration?.pushManager.getSubscription().catch(() => null);
	if (subscription) {
		await fetch(WEB_PUSH_SUBSCRIPTION_PATH, {
			method: 'DELETE',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ endpoint: subscription.endpoint })
		}).catch(() => null);
		await subscription.unsubscribe().catch(() => false);
	}
	return Notification.permission === 'denied' ? 'blocked' : 'off';
}

/**
 * App load: when this browser opted in and permission still stands, make
 * sure the subscription exists and the server has the current endpoint (the
 * push service may rotate it). A revoked permission drops the opt-in.
 */
export async function resyncWebPush(): Promise<WebPushState> {
	if (isDesktopApp()) return 'desktop-app';
	if (!webPushSupported()) return 'unsupported';
	if (!optedIn()) return Notification.permission === 'denied' ? 'blocked' : 'off';
	if (Notification.permission !== 'granted') {
		setOptedIn(false);
		await clearMirroredBearer();
		return Notification.permission === 'denied' ? 'blocked' : 'off';
	}
	const config = await fetchWebPushConfig();
	if (!config.enabled || !config.vapid_public_key) return 'server-disabled';
	const registration = await registerWorker();
	const subscription = await ensureSubscription(registration, config.vapid_public_key);
	await putSubscription(subscription);
	await mirrorCurrentBearer();
	return 'on';
}

export interface WebPushClientOptions {
	/** Show `path` (an Attention deep link) in this window; true when handled. */
	openPath(path: string): boolean;
}

/**
 * Mount-time wiring for the app shell: answer the worker's "open this ask"
 * messages, keep the worker's bearer copy in step with sign-in/sign-out, and
 * re-sync the subscription. Returns a cleanup.
 */
export function startWebPushClient(options: WebPushClientOptions): () => void {
	if (isDesktopApp() || !webPushSupported()) return () => {};

	const onMessage = (event: MessageEvent) => {
		const data = event.data as { type?: unknown; path?: unknown } | null;
		if (!data || data.type !== WEB_PUSH_OPEN_MESSAGE) return;
		const raw = typeof data.path === 'string' ? data.path : '';
		const path = safeWebPath(raw, '');
		let handled = false;
		if (path === raw) {
			try {
				handled = options.openPath(path);
			} catch {
				handled = false;
			}
		}
		event.ports?.[0]?.postMessage({ ok: handled });
	};
	navigator.serviceWorker.addEventListener('message', onMessage);

	let lastToken: string | null = null;
	const stopMirror = scopeIdentityStore.subscribe(() => {
		const token = getCurrentScopeBearerToken();
		if (token === lastToken) return;
		lastToken = token;
		if (!token) void clearMirroredBearer();
		else if (optedIn()) void writeMirroredBearer(token);
	});

	void resyncWebPush().catch((error: unknown) => {
		console.warn('[web-push] subscription re-sync failed:', error);
	});

	return () => {
		navigator.serviceWorker.removeEventListener('message', onMessage);
		stopMirror();
	};
}
