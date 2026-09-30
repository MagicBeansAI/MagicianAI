import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { beforeEach, describe, expect, it } from 'vitest';
import { notifications } from '../../../lib/shared/stores/notifications';
import TodayPage from './+page.svelte';
import { installFetchMock, jsonResponse } from '../../../test/browser';
import { scopeIdentityStore } from '../../../lib/stores/scopeIdentityStore';

beforeEach(() => {
	scopeIdentityStore.reset();
});

function mockResurfacingResponse(cards: any[] = []) {
	return {
		cards,
		total: cards.length,
		limit: 5,
		offset: 0,
		has_more: false,
		next_cursor: null,
		cross_lane_reconciliation: {
			schema_version: 1,
			status: 'succeeded',
			reason: null,
			authoritative_lane: 'follow_up',
			principal: 'test-user',
			workspace: 'test-ws',
			follow_up_source_total: 0,
			worth_a_look_source_total: cards.length,
			raw_source_total: cards.length,
			visible_source_total: cards.length,
			duplicate_hidden_total: 0,
			raw_scanned_total: cards.length,
			visible_page_total: cards.length,
			duplicate_hidden_page_total: 0,
			reconciliation_digest: 'rec-test'
		}
	};
}

describe("Today's Morning Edition page (todayPage.component.test.ts)", () => {
	it('renders the morning newspaper masthead and serene clear state', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/channel-assist/follow-ups',
				handle: () => jsonResponse({ items: [], total: 0, ok: true })
			},
			{
				method: 'GET',
				match: '/resurfacing/today',
				handle: () => jsonResponse(mockResurfacingResponse([]))
			},
			{
				method: 'GET',
				match: '/surfaces/published',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/today',
				handle: () => jsonResponse({
					counts: { needs_you: 0, delivered: 0, changed: 0, active_work: 0, followups: 0 },
					sections: { needs_you: [], delivered: [], changed: [], active_work: [], followups: [] },
					digest: { bullets: [], total: 0 }
				})
			}
		]);

		render(TodayPage);

		expect(await screen.findByRole('heading', { level: 1, name: "Today's" })).toBeInTheDocument();
		expect(screen.getByText(/VOL\.\s+[IVXLCDM]+\s+·\s+NO\.\s+\d+/i)).toBeInTheDocument();
		expect(screen.getByText(/Here is your brief\./i)).toBeInTheDocument();
		expect(screen.getByText('The Slate is Clear')).toBeInTheDocument();
		expect(screen.getByText('Reading Room')).toBeInTheDocument();
		expect(screen.getByText('For You')).toBeInTheDocument();
		expect(screen.getByText(/Worth a look/i)).toBeInTheDocument();
		expect(screen.getByText('Special Reports & Briefings')).toBeInTheDocument();
		expect(screen.getByText('View all briefings')).toBeInTheDocument();
	});

	it('renders completed deliverables as news cards when present in today store', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/channel-assist/follow-ups',
				handle: () => jsonResponse({ items: [], total: 0, ok: true })
			},
			{
				method: 'GET',
				match: '/resurfacing/today',
				handle: () => jsonResponse(mockResurfacingResponse([]))
			},
			{
				method: 'GET',
				match: '/surfaces/published',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/today',
				handle: () => jsonResponse({
					principal: 'test-user',
					workspace: 'test-ws',
					generated_at: Date.now(),
					freshness: { source: 'test', generated_at: Date.now() },
					headline: 'All systems running smoothly',
					counts: { needs_you: 0, delivered: 1, changed: 0, active_work: 0, followups: 0, total: 1 },
					sections: {
						needs_you: [],
						delivered: [
							{
								id: 'del-1',
								principal: 'test-user',
								workspace: 'test-ws',
								section: 'delivered',
								priority: 1,
								title: 'Architecture Blueprint Signed Off',
								summary: 'Full executive dossier filed and ready for review.',
								reason: 'Completed milestone',
								source_kind: 'task',
								source_id: 'task-101',
								task_id: 'task-101',
								status: 'completed',
								actions: [],
								evidence_refs: [],
								created_at: Date.now() - 3600000,
								updated_at: Date.now() - 1800000,
								metadata: {}
							}
						],
						changed: [],
						active_work: [],
						followups: []
					},
					digest: {
						bullets: [
							{
								id: 'bul-1',
								text: 'Updated authentication security policy',
								source_kind: 'security',
								source_id: 'sec-1',
								source_url: '/settings',
								space_ids: ['global'],
								updated_at: Date.now()
							}
						],
						total: 1,
						limit: 6,
						offset: 0,
						generated_at: Date.now()
					}
				})
			}
		]);

		render(TodayPage);

		expect(await screen.findByRole('heading', { level: 1, name: "Today's" })).toBeInTheDocument();
		expect(await screen.findByText('Completed Deliverables')).toBeInTheDocument();
		expect(screen.getByText('Architecture Blueprint Signed Off')).toBeInTheDocument();
		expect(screen.getByText('Full executive dossier filed and ready for review.')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /^Inspect$/i })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Acknowledge/i })).toBeInTheDocument();

		// Chronicle interactive bullet
		expect(screen.getByText('The Chronicle & Digest')).toBeInTheDocument();
		expect(screen.getByText('Updated authentication security policy')).toBeInTheDocument();
		expect(screen.getByTitle(/Click to view underlying source/i)).toBeInTheDocument();
	});

	it('allows toggling between Broadsheet and Morning Deck views', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/channel-assist/follow-ups',
				handle: () => jsonResponse({ items: [], total: 0, ok: true })
			},
			{
				method: 'GET',
				match: '/resurfacing/today',
				handle: () => jsonResponse(mockResurfacingResponse([]))
			},
			{
				method: 'GET',
				match: '/surfaces/published',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/today',
				handle: () => jsonResponse({
					counts: { needs_you: 0, delivered: 0, changed: 0, active_work: 0, followups: 0 },
					sections: { needs_you: [], delivered: [], changed: [], active_work: [], followups: [] },
					digest: { bullets: [], total: 0 }
				})
			}
		]);

		render(TodayPage);

		// Default view is Morning Brief (swipe deck)
		expect(screen.getByRole('tab', { name: /All/i })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /For You/i })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /Worth a Look/i })).toBeInTheDocument();

		const broadsheetBtn = await screen.findByRole('radio', { name: /Broadsheet/ });
		await fireEvent.click(broadsheetBtn);

		expect(screen.getByRole('heading', { level: 2, name: 'For You' })).toBeInTheDocument();
		expect(screen.getByRole('heading', { level: 2, name: 'Worth a look' })).toBeInTheDocument();

		const deckBtn = await screen.findByRole('radio', { name: /Morning Brief/ });
		await fireEvent.click(deckBtn);

		expect(screen.queryByRole('heading', { level: 2, name: 'For You' })).not.toBeInTheDocument();
		expect(screen.queryByRole('heading', { level: 2, name: 'Worth a look' })).not.toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /All/i })).toBeInTheDocument();
	});

	it('renders The Daily Index (Economics of Operations) on the front page', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/channel-assist/follow-ups',
				handle: () => jsonResponse({ items: [], total: 0, ok: true })
			},
			{
				method: 'GET',
				match: '/resurfacing/today',
				handle: () => jsonResponse(mockResurfacingResponse([]))
			},
			{
				method: 'GET',
				match: '/surfaces/published',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/today',
				handle: () => jsonResponse({
					counts: { needs_you: 0, delivered: 0, changed: 0, active_work: 0, followups: 0 },
					sections: { needs_you: [], delivered: [], changed: [], active_work: [], followups: [] },
					digest: { bullets: [], total: 0 }
				})
			}
		]);

		render(TodayPage);

		expect(await screen.findByRole('heading', { level: 2, name: 'Economics of Operations' })).toBeInTheDocument();
		expect(screen.getByText('Continuous real-time accounting')).toBeInTheDocument();
		expect(screen.getByText('COMMERCIAL MODEL EXPENDITURES')).toBeInTheDocument();

		// Fleet / task state lives on the carousel's second slide.
		await fireEvent.click(screen.getByRole('button', { name: 'Slide 2 of 3, State of Operations' }));
		expect(screen.getByRole('heading', { level: 2, name: 'State of Operations' })).toBeInTheDocument();
		expect(screen.getByRole('link', { name: 'Tasks' })).toHaveAttribute('href', '/tasks');
		expect(screen.getByRole('link', { name: 'Agent Crew' })).toHaveAttribute('href', '/crew');
	});

	it('renders press room telemetry and bandit health classifier when debug mode is toggled', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/channel-assist/follow-ups',
				handle: () => jsonResponse({
					items: [],
					total: 0,
					ok: true,
					bandit: {
						mode: 'canary',
						policy_snapshot_id: 'pol-1',
						exploration_rate: 0.1,
						propensity_coverage: 0.95,
						support_ok: true,
						degradation_reason: null
					}
				})
			},
			{
				method: 'GET',
				match: '/resurfacing/today',
				handle: () => jsonResponse(mockResurfacingResponse([]))
			},
			{
				method: 'GET',
				match: '/surfaces/published',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/today',
				handle: () => jsonResponse({
					counts: { needs_you: 0, delivered: 0, changed: 0, active_work: 0, followups: 0 },
					sections: { needs_you: [], delivered: [], changed: [], active_work: [], followups: [] },
					digest: { bullets: [], total: 0 }
				})
			}
		]);

		render(TodayPage);

		// Click the debug toggle button in the masthead
		const debugBtn = screen.getByRole('button', { name: 'Debug' });
		await fireEvent.click(debugBtn);

		// Telemetry tray should now be visible with Bandit Health header
		expect(screen.getByText('Press Room Telemetry & Learning Models')).toBeInTheDocument();
		expect(screen.getByText('Bandit Health & Routing Classifier')).toBeInTheDocument();
	});

	it('renders AppSlotRegion on the morning edition', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/channel-assist/follow-ups',
				handle: () => jsonResponse({ items: [], total: 0, ok: true })
			},
			{
				method: 'GET',
				match: '/resurfacing/today',
				handle: () => jsonResponse(mockResurfacingResponse([]))
			},
			{
				method: 'GET',
				match: '/surfaces/published',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/today',
				handle: () => jsonResponse({
					counts: { needs_you: 0, delivered: 0, changed: 0, active_work: 0, followups: 0 },
					sections: { needs_you: [], delivered: [], changed: [], active_work: [], followups: [] },
					digest: { bullets: [], total: 0 }
				})
			},
			{
				method: 'POST',
				match: '/api/magician/v2/apps/slots/resolve-batch',
				handle: () => jsonResponse({
					assignments: [
						{ slot_id: 'page:2f:primary', pinned_system_default: true, opted_out: false },
						{ slot_id: 'page:2f:secondary', pinned_system_default: true, opted_out: false }
					]
				})
			}
		]);

		render(TodayPage);

		expect(await screen.findByRole('region', { name: 'Morning edition app widgets' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Add a widget to primary' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Add a widget to secondary' })).toBeInTheDocument();
	});

	it('renders and interacts with the hidden / snoozed items restore drawer', async () => {
		let restoreCalled = false;
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/channel-assist/follow-ups',
				handle: () => jsonResponse({ items: [], total: 0, ok: true })
			},
			{
				method: 'GET',
				match: '/resurfacing/today',
				handle: () => jsonResponse(mockResurfacingResponse([]))
			},
			{
				method: 'GET',
				match: '/surfaces/published',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/today/visibility',
				handle: () => jsonResponse({
					items: [
						{
							item_id: 'snoozed-item-1',
							hidden_kind: 'snoozed',
							record: {
								updated_at: 1000,
								snoozed_until: Date.now() + 3600_000,
								snapshot: {
									title: 'Deferred Architecture Memo',
									summary: 'Postponed until afternoon press briefing',
									reason: 'Waiting on staging validation',
									section: 'needs_you',
									source_kind: 'agent_message',
									source_id: 'msg-1',
									space_ids: [],
									item_updated_at: 1000
								}
							}
						}
					]
				})
			},
			{
				method: 'GET',
				match: '/api/magician/v2/today',
				handle: () => jsonResponse({
					counts: { needs_you: 0, delivered: 0, changed: 0, active_work: 0, followups: 0 },
					sections: { needs_you: [], delivered: [], changed: [], active_work: [], followups: [] },
					digest: { bullets: [], total: 0 }
				})
			},
			{
				method: 'POST',
				match: '/api/magician/v2/today/items/snoozed-item-1/visibility',
				handle: () => {
					restoreCalled = true;
					return jsonResponse({ ok: true });
				}
			}
		]);

		render(TodayPage);

		// Toggle button with count should appear
		const toggleBtn = await screen.findByRole('button', { name: /1 hidden/i });
		expect(toggleBtn).toBeInTheDocument();

		// Content should be hidden initially
		expect(screen.queryByText('Deferred Architecture Memo')).not.toBeInTheDocument();

		// Click to expand drawer
		await fireEvent.click(toggleBtn);

		// Now item details should be visible
		expect(screen.getByText('Deferred Architecture Memo')).toBeInTheDocument();
		expect(screen.getByText(/Thread · Needs You/)).toBeInTheDocument();
		expect(screen.getByText('Postponed until afternoon press briefing')).toBeInTheDocument();

		// Find and click the Restore button
		const restoreBtn = screen.getByRole('button', { name: 'Restore' });
		await fireEvent.click(restoreBtn);

		expect(restoreCalled).toBe(true);
	});

	it('reports a failed follow-up action as an error and restores the card', async () => {
		const followUp = {
			annotation_id: 'fu-fail',
			provider: 'gmail',
			sender: 'Ops Desk',
			subject: 'Approve the vendor renewal',
			summary: 'Renewal lapses Friday.',
			reason: 'Deadline',
			available_actions: []
		};
		let usefulCalls = 0;
		installFetchMock([
			{ method: 'GET', match: '/api/magician/v2/agents', handle: () => jsonResponse([]) },
			{
				method: 'GET',
				match: '/channel-assist/follow-ups',
				handle: () => jsonResponse({ items: [followUp], total: 1, ok: true })
			},
			{
				method: 'GET',
				match: '/resurfacing/today',
				handle: () => jsonResponse(mockResurfacingResponse([]))
			},
			{ method: 'GET', match: '/surfaces/published', handle: () => jsonResponse([]) },
			{
				method: 'GET',
				match: '/api/magician/v2/today',
				handle: () => jsonResponse({
					counts: { needs_you: 0, delivered: 0, changed: 0, active_work: 0, followups: 0 },
					sections: { needs_you: [], delivered: [], changed: [], active_work: [], followups: [] },
					digest: { bullets: [], total: 0 }
				})
			},
			{
				method: 'POST',
				match: '/channel-assist/annotations/fu-fail/useful',
				handle: () => {
					usefulCalls += 1;
					return jsonResponse({ error: 'annotation is not awaiting approval' }, { status: 409 });
				}
			}
		]);

		render(TodayPage);
		await fireEvent.click(await screen.findByRole('radio', { name: /Broadsheet/ }));
		expect(await screen.findByText('Approve the vendor renewal')).toBeInTheDocument();

		const clickedAt = Date.now();
		await fireEvent.click(screen.getByRole('button', { name: /Useful/ }));

		await waitFor(() => expect(usefulCalls).toBe(1));
		await waitFor(() => {
			const shown = get(notifications).notifications.filter((n) => n.timestamp >= clickedAt);
			expect(shown.some((n) => n.type === 'error' && /not awaiting approval/.test(`${n.title} ${n.message ?? ''}`))).toBe(true);
			expect(shown.some((n) => n.type === 'success')).toBe(false);
		});
		// The reload after a failure brings the card back.
		expect(await screen.findByText('Approve the vendor renewal')).toBeInTheDocument();
	});
});
