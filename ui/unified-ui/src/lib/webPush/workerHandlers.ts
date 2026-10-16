/**
 * Service-worker event handlers for HITL web push, written against small
 * structural interfaces so vitest can drive them without a worker runtime.
 * `src/service-worker.ts` wires them to `push`, `notificationclick` and
 * `pushsubscriptionchange`. Docs: docs/components/unified-ui/web-push.md.
 */
import type { HitlResponseValue } from '$lib/hitl/types';
import {
	WEB_PUSH_GENERIC_BODY,
	WEB_PUSH_GENERIC_TITLE,
	WEB_PUSH_OPEN_MESSAGE,
	WEB_PUSH_RESULT_TITLES,
	classifyRespondResult,
	isWebPushNotificationData,
	parseWebPushPayload,
	planAskActions,
	requestFromFeedItem,
	respondBody,
	unwrapFeedItem,
	type WebPushNotificationData,
	type WebPushRespondResult,
	type WebPushRespondTemplate
} from './protocol';
import { subscriptionBody, WEB_PUSH_SUBSCRIPTION_PATH } from './subscriptionBody';

/** Tag for a push this client could not parse — Chrome still requires a notification. */
export const UNKNOWN_PUSH_TAG = 'magician-web-push';

export interface WorkerNotification {
	tag: string;
	data: unknown;
	close(): void;
}

export interface WorkerNotificationOptions {
	body?: string;
	tag?: string;
	data?: unknown;
	icon?: string;
	badge?: string;
	silent?: boolean;
	renotify?: boolean;
	actions?: Array<{ action: string; title: string }>;
}

export interface WorkerRegistration {
	showNotification(title: string, options: WorkerNotificationOptions): Promise<void>;
	getNotifications(filter?: { tag?: string }): Promise<WorkerNotification[]>;
}

export interface WorkerWindowClient {
	url: string;
	focused?: boolean;
	visibilityState?: string;
	focus(): Promise<unknown>;
	navigate?(url: string): Promise<unknown>;
	postMessage(message: unknown, transfer?: Transferable[]): void;
}

export interface WorkerClients {
	matchAll(options: { type: 'window'; includeUncontrolled: boolean }): Promise<readonly WorkerWindowClient[]>;
	openWindow(url: string): Promise<unknown>;
}

export interface WorkerDeps {
	registration: WorkerRegistration;
	clients: WorkerClients;
	fetch: typeof fetch;
	readBearer(): Promise<string>;
	/** `Notification.maxActions` (0 when the platform shows no buttons). */
	maxActions(): number;
	/** The worker's origin, e.g. `self.location.origin`. */
	origin: string;
	/** How long an open window gets to confirm it showed the ask in place. */
	openAckTimeoutMs?: number;
}

const NOTIFICATION_ICON = '/icon-192.png';
const NOTIFICATION_BADGE = '/favicon-48x48.png';

// Asks resolved while their fetch was still in flight must not be re-shown.
const recentlyClosed = new Set<string>();
function markClosed(correlationId: string): void {
	recentlyClosed.add(correlationId);
	if (recentlyClosed.size > 200) {
		const first = recentlyClosed.values().next().value;
		if (first !== undefined) recentlyClosed.delete(first);
	}
}

/** Test seam: forget asks closed by earlier pushes. */
export function resetWorkerStateForTests(): void {
	recentlyClosed.clear();
}

async function authHeaders(deps: WorkerDeps, extra: Record<string, string> = {}): Promise<Headers> {
	const headers = new Headers(extra);
	const bearer = await deps.readBearer().catch(() => '');
	if (bearer) headers.set('Authorization', `Bearer ${bearer}`);
	return headers;
}

async function closeTag(deps: WorkerDeps, tag: string): Promise<void> {
	const open = await deps.registration.getNotifications({ tag }).catch(() => []);
	for (const notification of open) {
		if (notification.tag === tag) notification.close();
	}
}

function baseData(correlationId: string, webPath: string): WebPushNotificationData {
	return { kind: 'magician_attention', correlation_id: correlationId, web_path: webPath };
}

export type PushOutcome = 'generic' | 'enriched' | 'actions' | 'closed' | 'unknown';

/**
 * `push`. An ask shows a generic notification at once (every push must be
 * user-visible), then is re-shown with its question — and two answer buttons
 * when eligible — once the ask is fetched. A resolution closes the ask's
 * notification.
 */
