import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import ObservableSourcesPanel from './ObservableSourcesPanel.svelte';
import type { ObservableSourceOffer, ObservationSubscription } from './sourceApi';

const offer: ObservableSourceOffer = {
	offer_id: 'arxiv-ai:observe-rss',
	source_id: 'arxiv-ai',
	source_revision: 'source-r1',
	profile_id: 'observe-rss',
	profile_revision: 'profile-r1',
	display_name: 'arXiv AI',
	category: 'research',
	description: 'New AI papers',
	readiness: 'eligible',
	subscribed: false,
	supported_cadence: ['hourly', 'daily'],
	default_cadence: 'hourly',
	limits: { max_candidates_per_run: 50, max_selected_per_run: 10 },
	targets: ['https://rss.arxiv.org/rss/cs.AI'],
	action_bindings: [{ action_id: 'rss.discover', adapter_id: 'rss' }]
};

const subscription: ObservationSubscription = {
	...offer,
	subscription_id: 'obs_1',
	enabled: true,
	custom: false,
	action_id: 'rss.discover',
	cadence: 'hourly',
	next_run_at_ms: Date.now() + 60_000,
	intent: null,
	max_candidates_per_run: 50,
	max_selected_per_run: 10,
	last_success_at_ms: null,
	consecutive_failures: 0,
	revision: 1
};

const notesOffer: ObservableSourceOffer = {
	offer_id: 'notes:observe-notes',
	source_id: 'notes',
	source_revision: 'notes-r1',
	profile_id: 'observe-notes',
	profile_revision: 'notes-profile-r1',
	display_name: 'Notes',
	category: 'knowledge',
	description: 'Observe new and edited Markdown notes.',
	readiness: 'eligible',
	subscribed: false,
	supported_cadence: ['hourly', 'twice_daily', 'daily'],
	default_cadence: 'hourly',
	limits: { max_candidates_per_run: 100, max_selected_per_run: 20 },
	targets: ['notes://configured'],
	action_bindings: [{ action_id: 'notes.discover', adapter_id: 'notes' }]
};

