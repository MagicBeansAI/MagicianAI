import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import CommandDock from './CommandDock.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

const citizen = (id: string, extra: Record<string, unknown> = {}) =>
	({
		id,
		name: id,
		vibe: 'idle',
		title: 'Crew',
		guildId: 'commons',
		guildIds: ['commons'],
		currentWork: [],
		...extra
	}) as never;

describe('CommandDock', () => {
	it('is fleet-wide when nothing is selected', () => {
		render(CommandDock, { citizens: [citizen('a')], guilds: [], selectedTarget: null });
		expect(screen.getByRole('tablist')).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /^crew$/i })).toBeInTheDocument();
	});

	it('names the selected crew member when one is selected', () => {
		render(CommandDock, {
			citizens: [citizen('atlas')],
			guilds: [],
			selectedTarget: 'agent:atlas'
		});
		expect(screen.getByText('atlas')).toBeInTheDocument();
	});

	it('offers Capabilities and Delegation on the selected crew member', async () => {
		render(CommandDock, {
			citizens: [
				citizen('atlas', { tools: ['web_search'], delegationTargets: ['nova'] }),
				citizen('nova')
			],
			guilds: [],
			selectedTarget: 'agent:atlas'
		});
		expect(screen.getByRole('tab', { name: /^capabilities$/i })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /^delegation$/i })).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('tab', { name: /^capabilities$/i }));
		expect(screen.getByText('Web Search')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('tab', { name: /^delegation$/i }));
		expect(screen.getByText('nova')).toBeInTheDocument();
	});
});