export async function handlePush(raw: unknown, deps: WorkerDeps): Promise<PushOutcome> {
	const payload = parseWebPushPayload(raw);
	if (!payload) {
		await deps.registration.showNotification(WEB_PUSH_GENERIC_TITLE, {
			body: WEB_PUSH_GENERIC_BODY,
			tag: UNKNOWN_PUSH_TAG,
			icon: NOTIFICATION_ICON,
			badge: NOTIFICATION_BADGE,
			data: baseData('', '/attention?attention=1')
		});
		return 'unknown';
	}
	const correlationId = payload.correlation_id;
	if (payload.kind !== 'attention_requested') {
		markClosed(correlationId);
		await closeTag(deps, correlationId);
		return 'closed';
	}

	recentlyClosed.delete(correlationId);
	const data = baseData(correlationId, payload.web_path);
	await deps.registration.showNotification(WEB_PUSH_GENERIC_TITLE, {
		body: WEB_PUSH_GENERIC_BODY,
		tag: correlationId,
		icon: NOTIFICATION_ICON,
		badge: NOTIFICATION_BADGE,
		data
	});

	let response: Response;
	try {
		response = await deps.fetch(
			`/api/magician/v2/feed/attention/${encodeURIComponent(correlationId)}`,
			{ headers: await authHeaders(deps), credentials: 'same-origin', cache: 'no-store' }
		);
	} catch {
		return 'generic';
	}
	if (response.status === 404) {
		// Answered or closed before the push arrived.
		markClosed(correlationId);
		await closeTag(deps, correlationId);
		return 'closed';
	}
	if (!response.ok) return 'generic';
	const item = unwrapFeedItem(await response.json().catch(() => null));
	if (!item) return 'generic';
	const request = requestFromFeedItem(item);
	const question = item.title?.trim() || request?.prompt?.trim() || '';
	if (!question) return 'generic';
	const plan = request ? planAskActions(request, deps.maxActions(), payload.answer_mode) : null;

	// Do not resurrect a notification the reader dismissed, or one a
	// resolution push closed while the fetch was in flight.
	if (recentlyClosed.has(correlationId)) return 'closed';
	const stillOpen = await deps.registration.getNotifications({ tag: correlationId }).catch(() => []);
	if (!stillOpen.some((notification) => notification.tag === correlationId)) return 'closed';

	await deps.registration.showNotification(question, {
		body: WEB_PUSH_GENERIC_BODY,
		tag: correlationId,
		icon: NOTIFICATION_ICON,
		badge: NOTIFICATION_BADGE,
		data: plan ? { ...data, answers: plan.answers, respond: plan.respond } : data,
		...(plan ? { actions: plan.actions } : {})
	});
	return plan ? 'actions' : 'enriched';
}

async function postAnswer(
	deps: WorkerDeps,
	template: WebPushRespondTemplate,
	value: HitlResponseValue
): Promise<WebPushRespondResult> {
	try {
		const response = await deps.fetch(template.url, {
			method: 'POST',
			headers: await authHeaders(deps, { 'Content-Type': 'application/json' }),
			credentials: 'same-origin',
			body: JSON.stringify(respondBody(template, value))
		});
		const payload = await response.json().catch(() => null);
		return classifyRespondResult(response.status, payload);
	} catch {
		return 'failed';
	}
}

function sameOrigin(url: string, origin: string): boolean {
	try {
		return new URL(url).origin === origin;
	} catch {
		return false;
	}
}

function askClientToOpen(client: WorkerWindowClient, path: string, timeoutMs: number): Promise<boolean> {
	if (typeof MessageChannel === 'undefined') return Promise.resolve(false);
	return new Promise((resolve) => {
		const channel = new MessageChannel();
		const timer = setTimeout(() => {
			channel.port1.close();
			resolve(false);
		}, timeoutMs);
		channel.port1.onmessage = (event: MessageEvent) => {
			clearTimeout(timer);
			channel.port1.close();
			resolve(!!(event.data && (event.data as { ok?: unknown }).ok === true));
		};
		try {
			client.postMessage({ type: WEB_PUSH_OPEN_MESSAGE, path }, [channel.port2]);
		} catch {
			clearTimeout(timer);
			channel.port1.close();
			resolve(false);
		}
	});
}

export type OpenOutcome = 'messaged' | 'navigated' | 'opened';

