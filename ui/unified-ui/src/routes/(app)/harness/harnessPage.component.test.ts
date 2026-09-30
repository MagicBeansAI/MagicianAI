import { render, screen, fireEvent } from '@testing-library/svelte';
import { beforeEach, describe, expect, it } from 'vitest';
import HarnessPage from './+page.svelte';
import { installFetchMock, jsonResponse } from '../../../test/browser';
import { scopeIdentityStore } from '../../../lib/stores/scopeIdentityStore';

beforeEach(() => {
	scopeIdentityStore.reset();
});

const mockAnomalies = [
	{
		signature: 'sig-cmo-001',
		agent_id: 'cmo',
		goal_id: 'harness:cmo:content-calendar',
		kind: 'cycle_failed',
		summary: 'Calendar generation failed due to timeout',
		detail: 'Connection to social platform timed out after 30s',
		status: 'open',
		occurrences: 3,
		first_seen: '2026-09-26T10:00:00Z',
		last_seen: '2026-09-26T14:30:00Z'
	},
	{
		signature: 'sig-cto-002',
		agent_id: 'cto',
		goal_id: 'harness:cto:morning-engineering-standup',
		kind: 'tool_unavailable',
		summary: 'Git provider tool credentials expired',
		detail: 'Auth token expired at 2026-09-26T12:00:00Z',
		status: 'fix_dispatched',
		occurrences: 1,
		first_seen: '2026-09-26T12:05:00Z',
		last_seen: '2026-09-26T12:05:00Z',
		fix_task_id: 'task-fix-99'
	}
];

describe('Harness Page component (harnessPage.component.test.ts)', () => {
	it('renders masthead, back-link to crew, company loop console, and anomaly KPIs', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/harness/anomalies',
				handle: () => jsonResponse({ anomalies: mockAnomalies })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/harness/runtime',
				handle: () =>
					jsonResponse({ enabled: true, paused: false, effective_source: 'config' })
			}
		]);

		render(HarnessPage);

		expect(await screen.findByText('Harness & Company Loop')).toBeInTheDocument();
		expect(screen.getByText('Back to Crew')).toBeInTheDocument();
		expect(screen.getByText('Autonomous Company Loop')).toBeInTheDocument();
		expect(screen.getByText('Run Company Loop')).toBeInTheDocument();

		// Check 5-stage stepper
		expect(screen.getByText('CMO')).toBeInTheDocument();
		expect(screen.getByText('CRO')).toBeInTheDocument();
		expect(screen.getByText('CPO')).toBeInTheDocument();
		expect(screen.getByText('CTO')).toBeInTheDocument();
		expect(screen.getByText('CEO')).toBeInTheDocument();

		// Check KPI cards
		expect(screen.getByText('Total Anomalies')).toBeInTheDocument();
		expect(screen.getByText('Open Issues')).toBeInTheDocument();
		expect(screen.getByText('Fix Dispatched')).toBeInTheDocument();
		expect(screen.getByText('Resolved / Dismissed')).toBeInTheDocument();

		// Check table rows
		expect(await screen.findByText('Calendar generation failed due to timeout')).toBeInTheDocument();
		expect(screen.getByText('Git provider tool credentials expired')).toBeInTheDocument();
	});

	it('toggles view mode between table and card grid', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/harness/anomalies',
				handle: () => jsonResponse({ anomalies: mockAnomalies })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/harness/runtime',
				handle: () =>
					jsonResponse({ enabled: true, paused: false, effective_source: 'config' })
			}
		]);

		render(HarnessPage);

		const cardsBtn = await screen.findByTitle('Card grid view');
		await fireEvent.click(cardsBtn);
		expect(cardsBtn).toHaveClass('view-toggle-btn--active');

		const tableBtn = screen.getByTitle('Table view');
		await fireEvent.click(tableBtn);
		expect(tableBtn).toHaveClass('view-toggle-btn--active');
	});

	it('filters anomalies by search query', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/harness/anomalies',
				handle: () => jsonResponse({ anomalies: mockAnomalies })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/harness/runtime',
				handle: () =>
					jsonResponse({ enabled: true, paused: false, effective_source: 'config' })
			}
		]);

		render(HarnessPage);

		expect(await screen.findByText('Calendar generation failed due to timeout')).toBeInTheDocument();
		const searchInput = await screen.findByPlaceholderText(/Search by agent, anomaly kind/i);
		await fireEvent.input(searchInput, { target: { value: 'social platform' } });

		expect(await screen.findByText('Calendar generation failed due to timeout')).toBeInTheDocument();
		expect(screen.queryByText('Git provider tool credentials expired')).not.toBeInTheDocument();
	});

	it('renders pagination control with ServerPager and page size dropdown', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/harness/anomalies',
				handle: () => jsonResponse({ anomalies: mockAnomalies })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/harness/runtime',
				handle: () =>
					jsonResponse({ enabled: true, paused: false, effective_source: 'config' })
			}
		]);

		render(HarnessPage);

		const pagerNav = await screen.findByRole('navigation', { name: 'Harness anomalies pagination' });
		expect(pagerNav).toBeInTheDocument();
		expect(screen.getByText(/Page 1 of 1/i)).toBeInTheDocument();
		expect(screen.getByText(/1-2 of 2/i)).toBeInTheDocument();

		const pageSizeSelect = screen.getByLabelText('Items per page');
		expect(pageSizeSelect).toBeInTheDocument();
		expect(pageSizeSelect).toHaveValue('10');
	});
});
