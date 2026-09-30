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

function installHostInvoke(openAppAt: () => Promise<unknown>): void {
	host.invoke.mockImplementation((command: string) => {
		if (command === 'get_app_theme') {
			return Promise.resolve({ name: 'system', tokens: {} });
		}
		if (command === 'get_notifications_enabled') return Promise.resolve(true);
		if (command === 'open_app_at') return openAppAt();
		return Promise.resolve(undefined);
	});
}

beforeEach(() => {
	cards.set([]);
	host.invoke.mockReset();
	host.listen.mockReset();
	host.listen.mockResolvedValue(() => {});
	host.showError.mockReset();
	host.startNotifyStream.mockReset();
	host.stopNotifyStream.mockReset();
	host.reconcileStaleCards.mockReset();
});

afterEach(() => {
	cleanup();
	cards.set([]);
});

describe('/notify-overlay actionable Attention handoff', () => {
	it('passes the encoded correlation id to open_app_at and removes the card only after success', async () => {
		const opened = deferred<void>();
		installHostInvoke(() => opened.promise);
		const card = actionableCard();
		cards.set([card]);
		render(NotifyOverlayPage);
		const user = userEvent.setup();

		await user.click(
			screen.getByRole('button', { name: 'Open notification: Which environment should I use?' })
		);

		await waitFor(() =>
			expect(host.invoke).toHaveBeenCalledWith('open_app_at', {
				path: '/attention?attention_item=pause%2Fid+with+spaces'
			})
		);
		// Navigation has not completed yet, so the pending request must remain
		// visible and present in the source-of-truth store.
		expect(get(cards)).toEqual([card]);
		expect(
			screen.getByRole('button', { name: 'Open notification: Which environment should I use?' })
		).toBeInTheDocument();

		opened.resolve();

		await waitFor(() => expect(get(cards)).toEqual([]));
		await waitFor(() =>
			expect(
				screen.queryByRole('button', {
					name: 'Open notification: Which environment should I use?'
				})
			).not.toBeInTheDocument()
		);
		expect(host.showError).not.toHaveBeenCalled();
	});

	it('retains the card and reports an error when open_app_at fails', async () => {
		installHostInvoke(() => Promise.reject(new Error('native window unavailable')));
		const card = actionableCard({ id: 'notification-failed' });
		cards.set([card]);
		render(NotifyOverlayPage);
		const user = userEvent.setup();

		await user.click(
			screen.getByRole('button', { name: 'Open notification: Which environment should I use?' })
		);

		await waitFor(() =>
			expect(host.showError).toHaveBeenCalledWith(
				'Could not open notification',
				'The Attention request is still pending.'
			)
		);
		expect(get(cards)).toEqual([card]);
		expect(
			screen.getByRole('button', { name: 'Open notification: Which environment should I use?' })
		).toBeInTheDocument();
	});
});
