import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse, type MockFetchCall } from '../../../test/browser';

const router = vi.hoisted(() => {
	type Snapshot = { url: URL; state: Record<string, unknown>; params: Record<string, string> };
	let page: Snapshot = { url: new URL('http://localhost/notes'), state: {}, params: {} };
	const subscribers = new Set<(snapshot: Snapshot) => void>();
	return {
		page: {
			subscribe(subscriber: (snapshot: Snapshot) => void): () => void {
				subscribers.add(subscriber);
				subscriber(page);
				return () => subscribers.delete(subscriber);
			}
		},
		set(url: string): void {
			page = { ...page, url: new URL(url, 'http://localhost') };
			for (const subscriber of subscribers) subscriber(page);
		},
		current(): URL {
			return page.url;
		}
	};
});

const navigation = vi.hoisted(() => ({
	goto: vi.fn(async (url: string) => router.set(url))
}));

vi.mock('$app/stores', () => ({
	page: router.page,
	navigating: { subscribe: (run: (value: null) => void) => (run(null), () => {}) },
	updated: { subscribe: (run: (value: boolean) => void) => (run(false), () => {}), check: async () => false }
}));

vi.mock('$app/navigation', () => ({
	goto: navigation.goto,
	replaceState: vi.fn(),
	pushState: vi.fn(),
	beforeNavigate: vi.fn(),
	afterNavigate: vi.fn(),
	onNavigate: vi.fn(),
	invalidate: vi.fn(async () => {}),
	invalidateAll: vi.fn(async () => {})
}));

const session = vi.hoisted(() => ({
	refreshScopeSession: vi.fn(),
	switchScopeWorkspace: vi.fn()
}));

vi.mock('$lib/stores/scopeIdentityStore', async () => {
	const actual = await vi.importActual<typeof import('$lib/stores/scopeIdentityStore')>(
		'$lib/stores/scopeIdentityStore'
	);
	return {
		...actual,
		refreshScopeSession: session.refreshScopeSession,
		switchScopeWorkspace: session.switchScopeWorkspace
	};
});

import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import Page from './+page.svelte';

const WORKSPACES = [
	{ id: 'home', display_name: 'Home', is_default: true },
	{ id: 'acme', display_name: 'Acme', is_default: false }
];

function sessionIn(workspace: string) {
	return {
		identity: { name: 'owner', display_name: 'Owner' },
		method: 'password',
		principal: 'owner',
		workspace,
		workspaces: WORKSPACES
	};
}

function routes() {
	return [
		{
			method: 'GET',
			match: '/notes/audio',
			handle: () => jsonResponse({ items: [], offset: 0, limit: 100, total: 0, has_more: false })
		},
		{ method: 'GET', match: '/notes/settings', handle: () => jsonResponse({ settings: { enabled: true }, resolved: {} }) },
		{ method: 'GET', match: '/notes/providers/status', handle: () => jsonResponse({}) },
		{
			method: 'GET',
			match: '/notes/tree?path=',
			handle: () =>
				jsonResponse({
					provider: 'silverbullet',
					path: '',
					entries: [
						{ name: 'Inbox', relative_path: 'Inbox', kind: 'dir', has_children: true },
						{ name: 'Plan.md', relative_path: 'Plan.md', kind: 'file' }
					]
				})
		},
		{
			method: 'GET',
			match: '/notes/file?path=',
			handle: (call: MockFetchCall) => {
				const path = new URL(call.url, 'http://localhost').searchParams.get('path') ?? '';
				return jsonResponse({ title: path, relative_path: path, markdown: `# ${path}\n\nBody` });
			}
		},
		{ method: 'GET', match: '/notes/backlinks', handle: () => jsonResponse({ backlinks: [] }) }
	];
}

function noteReads(calls: MockFetchCall[]): MockFetchCall[] {
	return calls.filter((call) => call.url.includes('/notes/'));
}

beforeEach(() => {
	session.refreshScopeSession.mockReset();
	session.switchScopeWorkspace.mockReset();
	navigation.goto.mockClear();
	scopeIdentityStore.observe('owner', 'home');
});

afterEach(() => {
	cleanup();
	scopeIdentityStore.reset();
	vi.unstubAllGlobals();
});

describe('notes deep links', () => {
	it('switches the session to the linked workspace when it may enter it, then opens the note', async () => {
		router.set('/notes?path=Inbox%2FIdea.md&workspace=acme');
		session.refreshScopeSession.mockResolvedValue(sessionIn('home'));
		session.switchScopeWorkspace.mockImplementation(async (workspace: string) => {
			scopeIdentityStore.observe('owner', workspace);
			return sessionIn(workspace);
		});
		const { calls } = installFetchMock(routes());

		render(Page);

		await waitFor(() => expect(session.switchScopeWorkspace).toHaveBeenCalledWith('acme'));
		expect(await screen.findByText('Inbox/Idea.md', { selector: 'header p' })).toBeTruthy();
		// Notes were read only after the session moved.
		const firstRead = calls.findIndex((call) => call.url.includes('/notes/'));
		expect(firstRead).toBeGreaterThanOrEqual(0);
		expect(router.current().searchParams.get('workspace')).toBe('acme');
		expect(screen.queryByText(/not signed into/)).toBeNull();
	});

	it('shows a blocked state and reads no notes for a workspace the session cannot enter', async () => {
		router.set('/notes?path=Secret.md&workspace=elsewhere');
		session.refreshScopeSession.mockResolvedValue(sessionIn('home'));
		const { calls } = installFetchMock(routes());

		render(Page);

		expect(
			await screen.findByRole('heading', { name: "This note is in a workspace you're not signed into" })
		).toBeTruthy();
		expect(session.switchScopeWorkspace).not.toHaveBeenCalled();
		expect(noteReads(calls)).toHaveLength(0);
		expect(screen.queryByText('Secret.md')).toBeNull();
		expect(screen.queryByRole('heading', { name: 'Files' })).toBeNull();
	});

	it('opens a link into the current workspace without touching the session', async () => {
		router.set('/notes?path=Plan.md&workspace=home');
		installFetchMock(routes());

		render(Page);

		expect(await screen.findByText('Plan.md', { selector: 'header p' })).toBeTruthy();
		expect(session.refreshScopeSession).not.toHaveBeenCalled();
		expect(session.switchScopeWorkspace).not.toHaveBeenCalled();
	});

	it('keeps the address bar on the canonical link of the note opened inside Notes', async () => {
		router.set('/notes');
		installFetchMock(routes());
		const user = userEvent.setup();

		render(Page);

		await user.click(await screen.findByRole('button', { name: /Plan\.md/ }));
		await waitFor(() =>
			expect(`${router.current().pathname}${router.current().search}`).toBe(
				'/notes?path=Plan.md&workspace=home'
			)
		);
		expect(navigation.goto).toHaveBeenLastCalledWith(
			'/notes?path=Plan.md&workspace=home',
			expect.objectContaining({ replaceState: true })
		);
	});

	it('copies the absolute link of the open note', async () => {
		router.set('/notes?path=Plan.md&workspace=home');
		installFetchMock(routes());
		const user = userEvent.setup();
		// After setup: user-event installs its own clipboard stub.
		const writeText = vi.fn(async () => undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });

		render(Page);

		await user.click(await screen.findByRole('button', { name: 'Copy link' }));
		expect(writeText).toHaveBeenCalledWith(
			`${window.location.origin}/notes?path=Plan.md&workspace=home`
		);
		expect(await screen.findByRole('button', { name: 'Link copied' })).toBeTruthy();
	});
});
