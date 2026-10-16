import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
	handleNotificationClick,
	handlePush,
	handleSubscriptionChange,
	openAppAt,
	resetWorkerStateForTests,
	UNKNOWN_PUSH_TAG,
	type WorkerDeps,
	type WorkerNotification,
	type WorkerNotificationOptions,
	type WorkerWindowClient
} from './workerHandlers';
import { WEB_PUSH_OPEN_MESSAGE } from './protocol';

const ORIGIN = 'https://magician.test';

function feedItem(schema: Record<string, unknown>) {
	return {
		id: 'feed-1',
		principal: 'owner',
		workspace: 'default',
		item_type: 'attention',
		task_id: 'task-1',
		title: 'Ship the release?',
		status: 'pending',
		created_at: 1,
		updated_at: 2,
		actions: [],
		metadata: {
			attention_kind: 'hitl.requested',
			source: 'user_request',
			request_id: 'cid-1',
			execution_id: 'exec-1',
			input_schema: schema
		}
	};
}

function json(status: number, body: unknown): Response {
	return new Response(body === undefined ? null : JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

interface Shown {
	title: string;
	options: WorkerNotificationOptions;
}

function harness(opts: { maxActions?: number; bearer?: string; windows?: WorkerWindowClient[] } = {}) {
	const shown: Shown[] = [];
	const open = new Map<string, WorkerNotification & { title: string }>();
	const registration = {
		showNotification: vi.fn(async (title: string, options: WorkerNotificationOptions) => {
			shown.push({ title, options });
			const tag = options.tag ?? '';
			const notification = {
				tag,
				title,
				data: options.data,
				close: vi.fn(() => {
					if (open.get(tag) === notification) open.delete(tag);
				})
			};
			open.set(tag, notification);
		}),
		getNotifications: vi.fn(async (filter?: { tag?: string }) =>
			[...open.values()].filter((n) => !filter?.tag || n.tag === filter.tag)
		)
	};
	const fetchMock = vi.fn<typeof fetch>();
	const clients = {
		matchAll: vi.fn(async () => opts.windows ?? []),
		openWindow: vi.fn(async () => null)
	};
	const deps: WorkerDeps = {
		registration,
		clients,
		fetch: fetchMock,
		readBearer: async () => opts.bearer ?? 'tok',
		maxActions: () => opts.maxActions ?? 2,
		origin: ORIGIN,
		openAckTimeoutMs: 20
	};
	return { deps, shown, open, registration, fetchMock, clients };
}

const requested = (extra: Record<string, unknown> = {}) => ({
	kind: 'attention_requested',
	correlation_id: 'cid-1',
	source: 'user_request',
	web_path: '/attention?attention_item=cid-1',
	answer_mode: 'confirm',
	...extra
});

beforeEach(() => {
	resetWorkerStateForTests();
});

describe('handlePush', () => {
	it('shows a generic notification first, tagged by correlation id, then the question with actions', async () => {
		const h = harness({ bearer: 'secret-token' });
		h.fetchMock.mockResolvedValue(
			json(200, feedItem({ type: 'confirmation', confirm_label: 'Ship', deny_label: 'Hold' }))
		);

		const outcome = await handlePush(requested({ answer_mode: 'confirm' }), h.deps);

		expect(outcome).toBe('actions');
		expect(h.shown[0].title).toBe('Magican');
		expect(h.shown[0].options.body).toBe('Magican needs your input');
		expect(h.shown[0].options.tag).toBe('cid-1');
		expect(h.shown[0].options.actions).toBeUndefined();
		expect(h.shown[1].title).toBe('Ship the release?');
		expect(h.shown[1].options.tag).toBe('cid-1');
		expect(h.shown[1].options.actions).toEqual([
			{ action: 'confirm', title: 'Ship' },
			{ action: 'deny', title: 'Hold' }
		]);
		const [url, init] = h.fetchMock.mock.calls[0];
		expect(url).toBe('/api/magician/v2/feed/attention/cid-1');
		expect(new Headers(init?.headers).get('Authorization')).toBe('Bearer secret-token');
	});

	it('accepts an {item} envelope and shows two-way choices as buttons', async () => {
		const h = harness();
		h.fetchMock.mockResolvedValue(
			json(200, {
				item: feedItem({
					type: 'choice',
					options: [
						{ id: 'stg', label: 'Staging' },
						{ id: 'prd', label: 'Production' }
					]
				})
			})
		);
		expect(await handlePush(requested({ answer_mode: 'choice', option_count: 2 }), h.deps)).toBe('actions');
		expect(h.shown[1].options.actions?.map((a) => a.title)).toEqual(['Staging', 'Production']);
	});

	it('shows the question without actions when maxActions is 0 or the ask is not eligible', async () => {
		const zero = harness({ maxActions: 0 });
		zero.fetchMock.mockResolvedValue(json(200, feedItem({ type: 'confirmation' })));
		expect(await handlePush(requested(), zero.deps)).toBe('enriched');
		expect(zero.shown[1].title).toBe('Ship the release?');
		expect(zero.shown[1].options.actions).toBeUndefined();

		const text = harness();
		text.fetchMock.mockResolvedValue(json(200, feedItem({ type: 'text' })));
		expect(await handlePush(requested(), text.deps)).toBe('enriched');
		expect(text.shown[1].options.actions).toBeUndefined();

		const unlisted = harness();
		unlisted.fetchMock.mockResolvedValue(json(200, feedItem({ type: 'confirmation' })));
		expect(await handlePush(requested({ answer_mode: undefined }), unlisted.deps)).toBe('enriched');
		expect(unlisted.shown[1].options.actions).toBeUndefined();

		const secret = harness();
		secret.fetchMock.mockResolvedValue(
			json(200, feedItem({ type: 'confirmation', sensitive: { kind: 'password' } }))
		);
		expect(await handlePush(requested(), secret.deps)).toBe('enriched');
		expect(secret.shown[1].options.actions).toBeUndefined();
	});

	it('keeps the generic notification when the ask cannot be fetched', async () => {
		const h = harness();
		h.fetchMock.mockResolvedValue(json(401, { error: 'unauthorized' }));
		expect(await handlePush(requested(), h.deps)).toBe('generic');
		expect(h.shown).toHaveLength(1);
		expect(h.open.has('cid-1')).toBe(true);

		const offline = harness();
		offline.fetchMock.mockRejectedValue(new TypeError('offline'));
		expect(await handlePush(requested(), offline.deps)).toBe('generic');
	});

	it('closes the notification when the ask is already gone (404)', async () => {
		const h = harness();
		h.fetchMock.mockResolvedValue(json(404, { error: 'not_found' }));
		expect(await handlePush(requested(), h.deps)).toBe('closed');
		expect(h.open.has('cid-1')).toBe(false);
	});

	it('does not re-show a notification dismissed while the ask was being fetched', async () => {
		const h = harness();
		h.fetchMock.mockImplementation(async () => {
			h.open.get('cid-1')?.close();
			return json(200, feedItem({ type: 'confirmation' }));
		});
		expect(await handlePush(requested(), h.deps)).toBe('closed');
		expect(h.shown).toHaveLength(1);
	});

	it('closes notifications with the tag on attention_resolved / attention_expired', async () => {
		const h = harness();
		h.fetchMock.mockResolvedValue(json(500, null));
		await handlePush(requested(), h.deps);
		expect(h.open.has('cid-1')).toBe(true);

		expect(await handlePush({ kind: 'attention_resolved', correlation_id: 'cid-1' }, h.deps)).toBe(
			'closed'
		);
		expect(h.open.has('cid-1')).toBe(false);
		expect(await handlePush({ kind: 'attention_expired', correlation_id: 'cid-1' }, h.deps)).toBe(
			'closed'
		);
	});

	it('still shows a notification for a payload it does not understand', async () => {
		const h = harness();
		expect(await handlePush({ kind: 'mystery' }, h.deps)).toBe('unknown');
		expect(h.shown[0].options.tag).toBe(UNKNOWN_PUSH_TAG);
	});
});

async function shownWithActions(schema: Record<string, unknown>) {
	const h = harness({ bearer: 'secret-token' });
	h.fetchMock.mockResolvedValueOnce(json(200, feedItem(schema)));
	await handlePush(requested({ answer_mode: schema.type === 'choice' ? 'choice' : 'confirm' }), h.deps);
	const notification = h.open.get('cid-1')!;
	return { h, notification };
}

describe('handleNotificationClick', () => {
	it('posts a confirmation answer with channel web_push and shows "Answer sent"', async () => {
		const { h, notification } = await shownWithActions({ type: 'confirmation' });
		h.fetchMock.mockResolvedValueOnce(json(200, { accepted: true }));

		const outcome = await handleNotificationClick({ action: 'deny', notification }, h.deps);

		expect(outcome).toBe('sent');
		const [url, init] = h.fetchMock.mock.calls[1];
		expect(url).toBe('/api/magician/v2/hitl/cid-1/respond');
		expect(init?.method).toBe('POST');
		expect(new Headers(init?.headers).get('Authorization')).toBe('Bearer secret-token');
		expect(JSON.parse(String(init?.body))).toEqual({
			source: 'user_request',
			input_type: 'confirmation',
			value: { type: 'confirmation', confirmed: false },
			channel: 'web_push',
			execution_id: 'exec-1'
		});
		const last = h.shown.at(-1)!;
		expect(last.title).toBe('Answer sent');
		expect(last.options.tag).toBe('cid-1');
		expect(last.options.actions).toBeUndefined();
	});

	it('posts the selected option id for a two-way choice', async () => {
		const { h, notification } = await shownWithActions({
			type: 'choice',
			options: [
				{ id: 'stg', label: 'Staging' },
				{ id: 'prd', label: 'Production' }
			]
		});
		h.fetchMock.mockResolvedValueOnce(json(200, {}));
		await handleNotificationClick({ action: 'option-1', notification }, h.deps);
		expect(JSON.parse(String(h.fetchMock.mock.calls[1][1]?.body)).value).toEqual({
			type: 'choice',
			selected_id: 'prd'
		});
	});

	it.each([
		[404, null, 'already', 'Already answered'],
		[409, { reason: 'already_resolved' }, 'already', 'Already answered'],
		[410, { code: 'pause_state_gone' }, 'already', 'Already answered'],
		[500, { error: 'boom' }, 'failed', 'Couldn’t send your answer — click to open']
	])('maps respond status %i to %s', async (status, body, expected, title) => {
		const { h, notification } = await shownWithActions({ type: 'confirmation' });
		h.fetchMock.mockResolvedValueOnce(json(status, body));
		expect(await handleNotificationClick({ action: 'confirm', notification }, h.deps)).toBe(expected);
		expect(h.shown.at(-1)!.title).toBe(title);
	});

	it('reports a network failure as failed, and a later click on it opens the app', async () => {
		const { h, notification } = await shownWithActions({ type: 'confirmation' });
		h.fetchMock.mockRejectedValueOnce(new TypeError('offline'));
		expect(await handleNotificationClick({ action: 'confirm', notification }, h.deps)).toBe('failed');

		const failed = h.open.get('cid-1')!;
		expect(await handleNotificationClick({ action: '', notification: failed }, h.deps)).toBe('opened');
		expect(h.clients.openWindow).toHaveBeenCalledWith(`${ORIGIN}/attention?attention_item=cid-1`);
	});

	it('opens the ask on a body click instead of answering', async () => {
		const { h, notification } = await shownWithActions({ type: 'confirmation' });
		expect(await handleNotificationClick({ action: '', notification }, h.deps)).toBe('opened');
		expect(h.fetchMock).toHaveBeenCalledTimes(1);
		expect(notification.close).toHaveBeenCalled();
	});
});

describe('openAppAt', () => {
	function windowClient(overrides: Partial<WorkerWindowClient> = {}): WorkerWindowClient {
		return {
			url: `${ORIGIN}/work`,
			focused: false,
			focus: vi.fn(async () => undefined),
			navigate: vi.fn(async () => undefined),
			postMessage: vi.fn(),
			...overrides
		};
	}

	it('focuses an open window and lets the app open the ask in place', async () => {
		const client = windowClient({
			postMessage: vi.fn((message: unknown, transfer?: Transferable[]) => {
				expect(message).toEqual({ type: WEB_PUSH_OPEN_MESSAGE, path: '/attention?attention_item=c' });
				(transfer?.[0] as MessagePort).postMessage({ ok: true });
			})
		});
		const h = harness({ windows: [client] });
		expect(await openAppAt('/attention?attention_item=c', h.deps)).toBe('messaged');
		expect(client.focus).toHaveBeenCalled();
		expect(client.navigate).not.toHaveBeenCalled();
		expect(h.clients.openWindow).not.toHaveBeenCalled();
	});

	it('navigates a window that does not answer', async () => {
		const client = windowClient();
		const h = harness({ windows: [client] });
		expect(await openAppAt('/attention?attention_item=c', h.deps)).toBe('navigated');
		expect(client.navigate).toHaveBeenCalledWith(`${ORIGIN}/attention?attention_item=c`);
	});

	it('opens a new window when none is open, ignoring other origins', async () => {
		const h = harness({ windows: [windowClient({ url: 'https://other.test/' })] });
		expect(await openAppAt('/attention?attention_item=c', h.deps)).toBe('opened');
		expect(h.clients.openWindow).toHaveBeenCalledWith(`${ORIGIN}/attention?attention_item=c`);
	});
});

describe('handleSubscriptionChange', () => {
	it('re-registers a rotated endpoint and retires the old one', async () => {
		const fetchMock = vi.fn<typeof fetch>(async () => json(200, { id: 's1' }));
		const next = {
			endpoint: 'https://push.test/new',
			toJSON: () => ({ endpoint: 'https://push.test/new', keys: { p256dh: 'p', auth: 'a' } })
		};
		const ok = await handleSubscriptionChange(
			{
				oldSubscription: {
					endpoint: 'https://push.test/old',
					options: { applicationServerKey: new Uint8Array([1]).buffer },
					toJSON: () => ({})
				},
				newSubscription: null
			},
			{ fetch: fetchMock, readBearer: async () => 'tok', subscribe: async () => next, label: 'Chrome on macOS' }
		);
		expect(ok).toBe(true);
		const [putUrl, put] = fetchMock.mock.calls[0];
		expect(putUrl).toBe('/api/magician/v2/web-push/subscription');
		expect(put?.method).toBe('PUT');
		expect(JSON.parse(String(put?.body))).toEqual({
			subscription: { endpoint: 'https://push.test/new', keys: { p256dh: 'p', auth: 'a' } },
			label: 'Chrome on macOS'
		});
		expect(fetchMock.mock.calls[1][1]?.method).toBe('DELETE');
		expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({
			endpoint: 'https://push.test/old'
		});
	});
});
