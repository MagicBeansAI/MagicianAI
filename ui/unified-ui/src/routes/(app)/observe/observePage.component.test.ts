import { fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import { beforeEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../../test/browser';
import ObservePage from './+page.svelte';

const catchUp = {
	status: {
		policy: {
			schema_version: 1,
			revision: 1,
			enabled: true,
			lookback_days: 7,
			max_items_per_source: 50,
			max_total_items: 200,
			max_duration_minutes: 15
		},
		phase: 'waiting',
		boot_id: 'boot',
		boot_started_at_ms: 0,
		admitted_items: 0,
		processed_items: 1,
		reserved_items: 0,
		remaining_items: 199,
		sources: []
	},
	options: {
		lookback_days: [1, 7],
		max_items_per_source: [50],
		max_total_items: [200],
		max_duration_minutes: [15]
	}
};

beforeEach(() => {
	installFetchMock([
		{
			match: '/api/magician/v2/observe/catch-up',
			handle: () => jsonResponse(catchUp)
		},
		{
			match: '/api/magician/v2/observe/sources',
			handle: () => jsonResponse({ items: [], total: 0 })
		},
		{
			match: /.*/,
			handle: () => jsonResponse({ items: [], total: 0, events: [], errors: [], channels: [] })
		}
	]);
});

describe('Observe console', () => {
	it('opens on Now and keeps the other panes closed', async () => {
		render(ObservePage);
		await tick();

		expect(screen.getByRole('heading', { name: 'Observe' })).toBeInTheDocument();
		expect(screen.getByText('Quiet — nothing capturing')).toBeInTheDocument();
		expect(screen.getByRole('heading', { name: 'Upcoming' })).toBeInTheDocument();
		expect(screen.getByRole('heading', { name: 'Recent' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Listen' })).toBeInTheDocument();
		expect(screen.queryByRole('heading', { name: 'Listen to meeting' })).not.toBeInTheDocument();
		expect(screen.queryByText('No active captures')).not.toBeInTheDocument();
		expect(screen.queryByRole('heading', { name: 'Mail & chat' })).not.toBeInTheDocument();
		expect(screen.queryByRole('heading', { name: 'Startup catch-up' })).not.toBeInTheDocument();
	});

	it('opens one start form at a time', async () => {
		render(ObservePage);
		await fireEvent.click(screen.getByRole('button', { name: 'Listen' }));
		expect(screen.getByRole('heading', { name: 'Listen to meeting' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Start listening' })).toBeInTheDocument();

		await fireEvent.click(screen.getByRole('button', { name: /Join as/ }));
		expect(screen.queryByRole('heading', { name: 'Listen to meeting' })).not.toBeInTheDocument();
		expect(screen.getByRole('heading', { name: /Join as/ })).toBeInTheDocument();

		await fireEvent.click(screen.getByRole('button', { name: 'Watch screen' }));
		expect(screen.getByRole('heading', { name: 'Watch screen' })).toBeInTheDocument();
		expect(screen.getByText('Also hear')).toBeInTheDocument();
		expect(screen.queryByRole('heading', { name: /Join as/ })).not.toBeInTheDocument();
	});

	it('moves sources, audio, and notes into their own panes', async () => {
		render(ObservePage);

		await fireEvent.click(screen.getByRole('tab', { name: /Sources/ }));
		expect(screen.getByRole('heading', { name: 'Mail & chat' })).toBeInTheDocument();
		expect(screen.getByRole('heading', { name: 'Observe calendar' })).toBeInTheDocument();
		expect(screen.getByRole('heading', { name: 'Observe tabs' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Pair browser' })).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Details' }));
		expect(screen.getByText(/Denylist/)).toBeInTheDocument();
		expect(screen.getByRole('heading', { name: 'Startup catch-up' })).toBeInTheDocument();
		expect(await screen.findByRole('button', { name: 'Edit policy' })).toBeInTheDocument();
		expect(screen.queryByRole('heading', { name: 'Upcoming' })).not.toBeInTheDocument();

		await fireEvent.click(screen.getByRole('button', { name: 'Edit policy' }));
		expect(screen.getByText('History window')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'How each source catches up' })).toBeInTheDocument();

		await fireEvent.click(screen.getByRole('tab', { name: 'Audio' }));
		expect(screen.getByLabelText('Meeting audio profile')).toBeInTheDocument();
		expect(screen.getByLabelText('Listening audio profile')).toBeInTheDocument();
		expect(screen.queryByRole('heading', { name: 'Mail & chat' })).not.toBeInTheDocument();

		await fireEvent.click(screen.getByRole('tab', { name: 'Notes' }));
		expect(screen.getByRole('heading', { name: 'Notes' })).toBeInTheDocument();
		expect(screen.getByRole('link', { name: 'Audio Notes' })).toBeInTheDocument();

		await fireEvent.click(screen.getByRole('button', { name: /^Sources on/ }));
		expect(screen.getByRole('heading', { name: 'Mail & chat' })).toBeInTheDocument();
	});
});
