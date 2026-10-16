import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

import { notifications } from '$lib/shared/stores/notifications';
import { optimisticAttentionMutationQueue } from '$lib/attention/optimisticAttentionMutationQueue';
import { attentionRankRecomputeStore } from '$lib/stores/attentionRankRecomputeStore';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import ResurfacingBand from './ResurfacingBand.svelte';

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((next) => {
		resolve = next;
	});
	return { promise, resolve };
}

const brief = {
	schema_version: 2,
	key_facts: ['Affected card: Platinum'],
	changes: [
		{
			aspect: 'Monthly reward cap',
			before: '10,000 points',
			after: '5,000 points',
			effective_text: 'August 1, 2026'
		}
	],
	temporal_facts: [
		{
			kind: 'effective',
			text: 'August 1, 2026',
			at_ms: new Date('2027-08-01T09:00:00.000Z').getTime(),
			timezone: 'Asia/Kolkata'
		}
	],
	detail_status: 'source_omits_details',
	missing_details: ['Whether existing points are affected']
};

const actions = [
	{ kind: 'view_details', label: 'Details', requires_input: false, side_effect: 'none' },
	{ kind: 'open_source', label: 'Open source', requires_input: false, side_effect: 'none' },
	{ kind: 'show_original', label: 'Original', requires_input: false, side_effect: 'none' },
	{ kind: 'create_reminder', label: 'Create reminder', requires_input: true, side_effect: 'creates_reminder' },
	{ kind: 'ask_presto', label: 'Ask Presto', requires_input: true, side_effect: 'none' },
	{ kind: 'create_task', label: 'Create task', requires_input: true, side_effect: 'creates_task' },
	{ kind: 'share', label: 'Share', requires_input: true, side_effect: 'creates_share_draft' },
	{ kind: 'save_to_memory', label: 'Save to memory', requires_input: true, side_effect: 'creates_memory_candidate' },
	{ kind: 'summarize_deeper', label: 'Summarize deeper', requires_input: false, side_effect: 'none' }
];

function card(summary = 'The monthly reward cap drops from 10,000 to 5,000 points on August 1.') {
	return {
		candidate_id: 'cand-1',
		line: 'Your card policy has changed',
		why_now: 'The new cap becomes effective next month.',
		source_title: 'Platinum card policy update',
		summary,
		source_kind: 'comm',
		source_ref: 'comm:message-1',
		detail_label: 'Message summary',
		temporal_anchor_at: brief.temporal_facts[0].at_ms,
		brief,
		content_revision: 'rev-1',
		source_updated: false,
		recommended_action: {
			kind: 'create_reminder',
			label: 'Remind me before August 1',
			rationale: 'The policy has a known effective date.',
			confidence: 0.91,
			content_revision: 'rev-1',
			source: 'curator'
		},
		actions
	};
}

function pagePayload(summary?: string) {
	const scope = get(scopeIdentityStore);
	return {
		cards: [card(summary)],
		total: 1,
		limit: 5,
		offset: 0,
		has_more: false,
		next_cursor: null,
		cross_lane_reconciliation: {
			schema_version: 1,
			status: 'succeeded',
			reason: null,
			authoritative_lane: 'follow_up',
			principal: scope.principal,
			workspace: scope.workspace,
			follow_up_source_total: 1,
			worth_a_look_source_total: 1,
			raw_source_total: 1,
			visible_source_total: 1,
			duplicate_hidden_total: 0,
			raw_scanned_total: 1,
			visible_page_total: 1,
			duplicate_hidden_page_total: 0,
			reconciliation_digest: 'reconciliation-1'
		}
	};
}

function detailPayload(original: unknown = null) {
	return {
		candidate_id: 'cand-1',
		source_kind: 'comm',
		status: 'newer_available',
		title: 'Platinum card policy update',
		summary: card().summary,
		brief,
		content_revision: 'rev-1',
		source_revision: 'source-rev-2',
		source_updated: true,
		has_newer: true,
		source_route: null,
		open_url: 'https://mail.example.test/thread-1',
		source: {
			kind: 'comm',
			provider: 'gmail',
			account_alias: 'personal',
			account_email: 'owner@example.test',
			thread_id: 'thread-1',
			message_id: 'message-1',
			received_at: 1,
			evidence_message_ids: ['message-1']
		},
		recommended_action: card().recommended_action,
		actions,
		original,
		temporal_anchor_at: brief.temporal_facts[0].at_ms
	};
}

