import { cleanup, render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { vibeStudioStore } from '$lib/stores/vibeStudioStore';
import StudioStage from './StudioStage.svelte';

describe('StudioStage first-run controls', () => {
	beforeEach(() => vibeStudioStore.resetView());
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
});
