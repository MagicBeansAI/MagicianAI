import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { get } from 'svelte/store';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';

import type { NotifyCard } from '$lib/notify/cardModel';

const host = vi.hoisted(() => ({
	invoke: vi.fn(),
	listen: vi.fn(),
	showError: vi.fn(),
	startNotifyStream: vi.fn(),
	stopNotifyStream: vi.fn(),
	reconcileStaleCards: vi.fn()
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: host.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: host.listen }));
vi.mock('$lib/shared/stores/notifications', () => ({ showError: host.showError }));
vi.mock('$lib/notify/notifyStream', async () => {
	const { writable } = await vi.importActual<typeof import('svelte/store')>('svelte/store');
	return {
		cards: writable<NotifyCard[]>([]),
		startNotifyStream: host.startNotifyStream,
		stopNotifyStream: host.stopNotifyStream,
		reconcileStaleCards: host.reconcileStaleCards
	};
});

import { cards } from '$lib/notify/notifyStream';
vi.mock('$lib/stores/attentionStore', async () => {
	const { writable } = await vi.importActual<typeof import('svelte/store')>('svelte/store');
	return { attentionStore: Object.assign(writable({}), { start: vi.fn(), stop: vi.fn() }) };
});
vi.mock('$lib/attention/model', async () => {
	const { writable } = await vi.importActual<typeof import('svelte/store')>('svelte/store');
	return { attentionRows: writable([]) };
});
vi.mock('$lib/attention/attentionBadgeCount', async () => {
	const { writable } = await vi.importActual<typeof import('svelte/store')>('svelte/store');
	return { attentionBadgeCount: writable(0) };
});
vi.mock('$lib/stores/pendingHitlStore', () => ({ ensurePendingHitlBridge: vi.fn() }));
vi.mock('$lib/stores/workLiveStore', async () => {
	const { writable } = await vi.importActual<typeof import('svelte/store')>('svelte/store');
	return {
		workLiveStore: Object.assign(writable({ items: [], loaded: true, error: null }), {
			start: vi.fn(),
			stop: vi.fn(),
			refresh: vi.fn()
		})
	};
});
import { workLiveStore } from '$lib/stores/workLiveStore';
import type { WorkItem } from '$lib/stores/workLiveStore';
import { attentionRows } from '$lib/attention/model';
import { attentionBadgeCount } from '$lib/attention/attentionBadgeCount';
import { attentionStore } from '$lib/stores/attentionStore';
import type { Writable } from 'svelte/store';
import type { AttentionDisplayRow } from '$lib/attention/model';
import NotifyOverlayPage from './+page.svelte';

const originalGetAnimations = Object.getOwnPropertyDescriptor(Element.prototype, 'getAnimations');

beforeAll(() => {
	// Svelte waits for outro animations before detaching a card. jsdom does not
	// implement Web Animations, so provide the empty browser result for this
	// mounted route test rather than bypassing the production transition.
	Object.defineProperty(Element.prototype, 'getAnimations', {
		configurable: true,
		value: () => []
	});
});

afterAll(() => {
	if (originalGetAnimations) {
		Object.defineProperty(Element.prototype, 'getAnimations', originalGetAnimations);
	} else {
		Reflect.deleteProperty(Element.prototype, 'getAnimations');
	}
});

interface Deferred<T> {
	promise: Promise<T>;
	resolve: (value: T | PromiseLike<T>) => void;
	reject: (reason?: unknown) => void;
}

