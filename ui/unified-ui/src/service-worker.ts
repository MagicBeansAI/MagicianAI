/// <reference types="@sveltejs/kit" />
/// <reference no-default-lib="true"/>
/// <reference lib="esnext" />
/// <reference lib="webworker" />

/**
 * Magician service worker — HITL web push only.
 *
 * Registered by `$lib/webPush/client` when the reader turns on browser
 * notifications in Settings (`kit.serviceWorker.register` is off). It has no
 * `fetch` handler and caches nothing, so page loads and API calls behave
 * exactly as they do without it. Handlers live in
 * `$lib/webPush/workerHandlers`. Docs: docs/components/unified-ui/web-push.md.
 */
import { readMirroredBearer } from '$lib/webPush/credentialMirror';
import { browserLabel } from '$lib/webPush/subscriptionBody';
import {
	handleNotificationClick,
	handlePush,
	handleSubscriptionChange,
	type WorkerClients,
	type WorkerDeps,
	type WorkerPushSubscription,
	type WorkerRegistration
} from '$lib/webPush/workerHandlers';

const sw = self as unknown as ServiceWorkerGlobalScope;

function deps(): WorkerDeps {
	const NotificationCtor = (sw as unknown as { Notification?: { maxActions?: number } }).Notification;
	return {
		registration: sw.registration as unknown as WorkerRegistration,
		clients: sw.clients as unknown as WorkerClients,
		fetch: (input, init) => sw.fetch(input, init),
		readBearer: readMirroredBearer,
		maxActions: () => NotificationCtor?.maxActions ?? 0,
		origin: sw.location.origin
	};
}

sw.addEventListener('install', () => {
	void sw.skipWaiting();
});

sw.addEventListener('activate', (event) => {
	event.waitUntil(sw.clients.claim());
});

sw.addEventListener('push', (event) => {
	let raw: unknown = null;
	try {
		raw = event.data ? event.data.json() : null;
	} catch {
		raw = event.data?.text() ?? null;
	}
	event.waitUntil(handlePush(raw, deps()));
});

sw.addEventListener('notificationclick', (event) => {
	event.waitUntil(
		handleNotificationClick(
			{ action: event.action, notification: event.notification },
			deps()
		)
	);
});

interface PushSubscriptionChangeEventLike extends ExtendableEvent {
	oldSubscription?: PushSubscription | null;
	newSubscription?: PushSubscription | null;
}

sw.addEventListener('pushsubscriptionchange', (event) => {
	const change = event as PushSubscriptionChangeEventLike;
	change.waitUntil(
		handleSubscriptionChange(
			{
				oldSubscription: change.oldSubscription as unknown as WorkerPushSubscription | null,
				newSubscription: change.newSubscription as unknown as WorkerPushSubscription | null
			},
			{
				fetch: (input, init) => sw.fetch(input, init),
				readBearer: readMirroredBearer,
				label: browserLabel(sw.navigator.userAgent),
				subscribe: async (applicationServerKey) =>
					(await sw.registration.pushManager.subscribe({
						userVisibleOnly: true,
						applicationServerKey
					})) as unknown as WorkerPushSubscription
			}
		)
	);
});
