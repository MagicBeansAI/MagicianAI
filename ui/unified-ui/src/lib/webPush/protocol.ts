/**
 * Web push for HITL asks — the pure half shared by the service worker and the
 * page. No DOM, no `$app/*`: `src/service-worker.ts` bundles this.
 *
 * The push itself is content-free (`attention_requested` names only the
 * correlation id). The worker shows a generic notification at once, then
 * fetches the ask with the mirrored bearer and — only for a plain two-way
 * decision — re-shows it with the question and two answer buttons. Anything
 * else (text, forms, sensitive or destructive asks, wider choices) opens the
 * app on click instead. Docs: docs/components/unified-ui/web-push.md.
 */
import { hitlRequestFromFeedItem, resolveHitlCall } from '$lib/hitl/adapters';
import type { HitlRequest, HitlResponseValue } from '$lib/hitl/types';
import type { FeedItem } from '$lib/feed/types';
import { PRODUCT_NAME } from '$lib/presentationIdentity';

export const WEB_PUSH_CHANNEL = 'web_push';
export const WEB_PUSH_GENERIC_TITLE = PRODUCT_NAME;
export const WEB_PUSH_GENERIC_BODY = `${PRODUCT_NAME} needs your input`;
/** postMessage type the worker sends an open app window to show an ask in place. */
export const WEB_PUSH_OPEN_MESSAGE = 'magician:web-push:open';

export type WebPushAnswerMode = 'confirm' | 'choice' | 'text';

export type WebPushPayload =
	| {
			kind: 'attention_requested';
			correlation_id: string;
			source: string | null;
			expires_at: string | number | null;
			web_path: string;
			answer_mode: WebPushAnswerMode | null;
			option_count: number | null;
	  }
	| { kind: 'attention_resolved' | 'attention_expired'; correlation_id: string };

