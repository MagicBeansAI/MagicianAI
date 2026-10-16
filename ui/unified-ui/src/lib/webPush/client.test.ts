import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { writable } from 'svelte/store';

const { bearer, scopeStore, mirror } = vi.hoisted(() => ({
	bearer: { value: 'tok' },
	scopeStore: { current: null as ReturnType<typeof writable<Record<string, unknown>>> | null },
	mirror: { write: vi.fn(async (_token: string) => {}), clear: vi.fn(async () => {}) }
}));
vi.mock('$lib/stores/scopeIdentityStore', async () => {
	const { writable: makeWritable } = await import('svelte/store');
	scopeStore.current = makeWritable<Record<string, unknown>>({ principal: 'owner' });
	return {
		getCurrentScopeBearerToken: () => bearer.value,
		scopeIdentityStore: { subscribe: scopeStore.current.subscribe }
	};
});

vi.mock('./credentialMirror', () => ({
	writeMirroredBearer: (token: string) => mirror.write(token),
	clearMirroredBearer: () => mirror.clear()
}));

import {
	disableWebPush,
	enableWebPush,
	readWebPushState,
	resyncWebPush,
	startWebPushClient
} from './client';
import { WEB_PUSH_OPEN_MESSAGE } from './protocol';

// base64url of bytes [1, 2, 3]
const VAPID = 'AQID';
const CONFIG = { enabled: true, vapid_public_key: VAPID };

function fakeSubscription(endpoint: string, key: number[] = [1, 2, 3]) {
	return {
		endpoint,
		options: { applicationServerKey: new Uint8Array(key).buffer },
		toJSON: () => ({ endpoint, keys: { p256dh: 'p256', auth: 'auth' } }),
		unsubscribe: vi.fn(async () => true)
	};
}

function install(permission: NotificationPermission, requestResult: NotificationPermission = 'granted') {
	let current: ReturnType<typeof fakeSubscription> | null = null;
	const pushManager = {
		getSubscription: vi.fn(async () => current),
		subscribe: vi.fn(async (options: { applicationServerKey: Uint8Array }) => {
			current = fakeSubscription('https://push.test/ep-1', Array.from(options.applicationServerKey));
			return current;
		})
	};
	const registration = { pushManager };
	let registered = false;
	const listeners = new Set<(event: MessageEvent) => void>();
	const serviceWorker = {
		register: vi.fn(async () => {
			registered = true;
			return registration;
		}),
		ready: Promise.resolve(registration),
		getRegistration: vi.fn(async () => (registered ? registration : undefined)),
		addEventListener: vi.fn((_type: string, listener: (event: MessageEvent) => void) =>
			listeners.add(listener)
		),
		removeEventListener: vi.fn((_type: string, listener: (event: MessageEvent) => void) =>
			listeners.delete(listener)
		)
	};
	const NotificationMock = {
		permission,
		requestPermission: vi.fn(async () => {
			NotificationMock.permission = requestResult;
			return requestResult;
		})
	};
	vi.stubGlobal('window', { isSecureContext: true, PushManager: function PushManager() {}, Notification: NotificationMock });
	vi.stubGlobal('Notification', NotificationMock);
	vi.stubGlobal('navigator', { serviceWorker, userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/130.0 Safari/537.36' });
	const fetchMock = vi.fn<typeof fetch>(async (input) => {
		if (String(input).endsWith('/web-push/config')) {
			return new Response(JSON.stringify(CONFIG), { status: 200 });
		}
		return new Response(JSON.stringify({ id: 'sub-1' }), { status: 200 });
	});
	vi.stubGlobal('fetch', fetchMock);
	return {
		pushManager,
		serviceWorker,
		NotificationMock,
		fetchMock,
		listeners,
		setSubscription: (value: ReturnType<typeof fakeSubscription> | null) => {
			current = value;
			registered = true;
		}
	};
}

beforeEach(() => {
	localStorage.clear();
	bearer.value = 'tok';
	mirror.write.mockClear();
	mirror.clear.mockClear();
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('enableWebPush', () => {
	it('asks permission, subscribes with the VAPID key, PUTs the subscription and mirrors the bearer', async () => {
		const env = install('default');

		expect(await enableWebPush(CONFIG)).toBe('on');

		expect(env.NotificationMock.requestPermission).toHaveBeenCalledTimes(1);
		expect(env.serviceWorker.register).toHaveBeenCalledWith(
			'/service-worker.js',
			expect.objectContaining({ scope: '/' })
		);
		const subscribeOptions = env.pushManager.subscribe.mock.calls[0][0] as unknown as {
			userVisibleOnly: boolean;
			applicationServerKey: Uint8Array;
		};
		expect(subscribeOptions.userVisibleOnly).toBe(true);
		expect(Array.from(subscribeOptions.applicationServerKey)).toEqual([1, 2, 3]);
		const put = env.fetchMock.mock.calls.find(([, init]) => init?.method === 'PUT');
		expect(put?.[0]).toBe('/api/magician/v2/web-push/subscription');
		expect(JSON.parse(String(put?.[1]?.body))).toEqual({
			subscription: { endpoint: 'https://push.test/ep-1', keys: { p256dh: 'p256', auth: 'auth' } },
			label: 'Chrome on macOS'
		});
		expect(mirror.write).toHaveBeenCalledWith('tok');
		expect(await readWebPushState(CONFIG)).toBe('on');
	});

	it('reports blocked when permission is denied and subscribes nothing', async () => {
		const env = install('default', 'denied');
		expect(await enableWebPush(CONFIG)).toBe('blocked');
		expect(env.pushManager.subscribe).not.toHaveBeenCalled();
		expect(mirror.write).not.toHaveBeenCalled();
	});

	it('does nothing when the server has web push disabled', async () => {
		const env = install('default');
		expect(await enableWebPush({ enabled: false, vapid_public_key: null })).toBe('server-disabled');
		expect(env.NotificationMock.requestPermission).not.toHaveBeenCalled();
		expect(await readWebPushState({ enabled: false, vapid_public_key: null })).toBe('server-disabled');
	});

	it('fails loudly when the server rejects the subscription', async () => {
		const env = install('granted');
		env.fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ message: 'nope' }), { status: 500 }));
		await expect(enableWebPush(CONFIG)).rejects.toThrow('nope');
		expect(await readWebPushState(CONFIG)).toBe('off');
	});
});