function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

afterEach(() => {
	cleanup();
	notifications.clear();
	optimisticAttentionMutationQueue.reset();
	attentionRankRecomputeStore.clear();
	scopeIdentityStore.reset();
	vi.unstubAllGlobals();
});

describe('ResurfacingBand rich interactions', () => {
	it('suppresses an unverified Worth response and shows an explicit system diagnostic', async () => {
		const scope = get(scopeIdentityStore);
		vi.stubGlobal('fetch', vi.fn(async () => json({
			...pagePayload('Must never render'),
			cross_lane_reconciliation: {
				schema_version: 1,
				status: 'unavailable',
				reason: 'follow_up_load_unavailable',
				authoritative_lane: 'follow_up',
				principal: scope.principal,
				workspace: scope.workspace,
				follow_up_source_total: null,
				worth_a_look_source_total: null,
				raw_source_total: null,
				visible_source_total: null,
				duplicate_hidden_total: null,
				raw_scanned_total: null,
				visible_page_total: null,
				duplicate_hidden_page_total: null,
				reconciliation_digest: null
			}
		})));
		render(ResurfacingBand, { showEmpty: true });
		expect(await screen.findByText('System reconciliation unavailable')).toBeInTheDocument();
		expect(screen.getByText(/uncertain Worth response was suppressed/)).toBeInTheDocument();
		expect(screen.queryByText('Must never render')).not.toBeInTheDocument();
	});

	it('retains only the last verified same-scope Worth list after reconciliation fails', async () => {
		let reads = 0;
		const scope = get(scopeIdentityStore);
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
			if (!String(input).includes('/resurfacing/today')) return json({ recorded: true });
			reads += 1;
			if (reads === 1) return json(pagePayload('Last verified Worth item.'));
			return json({
				...pagePayload('Unverified replacement.'),
				cross_lane_reconciliation: {
					schema_version: 1,
					status: 'unavailable',
					reason: 'follow_up_source_changed',
					authoritative_lane: 'follow_up',
					principal: scope.principal,
					workspace: scope.workspace,
					follow_up_source_total: null,
					worth_a_look_source_total: null,
					raw_source_total: null,
					visible_source_total: null,
					duplicate_hidden_total: null,
					raw_scanned_total: null,
					visible_page_total: null,
					duplicate_hidden_page_total: null,
					reconciliation_digest: null
				}
			});
		}));
		const { rerender } = render(ResurfacingBand, { showEmpty: true, page: 1 });
		expect(await screen.findByText('Last verified Worth item.')).toBeInTheDocument();
		await rerender({ showEmpty: true, page: 2 });
		expect(await screen.findByText(/last verified same-scope Worth list is retained/)).toBeInTheDocument();
		expect(screen.getByText('Last verified Worth item.')).toBeInTheDocument();
		expect(screen.queryByText('Unverified replacement.')).not.toBeInTheDocument();
	});

	it('shows the server-owned page-local hidden duplicate count without rendering an alias', async () => {
		const payload = pagePayload('Visible owner-safe Worth item.');
		payload.cross_lane_reconciliation.worth_a_look_source_total = 2;
		payload.cross_lane_reconciliation.raw_source_total = 2;
		payload.cross_lane_reconciliation.duplicate_hidden_total = 1;
		payload.cross_lane_reconciliation.raw_scanned_total = 2;
		payload.cross_lane_reconciliation.duplicate_hidden_page_total = 1;
		vi.stubGlobal('fetch', vi.fn(async () => json(payload)));
		render(ResurfacingBand, { showEmpty: true });
		expect(await screen.findByTestId('worth-cross-lane-reconciliation')).toHaveTextContent(
			'1 exact duplicate hidden on this page'
		);
		expect(screen.getByText('Visible owner-safe Worth item.')).toBeInTheDocument();
	});

	it('records recommendation presentation from the mounted primary action', async () => {
		const events: Array<Record<string, unknown>> = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload());
				if (url.endsWith('/recommendation-event')) {
					events.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
					return json({ recorded: true });
				}
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		await screen.findByRole('button', { name: 'Remind me before August 1' });
		await waitFor(() => expect(events).toHaveLength(1));
		expect(events[0]).toMatchObject({
			kind: 'create_reminder',
			content_revision: 'rev-1',
			event: 'presented'
		});
	});

	it('renders direct Useful / Acknowledge / Dismiss buttons outside the more-actions menu', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload());
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });
		// The ⋯ menu is closed; these must be direct buttons on the card.
		expect(await screen.findByRole('button', { name: 'Useful' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Acknowledge' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Dismiss' })).toBeInTheDocument();
		expect(screen.queryByRole('menuitem', { name: 'Acknowledge' })).not.toBeInTheDocument();
	});

	it('shows candidate health and rank movement while keeping accepted feedback quiet', async () => {
		const learnedPage = {
			...pagePayload(),
			semantic_ranking_enabled: true,
			actionability_mode: 'shadow',
			actionability_snapshot_id: 'snapshot-42',
			semantic_extraction_coverage: 0.82,
			actionability_scored_count: 82,
			actionability_fallback_count: 18,
			health: {
				total_active: 2014,
				source_family_counts: { comm: 1691, web: 323 },
				embedded_candidates: 1593,
				embedding_coverage: 0.7909,
				learned_rank_changes: 110
			},
			cards: [
				{
					...card(),
					baseline_rank: 4,
					learned_rank: 12,
					rank_delta: -8,
					learning_score: 0.21,
					actionability_probability: 0.84,
					actionability_explanation: {
						code: 'direct_owner_request',
						label: 'Direct request to you'
					},
					actionability_model_version: 'actionability-gbt-v1',
					actionability_snapshot_id: 'snapshot-42',
					semantic_feature_status: 'succeeded',
					actionability_score_status: 'scored',
					actionability_mode: 'shadow'
				}
			]
		};
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(learnedPage);
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				if (url.endsWith('/action')) {
					return json({
						feedback_receipt: {
							outcome_id: 'outcome-1',
							outcome: 'useful',
							surface: 'worth_a_look',
							feedback_recorded: true,
							affected_candidates: 18,
							rescore_status: 'completed',
							rank_recompute: null
						}
					});
				}
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		// The mode chip and the row's own rank facts stay visible while the health
		// strip is collapsed; the candidate totals and the ordering caveat live in
		// the detail, so this opens the disclosure before reading them.
		expect(await screen.findByText('Learned rank 12 · down 8 from baseline')).toBeInTheDocument();
		expect(screen.getByText('Actionability preview')).toBeInTheDocument();
		expect(
			screen.getByText('Actionability preview 84% · Direct request to you')
		).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: /System health/ }));
		expect(screen.getByText('2,014')).toBeInTheDocument();
		expect(screen.getByText(/preview-only and does not change ordering/)).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Useful' }));
		await waitFor(() => expect(get(attentionRankRecomputeStore).jobs).toHaveLength(1));
		expect(get(notifications).notifications).toEqual([]);
		expect(get(attentionRankRecomputeStore).jobs).toMatchObject([
			{ key: 'disabled:outcome-1', status: 'disabled' }
		]);
	});

	it('renders concrete row facts and lazily resolves detail and original content', async () => {
		const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.endsWith('/original')) {
				return json(
					detailPayload({
						kind: 'comm',
						message_id: 'message-1',
						subject: 'Policy update',
						summary: card().summary,
						received_at: 1,
						body: 'The reward cap becomes 5,000 points on August 1.',
						evidence_messages: []
					})
				);
			}
			if (url.endsWith('/detail')) return json(detailPayload());
			if (url.includes('/resurfacing/today')) return json(pagePayload());
			if (url.endsWith('/recommendation-event')) return json({ recorded: true });
			throw new Error(`unexpected request: ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);
		render(ResurfacingBand, { showEmpty: true });

		expect(await screen.findByText(card().summary)).toBeInTheDocument();
		expect(screen.getByText(/10,000 points to 5,000 points/)).toBeInTheDocument();
		expect(screen.getByText('Details missing from source')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Remind me before August 1' })).toBeInTheDocument();
		expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith('/detail'))).toBe(false);

		await fireEvent.click(screen.getByRole('button', { name: 'Details' }));
		const panel = await screen.findByTestId('resurfacing-detail-panel');
		expect(
			await within(panel).findByRole('heading', { name: 'What changed' })
		).toBeInTheDocument();
		expect(within(panel).getByText('A newer source item is available.')).toBeInTheDocument();
		expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith('/original'))).toBe(false);

		const originalButton = within(panel).getByRole('button', { name: 'Original' });
		await waitFor(() => expect(originalButton).toBeEnabled());
		await fireEvent.click(originalButton);
		expect(await within(panel).findByText(/reward cap becomes 5,000 points/)).toBeInTheDocument();
		expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith('/original'))).toBe(true);
	});

	it('supports roving keyboard focus and validates a focused action dialog', async () => {
		const user = userEvent.setup();
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload());
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				throw new Error(`unexpected request: ${String(input)}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		const more = await screen.findByRole('button', { name: 'More actions' });
		await fireEvent.keyDown(more, { key: 'ArrowDown' });
		const openSource = screen.getByRole('menuitem', { name: 'Open source' });
		expect(openSource).toHaveFocus();
		await fireEvent.keyDown(openSource, { key: 'End' });
		// The last menuitem is now the final dismiss-reason ("Spam"); the reason
		// items are real menuitems and join the roving Home/End navigation.
		expect(screen.getByRole('menuitem', { name: 'Spam' })).toHaveFocus();
		await fireEvent.keyDown(document.activeElement as HTMLElement, { key: 'Home' });
		expect(openSource).toHaveFocus();
		const createTask = screen.getByRole('menuitem', { name: 'Create task' });
		await user.click(createTask);

		const dialog = await screen.findByRole('dialog', { name: 'Create task' });
		const title = within(dialog).getByRole('textbox', { name: 'Title' });
		await user.clear(title);
		await user.click(within(dialog).getByRole('button', { name: 'Create task' }));
		expect(within(dialog).getByRole('alert')).toHaveTextContent('Title is required.');
		expect(screen.getByText(card().summary)).toBeInTheDocument();
	});

	it('closes the More-actions menu when clicking elsewhere on the page', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload());
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				throw new Error(`unexpected request: ${String(input)}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		const more = await screen.findByRole('button', { name: 'More actions' });
		await fireEvent.click(more);
		expect(await screen.findByRole('menuitem', { name: 'Open source' })).toBeInTheDocument();

		// Click somewhere unrelated on the page — the open menu should dismiss.
		await fireEvent.click(document.body);
		await waitFor(() =>
			expect(screen.queryByRole('menuitem', { name: 'Open source' })).not.toBeInTheDocument()
		);
	});

	it('retains the row and reuses its idempotency key after a failed side effect', async () => {
		const user = userEvent.setup();
		const actionBodies: Array<Record<string, unknown>> = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload());
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				if (url.endsWith('/actions')) {
					actionBodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
					return json({ error: 'scheduler unavailable' }, 503);
				}
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		await user.click(await screen.findByRole('button', { name: 'More actions' }));
		await user.click(screen.getByRole('menuitem', { name: 'Create task' }));
		const dialog = await screen.findByRole('dialog', { name: 'Create task' });
		const submit = within(dialog).getByRole('button', { name: 'Create task' });
		await user.click(submit);
		expect(await within(dialog).findByRole('alert')).toHaveTextContent('scheduler unavailable');
		expect(screen.getByText(card().summary)).toBeInTheDocument();
		await user.click(submit);
		await waitFor(() => expect(actionBodies).toHaveLength(2));
		expect(actionBodies[0].idempotency_key).toBe(actionBodies[1].idempotency_key);
	});

	it('creates an Apple Reminder through the host delivery without a task fallback', async () => {
		const user = userEvent.setup();
		let actionBody: Record<string, unknown> | null = null;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload());
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				if (url.endsWith('/actions')) {
					actionBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
					return json({
						candidate_id: 'candidate-1',
						action: 'create_reminder',
						result_ref: 'reminder_resurfacing_1',
						replayed: false,
						result: {
							kind: 'reminder',
							reminder_id: 'apple-reminder-1',
							provider: 'apple_reminders_macos',
							at: '2099-08-01T03:30:00Z',
							timezone: 'Asia/Kolkata'
						}
					});
				}
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		await user.click(await screen.findByRole('button', { name: 'Remind me before August 1' }));
		const dialog = await screen.findByRole('dialog', { name: 'Create in Apple Reminders' });
		await fireEvent.input(within(dialog).getByLabelText('Date and time'), {
			target: { value: '2099-08-01T09:00' }
		});
		await user.click(within(dialog).getByRole('button', { name: 'Create in Apple Reminders' }));

		await waitFor(() => expect(actionBody).not.toBeNull());
		expect(actionBody).toMatchObject({
			kind: 'create_reminder',
			input: {
				delivery: 'host_apple_reminders',
				title: expect.any(String),
				instruction: expect.any(String)
			}
		});
		expect(JSON.stringify(actionBody)).not.toContain('task_id');
	});

	it('refreshes a stale read recommendation before opening current detail', async () => {
		let listReads = 0;
		const staleCard = {
			...card('Old policy summary.'),
			recommended_action: {
				kind: 'view_details',
				label: 'Review current terms',
				rationale: 'The source may affect your card.',
				confidence: 0.92,
				content_revision: 'rev-old',
				source: 'curator'
			}
		};
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) {
					listReads += 1;
					return json({
						...pagePayload(),
						cards: [
							listReads === 1
								? staleCard
								: {
									...staleCard,
									summary: 'Current policy summary with the revised 5,000 point cap.',
									content_revision: 'rev-current',
									recommended_action: null
								}
						]
					});
				}
				if (url.endsWith('/recommendation-event')) {
					const body = JSON.parse(String(init?.body)) as { event?: string };
					if (body.event === 'presented') return json({ recorded: true });
					return json({ error: 'the recommendation is stale', error_code: 'stale_revision' }, 409);
				}
				if (url.endsWith('/detail')) {
					return json({
						...detailPayload(),
						summary: 'Current policy summary with the revised 5,000 point cap.',
						content_revision: 'rev-current',
						recommended_action: null
					});
				}
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		await fireEvent.click(await screen.findByRole('button', { name: 'Review current terms' }));
		expect(
			await screen.findByText('Current policy summary with the revised 5,000 point cap.')
		).toBeInTheDocument();
		expect(await screen.findByTestId('resurfacing-detail-panel')).toBeInTheDocument();
		expect(listReads).toBe(2);
	});

	it('restores only the failed card after explicit feedback fails', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload());
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				if (url.endsWith('/action')) return json({ error: 'feedback store unavailable' }, 503);
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		await fireEvent.click(await screen.findByRole('button', { name: 'Dismiss' }));
		expect(await screen.findByText(card().summary)).toBeInTheDocument();
		expect(get(notifications).notifications).toMatchObject([
			{ type: 'error', title: "Couldn't record that: feedback store unavailable" }
		]);
	});

	it('never restores a failed feedback card after the scope changes', async () => {
		const feedback = deferred<Response>();
		let listReads = 0;
		scopeIdentityStore.observe('owner-a', 'workspace-a');
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) {
					listReads += 1;
					return json(pagePayload(listReads === 1 ? 'Old scope summary.' : 'New scope summary.'));
				}
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				if (url.endsWith('/action')) return feedback.promise;
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		await fireEvent.click(await screen.findByRole('button', { name: 'Dismiss' }));
		scopeIdentityStore.observe('owner-b', 'workspace-b');
		expect(await screen.findByText('New scope summary.')).toBeInTheDocument();
		feedback.resolve(json({ error: 'feedback store unavailable' }, 503));
		await waitFor(() => expect(listReads).toBe(2));
		expect(screen.queryByText('Old scope summary.')).not.toBeInTheDocument();
	});

	it('replaces list presentation with authoritative suppressed detail', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload('Sensitive prior summary.'));
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				if (url.endsWith('/detail')) {
					return json({
						...detailPayload(),
						status: 'suppressed',
						title: null,
						summary: null,
						brief: null,
						content_revision: null,
						recommended_action: null,
						actions: [],
						open_url: null
					});
				}
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		await fireEvent.click(await screen.findByRole('button', { name: 'Details' }));
		const panel = await screen.findByTestId('resurfacing-detail-panel');
		expect(await within(panel).findByText('This source is restricted by content policy.')).toBeInTheDocument();
		expect(screen.queryByText('Sensitive prior summary.')).not.toBeInTheDocument();
		expect(screen.queryByText('Platinum card policy update')).not.toBeInTheDocument();
		expect(within(panel).queryByRole('button', { name: 'Original' })).not.toBeInTheDocument();
		expect(within(panel).queryByRole('button', { name: 'Open source' })).not.toBeInTheDocument();
	});

	it('does not let a stale Original response cancel or overwrite a new detail selection', async () => {
		const original = deferred<Response>();
		const secondDetail = deferred<Response>();
		const secondCard = {
			...card('Second list summary.'),
			candidate_id: 'cand-2',
			source_title: 'Second policy update',
			content_revision: 'rev-2'
		};
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) {
					const payload = pagePayload();
					return json({
						...payload,
						cards: [card('First list summary.'), secondCard],
						total: 2,
						cross_lane_reconciliation: {
							...payload.cross_lane_reconciliation,
							worth_a_look_source_total: 2,
							raw_source_total: 2,
							visible_source_total: 2,
							raw_scanned_total: 2,
							visible_page_total: 2
						}
					});
				}
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				if (url.includes('/cand-1/detail')) return json(detailPayload());
				if (url.includes('/cand-1/original')) return original.promise;
				if (url.includes('/cand-2/detail')) return secondDetail.promise;
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		const firstRow = (await screen.findByText('First list summary.')).closest('tr');
		expect(firstRow).not.toBeNull();
		await fireEvent.click(within(firstRow as HTMLElement).getByRole('button', { name: 'Details' }));
		const firstPanel = await screen.findByTestId('resurfacing-detail-panel');
		await fireEvent.click(await within(firstPanel).findByRole('button', { name: 'Original' }));

		const secondRow = screen.getByText('Second list summary.').closest('tr');
		expect(secondRow).not.toBeNull();
		await fireEvent.click(within(secondRow as HTMLElement).getByRole('button', { name: 'Details' }));
		original.resolve(
			json(
				detailPayload({
					kind: 'comm',
					message_id: 'message-1',
					subject: 'Stale original',
					summary: null,
					received_at: 1,
					body: 'STALE ORIGINAL BODY',
					evidence_messages: []
				})
			)
		);
		secondDetail.resolve(
			json({
				...detailPayload(),
				candidate_id: 'cand-2',
				title: 'Second current detail',
				summary: 'Second current summary.',
				content_revision: 'rev-2',
				recommended_action: null,
				actions: []
			})
		);

		const currentPanel = await screen.findByTestId('resurfacing-detail-panel');
		expect(await within(currentPanel).findByText('Second current summary.')).toBeInTheDocument();
		expect(screen.queryByText('STALE ORIGINAL BODY')).not.toBeInTheDocument();
	});

	it('reuses one idempotency key for direct deeper-summary in-progress retries', async () => {
		const actionBodies: Array<Record<string, unknown>> = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
				const url = String(input);
				if (url.includes('/resurfacing/today')) return json(pagePayload());
				if (url.endsWith('/recommendation-event')) return json({ recorded: true });
				if (url.endsWith('/detail')) return json(detailPayload());
				if (url.endsWith('/actions')) {
					actionBodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
					if (actionBodies.length === 1) {
						return json({ error: 'still running', error_code: 'in_progress' }, 409);
					}
					return json({
						candidate_id: 'cand-1',
						action: 'summarize_deeper',
						result_ref: 'summary-1',
						replayed: true,
						result: {
							kind: 'deeper_summary',
							summary: 'Current deeper summary.',
							key_points: [],
							recommended_actions: [],
							caveats: []
						}
					});
				}
				throw new Error(`unexpected request: ${url}`);
			})
		);
		render(ResurfacingBand, { showEmpty: true });

		await fireEvent.click(await screen.findByRole('button', { name: 'More actions' }));
		await fireEvent.click(screen.getByRole('menuitem', { name: 'Summarize deeper' }));
		await waitFor(() => expect(actionBodies).toHaveLength(1));
		await fireEvent.click(screen.getByRole('button', { name: 'More actions' }));
		await fireEvent.click(screen.getByRole('menuitem', { name: 'Summarize deeper' }));
		expect(await screen.findByText('Current deeper summary.')).toBeInTheDocument();
		expect(actionBodies[0].idempotency_key).toBe(actionBodies[1].idempotency_key);
	});

	it('loads a nonadjacent page with one bounded offset request', async () => {
		const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
			const payload = pagePayload();
			return json({
				...payload,
				cards: [{ ...card(), recommended_action: null }],
				total: 100,
				offset: 95,
				has_more: false,
				cross_lane_reconciliation: {
					...payload.cross_lane_reconciliation,
					worth_a_look_source_total: 100,
					raw_source_total: 100,
					visible_source_total: 100
				}
			});
		});
		vi.stubGlobal('fetch', fetchMock);
		render(ResurfacingBand, { showEmpty: true, page: 20 });
		await screen.findByText(card().summary);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(String(fetchMock.mock.calls[0][0])).toContain('offset=95');
	});
});