function record(value: unknown): Record<string, unknown> | null {
	return value && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function text(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value.trim() : null;
}

/** The Attention deep link for an ask. */
export function defaultAttentionPath(correlationId: string): string {
	return `/attention?attention_item=${encodeURIComponent(correlationId)}`;
}

/**
 * A same-origin app path, or the Attention deep link for `correlationId`. A
 * payload path is server-written, but the worker still refuses anything that
 * a browser could resolve to another origin.
 */
export function safeWebPath(value: unknown, correlationId: string): string {
	const path = text(value);
	if (
		path &&
		path.startsWith('/') &&
		!path.startsWith('//') &&
		!/[\\\u0000- \u007f]/.test(path)
	) {
		return path;
	}
	return defaultAttentionPath(correlationId);
}

/** Parse a decrypted push payload; `null` for anything this client does not handle. */
export function parseWebPushPayload(raw: unknown): WebPushPayload | null {
	let value = raw;
	if (typeof value === 'string') {
		try {
			value = JSON.parse(value);
		} catch {
			return null;
		}
	}
	const body = record(value);
	if (!body) return null;
	const correlationId = text(body.correlation_id);
	if (!correlationId) return null;
	const kind = body.kind;
	if (kind === 'attention_resolved' || kind === 'attention_expired') {
		return { kind, correlation_id: correlationId };
	}
	if (kind !== 'attention_requested') return null;
	const mode = body.answer_mode;
	const optionCount = body.option_count;
	const expires = body.expires_at;
	return {
		kind,
		correlation_id: correlationId,
		source: text(body.source),
		expires_at: typeof expires === 'string' || typeof expires === 'number' ? expires : null,
		web_path: safeWebPath(body.web_path, correlationId),
		answer_mode: mode === 'confirm' || mode === 'choice' || mode === 'text' ? mode : null,
		option_count:
			typeof optionCount === 'number' && Number.isFinite(optionCount) ? optionCount : null
	};
}

/** `GET /feed/attention/{id}` answers a bare FeedItem or `{item: FeedItem}`. */
export function unwrapFeedItem(raw: unknown): FeedItem | null {
	const body = record(raw);
	if (!body) return null;
	const inner = record(body.item);
	const item = inner ?? body;
	return typeof item.id === 'string' || typeof item.title === 'string'
		? (item as unknown as FeedItem)
		: null;
}

/**
 * The canonical request for a fetched ask. Some producers nest the HITL
 * envelope under `metadata.hitl_request`; lift it when the flat read fails.
 */
export function requestFromFeedItem(item: FeedItem): HitlRequest | null {
	const direct = hitlRequestFromFeedItem(item);
	if (direct) return direct;
	const metadata = record(item.metadata);
	const nested = record(metadata?.hitl_request);
	if (!metadata || !nested) return null;
	return hitlRequestFromFeedItem({ ...item, metadata: { ...metadata, ...nested } });
}

export interface WebPushAction {
	action: string;
	title: string;
}

export interface WebPushRespondTemplate {
	url: string;
	body: Record<string, unknown>;
}

export interface WebPushAskPlan {
	mode: 'confirm' | 'choice';
	actions: WebPushAction[];
	/** Response value per action id. */
	answers: Record<string, HitlResponseValue>;
	respond: WebPushRespondTemplate;
}

/** The respond call for `request` with channel `web_push` and no value yet. */
export function respondTemplate(request: HitlRequest): WebPushRespondTemplate | null {
	const placeholder: HitlResponseValue = { type: 'confirmation', confirmed: false };
	const call = resolveHitlCall(request, placeholder);
	if (!call) return null;
	const body: Record<string, unknown> = { ...call.body, channel: WEB_PUSH_CHANNEL };
	delete body.value;
	return { url: call.url, body };
}

/** The full respond body for one tapped action. */
export function respondBody(
	template: WebPushRespondTemplate,
	value: HitlResponseValue
): Record<string, unknown> {
	return { ...template.body, value, channel: WEB_PUSH_CHANNEL };
}

/**
 * Two answer buttons for a plain two-way decision, else `null`.
 *
 * The server's `answer_mode` is the allow-list (`push_answer_mode`: it leaves
 * out approvals, clarifications, authority-granting user requests, …), so a
 * push without one never gets buttons. When it is present the fetched ask is
 * re-checked: a `confirmation` for `confirm`, or a single-select `choice`
 * with exactly two options neither of which needs typed input for `choice`.
 * Never for a sensitive, destructive or multi-question ask, and never when
 * the platform shows fewer than two actions (one button would bias the
 * answer). `text` asks always open the app — a notification cannot take a
 * typed reply on the web.
 */
export function planAskActions(
	request: HitlRequest,
	maxActions: number,
	hint: WebPushAnswerMode | null
): WebPushAskPlan | null {
	if (hint !== 'confirm' && hint !== 'choice') return null;
	if (!Number.isFinite(maxActions) || maxActions < 2) return null;
	const schema = request.schema;
	if (schema.sensitive || schema.destructive === true) return null;
	if (schema.questions && schema.questions.length > 0) return null;
	const respond = respondTemplate(request);
	if (!respond) return null;

	if (request.input_type === 'confirmation') {
		if (hint !== 'confirm') return null;
		return {
			mode: 'confirm',
			actions: [
				{ action: 'confirm', title: schema.confirm_label?.trim() || 'Approve' },
				{ action: 'deny', title: schema.deny_label?.trim() || 'Reject' }
			],
			answers: {
				confirm: { type: 'confirmation', confirmed: true },
				deny: { type: 'confirmation', confirmed: false }
			},
			respond
		};
	}

	if (request.input_type === 'choice') {
		if (hint !== 'choice') return null;
		const options = schema.options ?? [];
		if (options.length !== 2 || schema.multiple === true) return null;
		if (options.some((option) => option.requires_input || !option.id || !option.label?.trim())) {
			return null;
		}
		const actions: WebPushAction[] = [];
		const answers: Record<string, HitlResponseValue> = {};
		options.forEach((option, index) => {
			const action = `option-${index}`;
			actions.push({ action, title: option.label.trim() });
			answers[action] = { type: 'choice', selected_id: option.id };
		});
		return { mode: 'choice', actions, answers, respond };
	}

	return null;
}

/** What the notification data carries so a click can act without refetching. */
export interface WebPushNotificationData {
	kind: 'magician_attention';
	correlation_id: string;
	web_path: string;
	answers?: Record<string, HitlResponseValue>;
	respond?: WebPushRespondTemplate;
}

export function isWebPushNotificationData(value: unknown): value is WebPushNotificationData {
	const data = record(value);
	return !!data && data.kind === 'magician_attention' && typeof data.correlation_id === 'string';
}

export type WebPushRespondResult = 'sent' | 'already' | 'failed';

/**
 * Map a respond response. 404/409/410 all mean the ask is no longer open
 * (answered elsewhere, expired, or the pause drained). A 200 that the server
 * did not accept (re-ask, `accepted:false`) is a failure: the ask is still
 * open and needs the app.
 */
export function classifyRespondResult(status: number, payload: unknown): WebPushRespondResult {
	if (status === 404 || status === 409 || status === 410) return 'already';
	if (status < 200 || status >= 300) return 'failed';
	const body = record(payload);
	if (body?.status === 'reask_required') return 'failed';
	if (body?.accepted === false || body?.resumed === false) return 'failed';
	return 'sent';
}

export const WEB_PUSH_RESULT_TITLES: Record<WebPushRespondResult, string> = {
	sent: 'Answer sent',
	already: 'Already answered',
	failed: 'Couldn’t send your answer — click to open'
};

/** VAPID public key (base64url) → the bytes `pushManager.subscribe` takes. */
export function urlBase64ToUint8Array(base64: string): Uint8Array<ArrayBuffer> {
	const padded = `${base64}${'='.repeat((4 - (base64.length % 4)) % 4)}`
		.replace(/-/g, '+')
		.replace(/_/g, '/');
	const raw = atob(padded);
	const bytes = new Uint8Array(new ArrayBuffer(raw.length));
	for (let index = 0; index < raw.length; index += 1) bytes[index] = raw.charCodeAt(index);
	return bytes;
}