function deferred<T>(): Deferred<T> {
	let resolve!: Deferred<T>['resolve'];
	let reject!: Deferred<T>['reject'];
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

function actionableCard(
	overrides: Partial<Extract<NotifyCard, { kind: 'actionable' }>> = {}
): Extract<NotifyCard, { kind: 'actionable' }> {
	return {
		id: 'notification-1',
		kind: 'actionable' as const,
		correlationId: 'pause/id with spaces',
		source: 'clarification',
		inputType: 'text',
		prompt: 'Which environment should I use?',
		...overrides
	};
}

const pendingNotchAnswers: unknown[] = [];

function installHostInvoke(openAppAt: () => Promise<unknown>): void {
	host.invoke.mockImplementation((command: string) => {
		if (command === 'get_app_theme') {
			return Promise.resolve({ name: 'system', tokens: {} });
		}
		if (command === 'get_notifications_enabled') return Promise.resolve(true);
		if (command === 'drain_notch_answers') {
			return Promise.resolve(pendingNotchAnswers.splice(0));
		}
		if (command === 'open_app_at') return openAppAt();
		return Promise.resolve(undefined);
	});
}

function deliverNotchAnswer(
	onAnswer: ((event: { payload: unknown }) => void) | undefined,
	payload: unknown
): void {
	pendingNotchAnswers.push(payload);
	onAnswer?.({ payload });
}

beforeEach(() => {
	(attentionRows as Writable<AttentionDisplayRow[]>).set([]);
	(attentionBadgeCount as Writable<number>).set(0);
	cards.set([]);
	host.invoke.mockReset();
	host.listen.mockReset();
	host.listen.mockResolvedValue(() => {});
	host.showError.mockReset();
	host.startNotifyStream.mockReset();
	host.stopNotifyStream.mockReset();
	host.reconcileStaleCards.mockReset();
	pendingNotchAnswers.splice(0);
});

afterEach(() => {
	cleanup();
	cards.set([]);
});

describe('/notify-overlay actionable Attention handoff', () => {
	it('rehydrates persistent Attention without a live card and preserves the server total', async () => {
		installHostInvoke(() => Promise.resolve());
		const card = actionableCard();
		(attentionRows as Writable<AttentionDisplayRow[]>).set([{
			key: card.id, correlation_id: card.correlationId, source: card.source,
			prompt: card.prompt, request: null
		} as AttentionDisplayRow]);
		(attentionBadgeCount as Writable<number>).set(103);
		render(NotifyOverlayPage);

		await waitFor(() =>
			expect(host.invoke).toHaveBeenCalledWith('publish_notch_inbox', {
				totalCount: 103,
				items: [
					expect.objectContaining({
						id: card.id,
						correlationId: card.correlationId,
						prompt: card.prompt,
						inline: 'open'
					})
				]
			})
		);
		expect(attentionStore.start).toHaveBeenCalled();
		(attentionRows as Writable<AttentionDisplayRow[]>).set([]);
		(attentionBadgeCount as Writable<number>).set(0);
		await waitFor(() => expect(host.invoke).toHaveBeenCalledWith('publish_notch_inbox', {
			items: [], totalCount: 0
		}));
		expect(
			screen.queryByRole('button', { name: 'Open notification: Which environment should I use?' })
		).not.toBeInTheDocument();
		expect(host.invoke).not.toHaveBeenCalledWith('show_notify_overlay');
	});

	it('opens Attention from the notch and removes the card only after success', async () => {
		const opened = deferred<void>();
		installHostInvoke(() => opened.promise);
		let onAnswer: ((event: { payload: unknown }) => void) | undefined;
		host.listen.mockImplementation(async (event: string, handler: (event: { payload: unknown }) => void) => {
			if (event === 'notch://answer') onAnswer = handler;
			return () => {};
		});
		const card = actionableCard();
		cards.set([card]);
		render(NotifyOverlayPage);
		await waitFor(() => expect(onAnswer).toEqual(expect.any(Function)));

		deliverNotchAnswer(onAnswer, {
			id: card.id,
			correlationId: card.correlationId,
			kind: 'open'
		});

		await waitFor(() =>
			expect(host.invoke).toHaveBeenCalledWith('open_app_at', {
				path: '/attention?attention_item=pause%2Fid%20with%20spaces'
			})
		);
		expect(get(cards)).toEqual([card]);

		opened.resolve();

		await waitFor(() => expect(get(cards)).toEqual([]));
		expect(host.showError).not.toHaveBeenCalled();
	});

	it('retains the card and reports an error when opening Attention from the notch fails', async () => {
		installHostInvoke(() => Promise.reject(new Error('native window unavailable')));
		let onAnswer: ((event: { payload: unknown }) => void) | undefined;
		host.listen.mockImplementation(async (event: string, handler: (event: { payload: unknown }) => void) => {
			if (event === 'notch://answer') onAnswer = handler;
			return () => {};
		});
		const card = actionableCard({ id: 'notification-failed' });
		cards.set([card]);
		render(NotifyOverlayPage);
		await waitFor(() => expect(onAnswer).toEqual(expect.any(Function)));

		deliverNotchAnswer(onAnswer, {
			id: card.id,
			correlationId: card.correlationId,
			kind: 'open'
		});

		await waitFor(() =>
			expect(host.showError).toHaveBeenCalledWith(
				'Could not open notification',
				'The Attention request is still pending.'
			)
		);
		expect(get(cards)).toEqual([card]);
	});

	it('dismisses an Ask or HITL question from the notch without posting an answer', async () => {
		installHostInvoke(() => Promise.resolve());
		let onAnswer: ((event: { payload: unknown }) => void) | undefined;
		host.listen.mockImplementation(async (event: string, handler: (event: { payload: unknown }) => void) => {
			if (event === 'notch://answer') onAnswer = handler;
			return () => {};
		});
		const card = actionableCard({ id: 'notification-dismiss' });
		window.localStorage.removeItem('magician.notify.dismissed-actionable.v1');
		cards.set([card]);
		render(NotifyOverlayPage);
		await waitFor(() => expect(onAnswer).toEqual(expect.any(Function)));

		deliverNotchAnswer(onAnswer, {
			id: card.id,
			correlationId: card.correlationId,
			kind: 'dismiss'
		});

		await waitFor(() => expect(get(cards)).toEqual([]));
		expect(host.invoke).not.toHaveBeenCalledWith('open_app_at', expect.anything());
		expect(host.invoke).toHaveBeenCalledWith('notch_report', {
			id: card.id,
			ok: true,
			message: null
		});
		expect(window.localStorage.getItem('magician.notify.dismissed-actionable.v1')).toContain(
			card.correlationId
		);
	});

	it('drops a card the host already posted without posting again', async () => {
		installHostInvoke(() => Promise.resolve());
		const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}'));
		let onAnswer: ((event: { payload: unknown }) => void) | undefined;
		host.listen.mockImplementation(async (event: string, handler: (event: { payload: unknown }) => void) => {
			if (event === 'notch://answer') onAnswer = handler;
			return () => {};
		});
		const card = actionableCard({ id: 'already-posted', correlationId: 'already-posted' });
		cards.set([card]);
		render(NotifyOverlayPage);
		await waitFor(() => expect(onAnswer).toEqual(expect.any(Function)));

		deliverNotchAnswer(onAnswer, {
			id: card.id,
			correlationId: card.correlationId,
			kind: 'text',
			text: 'ok',
			posted: true
		});

		await waitFor(() => expect(get(cards)).toEqual([]));
		expect(host.invoke).toHaveBeenCalledWith('notch_report', {
			id: card.id,
			ok: true,
			message: null
		});
		expect(fetchSpy).not.toHaveBeenCalled();
		fetchSpy.mockRestore();
	});

	it('drops a text answer the notch cannot post and reports success without posting', async () => {
		installHostInvoke(() => Promise.resolve());
		const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}'));
		let onAnswer: ((event: { payload: unknown }) => void) | undefined;
		host.listen.mockImplementation(async (event: string, handler: (event: { payload: unknown }) => void) => {
			if (event === 'notch://answer') onAnswer = handler;
			return () => {};
		});
		const card = actionableCard({ id: 'orphaned-text', correlationId: 'orphaned-text' });
		cards.set([card]);
		render(NotifyOverlayPage);
		await waitFor(() => expect(onAnswer).toEqual(expect.any(Function)));

		deliverNotchAnswer(onAnswer, {
			id: card.id,
			correlationId: card.correlationId,
			kind: 'text',
			text: 'biryani'
		});

		await waitFor(() => expect(get(cards)).toEqual([]));
		expect(host.invoke).toHaveBeenCalledWith('notch_report', {
			id: card.id,
			ok: true,
			message: null
		});
		expect(host.showError).not.toHaveBeenCalled();
		expect(fetchSpy).not.toHaveBeenCalled();
		fetchSpy.mockRestore();
	});
	it('publishes work item dots to the notch ear', async () => {
		installHostInvoke(() => Promise.resolve());
		render(NotifyOverlayPage);
		const crew: WorkItem = {
			id: 'crew:crew_1',
			kind: 'crew',
			title: 'Add checkout',
			context: 'Shop',
			state: 'needs_you',
			step: null,
			byOwner: true,
			fromPhone: false,
			followed: false,
			startedAt: '2026-10-12T11:50:00Z',
			updatedAt: '2026-10-12T11:59:00Z',
			waitingSince: '2026-10-12T11:59:00Z',
			spentUsd: null,
			budgetUsd: null,
			action: null,
			deepLink: 'magican://vibe?project=proj&crew=crew_1',
			webPath: '/vibe?project=proj&crew=crew_1',
			projectId: 'proj',
			runId: null,
			liveTaskId: null,
			crewId: 'crew_1',
			taskId: null
		};
		(workLiveStore as unknown as Writable<{ items: WorkItem[] }>).set({ items: [crew] });
		await waitFor(() =>
			expect(host.invoke).toHaveBeenCalledWith('publish_notch_runs', {
				runs: [
					{
						id: 'crew:crew_1',
						state: 'needs_you',
						title: 'Add checkout · Shop',
						path: '/vibe?project=proj&crew=crew_1'
					}
				]
			})
		);
		(workLiveStore as unknown as Writable<{ items: WorkItem[] }>).set({ items: [] });
	});
});