/**
 * Show `path` in the app: an open window is focused and asked to open the ask
 * in place (the app shell answers over a MessageChannel); a window that does
 * not answer is navigated; with no window, a new one is opened.
 */
export async function openAppAt(path: string, deps: WorkerDeps): Promise<OpenOutcome> {
	const url = new URL(path, deps.origin).href;
	const windows = (
		await deps.clients.matchAll({ type: 'window', includeUncontrolled: true }).catch(() => [])
	).filter((client) => sameOrigin(client.url, deps.origin));
	const target =
		windows.find((client) => client.focused) ??
		windows.find((client) => client.visibilityState === 'visible') ??
		windows[0];
	if (target) {
		await target.focus().catch(() => undefined);
		if (await askClientToOpen(target, path, deps.openAckTimeoutMs ?? 600)) return 'messaged';
		if (target.navigate) {
			try {
				await target.navigate(url);
				return 'navigated';
			} catch {
				// Uncontrolled windows cannot be navigated; fall through.
			}
		}
	}
	await deps.clients.openWindow(url);
	return 'opened';
}

export interface ClickEventLike {
	action?: string;
	notification: WorkerNotification & { title?: string };
}

export type ClickOutcome = WebPushRespondResult | OpenOutcome;

/**
 * `notificationclick`. An answer button posts the answer with channel
 * `web_push` and replaces the notification with the result; the body (or a
 * result notification) opens the ask in the app.
 */
export async function handleNotificationClick(
	event: ClickEventLike,
	deps: WorkerDeps
): Promise<ClickOutcome> {
	const { notification } = event;
	const data = isWebPushNotificationData(notification.data) ? notification.data : null;
	notification.close();
	const path = data?.web_path || '/attention?attention=1';
	const value = event.action && data?.answers ? data.answers[event.action] : undefined;
	if (!data || !value || !data.respond) return openAppAt(path, deps);

	const result = await postAnswer(deps, data.respond, value);
	await deps.registration.showNotification(WEB_PUSH_RESULT_TITLES[result], {
		body: notification.title && notification.title !== WEB_PUSH_GENERIC_TITLE ? notification.title : undefined,
		tag: data.correlation_id,
		icon: NOTIFICATION_ICON,
		badge: NOTIFICATION_BADGE,
		silent: true,
		data: baseData(data.correlation_id, data.web_path)
	});
	if (result !== 'failed') markClosed(data.correlation_id);
	return result;
}

export interface WorkerPushSubscription {
	endpoint: string;
	options?: { applicationServerKey?: ArrayBuffer | null };
	toJSON(): { endpoint?: string; keys?: Record<string, string> };
}

export interface SubscriptionChangeDeps {
	fetch: typeof fetch;
	readBearer(): Promise<string>;
	subscribe(applicationServerKey: ArrayBuffer): Promise<WorkerPushSubscription>;
	label?: string;
}

/**
 * `pushsubscriptionchange`: the browser rotated or dropped the endpoint
 * while no page was open. Re-register the new endpoint and retire the old.
 */
export async function handleSubscriptionChange(
	event: { oldSubscription?: WorkerPushSubscription | null; newSubscription?: WorkerPushSubscription | null },
	deps: SubscriptionChangeDeps
): Promise<boolean> {
	let next = event.newSubscription ?? null;
	const key = event.oldSubscription?.options?.applicationServerKey;
	if (!next && key) next = await deps.subscribe(key).catch(() => null);
	if (!next) return false;
	const body = subscriptionBody(next, deps.label);
	if (!body) return false;
	const bearer = await deps.readBearer().catch(() => '');
	const headers: Record<string, string> = { 'Content-Type': 'application/json' };
	if (bearer) headers.Authorization = `Bearer ${bearer}`;
	const put = await deps
		.fetch(WEB_PUSH_SUBSCRIPTION_PATH, {
			method: 'PUT',
			headers,
			credentials: 'same-origin',
			body: JSON.stringify(body)
		})
		.catch(() => null);
	const old = event.oldSubscription?.endpoint;
	if (old && old !== next.endpoint) {
		await deps
			.fetch(WEB_PUSH_SUBSCRIPTION_PATH, {
				method: 'DELETE',
				headers,
				credentials: 'same-origin',
				body: JSON.stringify({ endpoint: old })
			})
			.catch(() => null);
	}
	return !!put?.ok;
}
