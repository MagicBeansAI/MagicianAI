import { cleanup, render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { vibeStudioStore } from '$lib/stores/vibeStudioStore';
import StudioStage from './StudioStage.svelte';

describe('StudioStage first-run controls', () => {
	beforeEach(() => {
		// Below 1280px the open stage is a drawer that starts closed; test the
		// wide layout, where it starts open.
		Object.defineProperty(window, 'innerWidth', { configurable: true, writable: true, value: 1440 });
		vibeStudioStore.resetView();
	});
	afterEach(() => {
		cleanup();
		vibeStudioStore.resetView();
	});

	it('keeps the CLI Agent available before any run content appears', async () => {
		const user = userEvent.setup();
		render(StudioStage, { firstRun: true });

		expect(screen.getByText('Your build will appear here')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Apply all changes' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Publish ↗' })).not.toBeInTheDocument();

		const cliButton = screen.getByRole('button', { name: '⌥ CLI Agent' });
		expect(cliButton).toHaveAttribute('aria-pressed', 'false');
		await user.click(cliButton);
		expect(get(vibeStudioStore).terminalOpen).toBe(true);
	});

	it('offers no Apply-all for a finished run that only carries a briefing', () => {
		vibeStudioStore.setStageTab('diff');
		const briefing = {
			id: 'vibedev-run-ended:task_1:exec_1',
			source: 'user_request' as const,
			input_type: 'choice' as const,
			schema: {
				request_type: 'harness.notify_owner.briefing',
				options: [{ id: 'acknowledge', label: 'Acknowledge' }]
			},
			prompt: 'Owner briefing: Autopilot run finished: Pi\n\nAutopilot run finished: Pi. Open it.',
			scope: { task_id: 'task_1' },
			identifiers: { request_id: 'vibedev-run-ended:task_1:exec_1' }
		};
		render(StudioStage, {
			firstRun: false,
			orderedDiffRows: [],
			orderedOtherRows: [
				{ key: briefing.id, entry: { correlation_id: briefing.id, at: 0 }, request: briefing }
			]
		});

		expect(screen.queryByRole('button', { name: 'Apply all changes' })).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Acknowledge' })).toBeEnabled();
		expect(screen.getByText('Autopilot run finished: Pi. Open it.')).toBeInTheDocument();
	});
});