describe('disableWebPush', () => {
	it('DELETEs the endpoint, unsubscribes and clears the mirrored bearer', async () => {
		const env = install('granted');
		await enableWebPush(CONFIG);
		const subscription = await env.pushManager.getSubscription();

		expect(await disableWebPush()).toBe('off');

		const del = env.fetchMock.mock.calls.find(([, init]) => init?.method === 'DELETE');
		expect(JSON.parse(String(del?.[1]?.body))).toEqual({ endpoint: 'https://push.test/ep-1' });
		expect(subscription?.unsubscribe).toHaveBeenCalled();
		expect(mirror.clear).toHaveBeenCalled();
		expect(await readWebPushState(CONFIG)).toBe('off');
	});
});

describe('resyncWebPush', () => {
	it('does nothing for a browser that never opted in', async () => {
		const env = install('granted');
		expect(await resyncWebPush()).toBe('off');
		expect(env.fetchMock).not.toHaveBeenCalled();
	});

	it('re-PUTs the current endpoint on load', async () => {
		const env = install('granted');
		await enableWebPush(CONFIG);
		env.fetchMock.mockClear();

		expect(await resyncWebPush()).toBe('on');
		expect(env.fetchMock.mock.calls.some(([, init]) => init?.method === 'PUT')).toBe(true);
		// Same key: the existing subscription is reused.
		expect(env.pushManager.subscribe).toHaveBeenCalledTimes(1);
	});

	it('resubscribes when the server VAPID key rotated', async () => {
		const env = install('granted');
		localStorage.setItem('magician:web-push:enabled', '1');
		const stale = fakeSubscription('https://push.test/old', [9, 9]);
		env.setSubscription(stale);

		expect(await resyncWebPush()).toBe('on');
		expect(stale.unsubscribe).toHaveBeenCalled();
		expect(env.pushManager.subscribe).toHaveBeenCalledTimes(1);
	});

	it('drops the opt-in when permission was revoked', async () => {
		install('denied');
		localStorage.setItem('magician:web-push:enabled', '1');
		expect(await resyncWebPush()).toBe('blocked');
		expect(localStorage.getItem('magician:web-push:enabled')).toBeNull();
		expect(mirror.clear).toHaveBeenCalled();
	});
});

describe('startWebPushClient', () => {
	it('opens a clicked ask in place and acknowledges the worker', async () => {
		const env = install('granted');
		const openPath = vi.fn(() => true);
		const stop = startWebPushClient({ openPath });
		const port = { postMessage: vi.fn() };
		for (const listener of env.listeners) {
			listener({
				data: { type: WEB_PUSH_OPEN_MESSAGE, path: '/attention?attention_item=c' },
				ports: [port]
			} as unknown as MessageEvent);
		}
		expect(openPath).toHaveBeenCalledWith('/attention?attention_item=c');
		expect(port.postMessage).toHaveBeenCalledWith({ ok: true });

		openPath.mockClear();
		for (const listener of env.listeners) {
			listener({
				data: { type: WEB_PUSH_OPEN_MESSAGE, path: '//evil.test/' },
				ports: [port]
			} as unknown as MessageEvent);
		}
		expect(openPath).not.toHaveBeenCalled();
		expect(port.postMessage).toHaveBeenLastCalledWith({ ok: false });
		stop();
		expect(env.listeners.size).toBe(0);
	});

	it('clears the mirrored bearer on sign-out and refreshes it on sign-in when opted in', async () => {
		install('granted');
		localStorage.setItem('magician:web-push:enabled', '1');
		const stop = startWebPushClient({ openPath: () => true });
		expect(mirror.write).toHaveBeenCalledWith('tok');

		bearer.value = '';
		scopeStore.current!.set({ principal: 'anonymous', workspace: 'default', isResolved: false });
		expect(mirror.clear).toHaveBeenCalled();

		bearer.value = 'tok-2';
		scopeStore.current!.set({ principal: 'owner', workspace: 'default', isResolved: true });
		expect(mirror.write).toHaveBeenLastCalledWith('tok-2');
		stop();
	});

	it('is inert in the desktop app', () => {
		const env = install('granted');
		vi.stubGlobal('window', { isSecureContext: true, PushManager: function PushManager() {}, Notification: env.NotificationMock, __TAURI_INTERNALS__: {} });
		startWebPushClient({ openPath: () => true })();
		expect(env.serviceWorker.addEventListener).not.toHaveBeenCalled();
	});
});
