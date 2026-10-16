import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import ObservableSourceObservability from './ObservableSourceObservability.svelte';
import type {
	ObservationRunRecord,
	ObservationSourceObservabilityPage,
	ObservationSourceObservabilitySummary
} from './sourceApi';

const run: ObservationRunRecord = {
	schema_version: 1,
	run_id: 'run-1',
	subscription_id: 'obs-1',
	source_id: 'arxiv-ai',
	profile_id: 'observe-rss',
	action_id: 'rss.discover',
	trigger: 'scheduled',
	status: 'succeeded',
	started_at_ms: 1_000,
	finished_at_ms: 2_500,
	duration_ms: 1_500,
	discovered: 12,
	deduped: 2,
	selected: 4,
	handed_off: 4,
	cursor_advanced: false,
	modified_targets: 1,
	not_modified_targets: 0,
	response_bytes: 8_192
};

const source: ObservationSourceObservabilitySummary = {
	subscription_id: 'obs-1',
	source_id: 'arxiv-ai',
	profile_id: 'observe-rss',
	display_name: 'arXiv AI',
	category: 'research',
	action_id: 'rss.discover',
	enabled: true,
	cadence: 'hourly',
	next_run_at_ms: Date.now() + 60_000,
	consecutive_failures: 0,
	runs: 3,
	succeeded: 3,
	failed: 0,
	cancelled: 0,
	candidates_discovered: 30,
	candidates_deduped: 5,
	candidates_selected: 8,
	enrichment_handoffs: 8,
	enrichment_processed: 7,
	enrichment_failed: 1,
	cursor_advances: 0,
	modified_targets: 2,
	not_modified_targets: 1,
	response_bytes: 16_384,
	total_latency_ms: 4_500,
	cost_microunits: {},
	last_run: run
};

function observabilityPage(
	items: ObservationSourceObservabilitySummary[] = [source],
	nextCursor: string | null = null
): ObservationSourceObservabilityPage {
	return {
		items,
		total: nextCursor ? 6 : items.length,
		next_cursor: nextCursor,
		totals: {
			subscriptions: items.length,
			enabled: items.length,
			healthy: items.length,
			degraded: 0,
			never_run: 0,
			runs: 3,
			succeeded: 3,
			failed: 0,
			cancelled: 0,
			candidates_discovered: 30,
			candidates_deduped: 5,
			candidates_selected: 8,
			enrichment_handoffs: 8,
			enrichment_processed: 7,
			enrichment_failed: 1,
			modified_targets: 2,
			not_modified_targets: 1,
			response_bytes: 16_384,
			total_latency_ms: 4_500,
			cost_microunits: {}
		},
		handoff_backlog: 2,
		failed_handoffs: 1,
		run_history_retained: 3,
		runtime_metrics: {
			offer_projections: 1,
			offers_eligible: 1,
			offers_needs_setup: 0,
			offers_unavailable: 0,
			runs_started: 1,
			runs_succeeded: 1,
			runs_failed: 0,
			runs_throttled: 0,
			leases_skipped: 0,
			policy_denials: 0,
			candidates_discovered: 12,
			candidates_deduped: 2,
			candidates_selected: 4,
			enrichment_handoffs: 4,
			enrichment_processed: 3,
			enrichment_failed: 1,
			cursor_advances: 0,
			modified_targets: 1,
			not_modified_targets: 0,
			response_bytes: 8_192,
			cost_microunits: {},
			total_latency_ms: 1_500
		}
	};
}

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('ObservableSourceObservability', () => {
	it('renders durable source health and opens server-paginated run history', async () => {
		const calls: string[] = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request) => {
				const url = String(request);
				calls.push(url);
				if (url.includes('/runs?')) {
					return { ok: true, status: 200, json: async () => ({ items: [run], total: 1 }) };
				}
				return { ok: true, status: 200, json: async () => observabilityPage() };
			})
		);

		const user = userEvent.setup();
		render(ObservableSourceObservability);
		expect(await screen.findByText('arXiv AI')).toBeInTheDocument();
		expect(screen.getByText('100%')).toBeInTheDocument();
		expect(screen.getByText('2', { selector: '.summary-strip strong' })).toBeInTheDocument();
		expect(screen.getByText('30')).toBeInTheDocument();
		expect(screen.getByLabelText('30 discovered, 8 selected, 7 processed')).toBeInTheDocument();
		expect(screen.getByText('1 unchanged')).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Show arXiv AI run history' }));
		await screen.findByRole('heading', { name: 'arXiv AI', level: 3 });
		expect(screen.getByText('12 → 4 → 4')).toBeInTheDocument();
		expect(calls.some((url) => url.includes('/observe/subscriptions/obs-1/runs?limit=5'))).toBe(true);
	});

	it('recovers stale summary cursors by returning to the first server page', async () => {
		const calls: string[] = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request) => {
				const url = String(request);
				calls.push(url);
				if (url.includes('cursor=stale')) {
					return {
						ok: false,
						status: 409,
						json: async () => ({ error: 'stale_cursor', message: 'restart pagination' })
					};
				}
				return { ok: true, status: 200, json: async () => observabilityPage([source], 'stale') };
			})
		);

		const user = userEvent.setup();
		render(ObservableSourceObservability);
		await screen.findByText('arXiv AI');
		await user.click(screen.getByRole('button', { name: 'Next sources' }));
		await waitFor(() => expect(calls.filter((url) => !url.includes('cursor=stale')).length).toBe(2));
		expect(screen.queryByText('restart pagination')).not.toBeInTheDocument();
		expect(screen.getByText('Page 1')).toBeInTheDocument();
	});
});