const notesSubscription: ObservationSubscription = {
	...notesOffer,
	subscription_id: 'obs_notes',
	enabled: true,
	custom: false,
	action_id: 'notes.discover',
	cadence: 'hourly',
	next_run_at_ms: Date.now() + 60_000,
	intent: null,
	max_candidates_per_run: 100,
	max_selected_per_run: 20,
	last_success_at_ms: null,
	consecutive_failures: 0,
	revision: 1
};

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('ObservableSourcesPanel', () => {
	it('renders server totals, paginates with cursors, and enables an exact offer', async () => {
		const calls: Array<{ url: string; init?: RequestInit }> = [];
		let listening = false;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
				const url = String(request);
				calls.push({ url, init });
				if (init?.method === 'PUT') {
					listening = true;
					return { ok: true, status: 200, json: async () => subscription };
				}
				if (url.includes('/observe/subscriptions')) {
					return {
						ok: true,
						status: 200,
						json: async () => ({
							items: listening && !url.includes('limit=1') ? [subscription] : [],
							total: listening ? 1 : 0
						})
					};
				}
				if (url.includes('readiness=needs_setup')) {
					return { ok: true, status: 200, json: async () => ({ items: [], total: 0 }) };
				}
				const secondPage = url.includes('cursor=offer-5');
				return {
					ok: true,
					status: 200,
					json: async () => ({
						items: secondPage ? [{ ...offer, offer_id: 'offer-6', display_name: 'Page two' }] : [offer],
						total: 6,
						next_cursor: secondPage ? null : 'offer-5',
						catalog_revision: 'catalog-r1',
						manifest_issues: []
					})
				};
			})
		);

		const user = userEvent.setup();
		render(ObservableSourcesPanel);
		await user.click(await screen.findByRole('tab', { name: /Available 6/ }));
		expect(screen.getByText('arXiv AI')).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Next page' }));
		await screen.findByText('Page two');
		expect(calls.some((call) => call.url.includes('cursor=offer-5'))).toBe(true);

		await user.click(screen.getByRole('button', { name: 'Previous page' }));
		await user.click(await screen.findByRole('button', { name: 'Listen' }));
		await waitFor(() => expect(screen.getByRole('tab', { name: /Listening 1/ })).toBeInTheDocument());
		const put = calls.find((call) => call.init?.method === 'PUT');
		expect(JSON.parse(String(put?.init?.body))).toMatchObject({
			source_id: 'arxiv-ai',
			profile_id: 'observe-rss',
			source_revision: 'source-r1',
			enabled: true,
			cadence: 'hourly'
		});
		expect(screen.queryByRole('option', { name: 'Twice Daily' })).not.toBeInTheDocument();
	});

	it('offers Notes as a first-class target and subscribes it to the common source API', async () => {
		const calls: Array<{ url: string; init?: RequestInit }> = [];
		let listening = false;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
				const url = String(request);
				calls.push({ url, init });
				if (init?.method === 'PUT') {
					listening = true;
					return { ok: true, status: 200, json: async () => notesSubscription };
				}
				if (url.includes('/observe/subscriptions')) {
					return {
						ok: true,
						status: 200,
						json: async () => ({
							items: listening && !url.includes('limit=1') ? [notesSubscription] : [],
							total: listening ? 1 : 0
						})
					};
				}
				if (url.includes('readiness=needs_setup')) {
					return { ok: true, status: 200, json: async () => ({ items: [], total: 0 }) };
				}
				return {
					ok: true,
					status: 200,
					json: async () => ({
						items: [notesOffer],
						total: 1,
						catalog_revision: 'catalog-r1',
						manifest_issues: []
					})
				};
			})
		);

		const user = userEvent.setup();
		render(ObservableSourcesPanel);
		await user.click(await screen.findByRole('tab', { name: /Available 1/ }));
		expect(screen.getByText('Notes')).toBeInTheDocument();
		await user.click(screen.getByRole('button', { name: 'Listen' }));

		await screen.findByText(/Via Notes/);
		const put = calls.find((call) => call.init?.method === 'PUT');
		expect(JSON.parse(String(put?.init?.body))).toMatchObject({
			source_id: 'notes',
			profile_id: 'observe-notes',
			source_revision: 'notes-r1',
			enabled: true,
			cadence: 'hourly'
		});
		expect(calls.some((call) => call.url.includes('required_action='))).toBe(false);
	});

	it('shows stable skeleton, error recovery, and custom RSS controls', async () => {
		let fail = true;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request) => {
				const url = String(request);
				if (fail && url.includes('/observe/subscriptions') && !url.includes('limit=1')) {
					return {
						ok: false,
						status: 503,
						json: async () => ({ message: 'Backend starting' })
					};
				}
				return {
					ok: true,
					status: 200,
					json: async () => ({ items: [], total: 0, catalog_revision: 'r', manifest_issues: [] })
				};
			})
		);
		const user = userEvent.setup();
		render(ObservableSourcesPanel);
		await screen.findByText('Backend starting');
		fail = false;
		await user.click(screen.getByRole('button', { name: 'Retry' }));
		await screen.findByText('No sources are listening');

		await user.click(screen.getByRole('tab', { name: /Available/ }));
		await user.click(screen.getByRole('button', { name: /Add RSS feed/ }));
		const form = screen.getByPlaceholderText('Feed name').closest('form');
		expect(form).not.toBeNull();
		expect(within(form as HTMLFormElement).getByRole('button', { name: 'Start listening' })).toBeInTheDocument();
	});

	it('recovers a stale server cursor by restarting the active tab', async () => {
		const calls: string[] = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request) => {
				const url = String(request);
				calls.push(url);
				if (url.includes('cursor=stale-offer')) {
					return {
						ok: false,
						status: 409,
						json: async () => ({ error: 'stale_cursor', message: 'restart pagination' })
					};
				}
				if (url.includes('/observe/subscriptions')) {
					return { ok: true, status: 200, json: async () => ({ items: [], total: 0 }) };
				}
				if (url.includes('readiness=needs_setup')) {
					return {
						ok: true,
						status: 200,
						json: async () => ({ items: [], total: 0, catalog_revision: 'r', manifest_issues: [] })
					};
				}
				return {
					ok: true,
					status: 200,
					json: async () => ({
						items: [offer],
						total: 6,
						next_cursor: 'stale-offer',
						catalog_revision: 'r',
						manifest_issues: []
					})
				};
			})
		);

		const user = userEvent.setup();
		render(ObservableSourcesPanel);
		await user.click(await screen.findByRole('tab', { name: /Available 6/ }));
		await user.click(screen.getByRole('button', { name: 'Next page' }));
		await screen.findByText('arXiv AI');

		expect(calls.some((url) => url.includes('cursor=stale-offer'))).toBe(true);
		expect(screen.queryByText('restart pagination')).not.toBeInTheDocument();
		expect(screen.getByText('1-5 of 6')).toBeInTheDocument();
	});

	it('refreshes authoritative subscription state after a stale source mutation', async () => {
		let listeningLoads = 0;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
				const url = String(request);
				if (init?.method === 'PUT') {
					return {
						ok: false,
						status: 409,
						json: async () => ({
							error: 'stale_source',
							message: 'observable source definition changed; refresh and retry'
						})
					};
				}
				if (url.includes('/observe/subscriptions')) {
					if (!url.includes('limit=1')) listeningLoads += 1;
					return {
						ok: true,
						status: 200,
						json: async () => ({
							items: url.includes('limit=1')
								? []
								: [{ ...subscription, source_revision: `source-r${listeningLoads}` }],
							total: 1
						})
					};
				}
				return {
					ok: true,
					status: 200,
					json: async () => ({ items: [], total: 0, catalog_revision: 'r', manifest_issues: [] })
				};
			})
		);

		const user = userEvent.setup();
		render(ObservableSourcesPanel);
		await screen.findByText('arXiv AI');
		await user.click(screen.getByRole('button', { name: 'Save' }));

		await waitFor(() => expect(listeningLoads).toBe(2));
		expect(screen.getByText('arXiv AI')).toBeInTheDocument();
	});
});
