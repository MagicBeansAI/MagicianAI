/** Wire shapes for `/api/magician/v2/web-push/*`, shared by page and worker. */

export const WEB_PUSH_CONFIG_PATH = '/api/magician/v2/web-push/config';
export const WEB_PUSH_SUBSCRIPTION_PATH = '/api/magician/v2/web-push/subscription';

export interface WebPushSubscriptionBody {
	subscription: { endpoint: string; keys: { p256dh: string; auth: string } };
	label?: string;
}

/** `PUT /web-push/subscription` body, or `null` when the subscription lacks its keys. */
export function subscriptionBody(
	subscription: { toJSON(): { endpoint?: string; keys?: Record<string, string> } },
	label?: string
): WebPushSubscriptionBody | null {
	const json = subscription.toJSON();
	const endpoint = json.endpoint?.trim();
	const p256dh = json.keys?.p256dh;
	const auth = json.keys?.auth;
	if (!endpoint || !p256dh || !auth) return null;
	const body: WebPushSubscriptionBody = { subscription: { endpoint, keys: { p256dh, auth } } };
	const trimmed = label?.trim();
	if (trimmed) body.label = trimmed.slice(0, 80);
	return body;
}

/** A short device label from the user agent, e.g. "Chrome on macOS". */
export function browserLabel(userAgent: string): string {
	const ua = userAgent || '';
	const browserName = /Edg\//.test(ua)
		? 'Edge'
		: /Firefox\//.test(ua)
			? 'Firefox'
			: /Chrome\//.test(ua)
				? 'Chrome'
				: /Safari\//.test(ua)
					? 'Safari'
					: 'Browser';
	const os = /iPhone|iPad|iPod/.test(ua)
		? 'iOS'
		: /Android/.test(ua)
			? 'Android'
			: /Mac OS X|Macintosh/.test(ua)
				? 'macOS'
				: /Windows/.test(ua)
					? 'Windows'
					: /Linux/.test(ua)
						? 'Linux'
						: '';
	return os ? `${browserName} on ${os}` : browserName;
}
