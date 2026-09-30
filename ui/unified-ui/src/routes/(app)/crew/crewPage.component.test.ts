import { render, screen, fireEvent } from '@testing-library/svelte';
import { beforeEach, describe, expect, it } from 'vitest';
import CrewPage from './+page.svelte';
import { installFetchMock, jsonResponse } from '../../../test/browser';
import { scopeIdentityStore } from '../../../lib/stores/scopeIdentityStore';

beforeEach(() => {
	scopeIdentityStore.reset();
});

const mockAgents = [
	{
		definition: {
			agent_id: 'coder-bot',
			name: 'Coder Bot',
			description: 'Writes code and runs tests',
			kind: 'Worker',
			tools: ['bash', 'editor'],
			delegation_targets: ['reviewer-bot'],
			disabled: false
		},
		version: 1,
		etag: 'etag-1',
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-01T00:00:00Z',
		status: 'idle'
	},
	{
		definition: {
			agent_id: 'lead-steward',
			name: 'Lead Steward',
			description: 'Oversees architecture and team delegation',
			kind: 'Personal',
			tools: ['memory', 'chat'],
			delegation_targets: ['coder-bot'],
			disabled: false
		},
		version: 1,
		etag: 'etag-2',
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-01T00:00:00Z',
		status: 'running'
	}
];

const mockSystemAgents = [
	{
		agent_id: 'intent-classifier',
		name: 'Intent Classifier',
		description: 'Classifies user intent for routing'
	}
];

describe('Crew Page component (crewPage.component.test.ts)', () => {
	it('renders masthead, KPI ribbon, and view tabs', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse({ agents: mockAgents, system_agents: mockSystemAgents, total_count: mockAgents.length })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/system-agents',
				handle: () => jsonResponse(mockSystemAgents)
			},
			{
				method: 'GET',
				match: '/api/magician/v2/harness/status',
				handle: () => jsonResponse({ enabled: true, effective_source: 'config' })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/agents/health',
				handle: () => jsonResponse({ agents: {} })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/square/fleet-state',
				handle: () => jsonResponse({ citizens: [] })
			}
		]);

		render(CrewPage);

		expect(await screen.findByText('Crew Operations')).toBeInTheDocument();
		expect(screen.getByText('Create Crew Member')).toBeInTheDocument();
		expect(screen.getByText('Total Fleet')).toBeInTheDocument();
		expect(screen.getByText('Active Now')).toBeInTheDocument();
		expect(screen.getByText('Needs Attention')).toBeInTheDocument();
		expect(screen.getByRole('region', { name: 'Autonomous Company Loop Control' })).toBeInTheDocument();
		expect(screen.getByRole('heading', { name: 'Company Loop', level: 2 })).toBeInTheDocument();
		expect(screen.getByRole('switch', { name: /autonomous company loop/i })).toBeInTheDocument();
		expect(screen.getByRole('link', { name: /Harness Console/i })).toBeInTheDocument();

		// Check navigation tabs
		expect(screen.getByRole('button', { name: /Crew Members/i })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Leaderboard & 7d/i })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Delegation Hierarchy/i })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Pipeline Roles/i })).toBeInTheDocument();
	});

	it('switches tabs to Delegation Hierarchy and Pipeline Roles', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse({ agents: mockAgents, system_agents: mockSystemAgents, total_count: mockAgents.length })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/system-agents',
				handle: () => jsonResponse(mockSystemAgents)
			},
			{
				method: 'GET',
				match: '/api/magician/v2/harness/status',
				handle: () => jsonResponse({ enabled: true })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/agents/health',
				handle: () => jsonResponse({ agents: {} })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/square/fleet-state',
				handle: () => jsonResponse({ citizens: [] })
			}
		]);

		render(CrewPage);

		// Switch to Pipeline Roles
		const pipelineTab = await screen.findByRole('button', { name: /Pipeline Roles/i });
		await fireEvent.click(pipelineTab);

		expect(await screen.findByRole('heading', { name: 'Internal Pipeline Agents', level: 2 })).toBeInTheDocument();

		// Switch to Delegation Hierarchy
		const hierarchyTab = screen.getByRole('button', { name: /Delegation Hierarchy/i });
		await fireEvent.click(hierarchyTab);

		expect(await screen.findByRole('heading', { name: 'Delegation Hierarchy', level: 2 })).toBeInTheDocument();
	});

	it('toggles view mode between table and grid on Crew Members tab', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse({ agents: mockAgents, system_agents: mockSystemAgents, total_count: mockAgents.length })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/system-agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/harness/status',
				handle: () => jsonResponse({ enabled: true })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/agents/health',
				handle: () => jsonResponse({ agents: {} })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/square/fleet-state',
				handle: () => jsonResponse({ citizens: [] })
			}
		]);

		render(CrewPage);

		// Default is table view
		const gridBtn = await screen.findByTitle('Card grid view');
		await fireEvent.click(gridBtn);

		const tableBtn = screen.getByTitle('Table list view');
		await fireEvent.click(tableBtn);
		expect(tableBtn).toHaveClass('view-toggle-btn--active');
	});

	it('renders pagination bar with ServerPager and page size options', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse({ agents: mockAgents, system_agents: mockSystemAgents, total_count: mockAgents.length })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/system-agents',
				handle: () => jsonResponse([])
			},
			{
				method: 'GET',
				match: '/api/magician/v2/harness/status',
				handle: () => jsonResponse({ enabled: true })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/agents/health',
				handle: () => jsonResponse({ agents: {} })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/square/fleet-state',
				handle: () => jsonResponse({ citizens: [] })
			}
		]);

		render(CrewPage);

		// Pager navigation is rendered
		const pagerNav = await screen.findByRole('navigation', { name: 'Crew members pagination' });
		expect(pagerNav).toBeInTheDocument();
		expect(screen.getByText(/Page 1 of 1/i)).toBeInTheDocument();
		expect(screen.getByText(/1-2 of 2/i)).toBeInTheDocument();

		// Page size select is rendered
		const pageSizeSelect = screen.getByLabelText('Items per page');
		expect(pageSizeSelect).toBeInTheDocument();
		expect(pageSizeSelect).toHaveValue('12');
	});
});
