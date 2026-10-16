import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AgentSummary } from '$lib/stores/agentStore';
import SquareWorldChrome from './SquareWorldChrome.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

function agent(overrides: Partial<AgentSummary> = {}): AgentSummary {
	return {
		agent_id: 'atlas',
		name: 'Atlas',
		status: 'idle',
		updated_at: 1,
		...overrides
	};
}

describe('SquareWorldChrome', () => {
	it('puts Create task, Attention, the glance status bar, and the roster on the world pane', () => {
		render(SquareWorldChrome, { agents: [agent()], selectedId: null, canFly: false });
		expect(screen.getByRole('button', { name: /create task/i })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /attention/i })).toBeInTheDocument();
		expect(screen.getByRole('region', { name: /crew status/i })).toBeInTheDocument();
		expect(screen.getByTitle('Open crew')).toBeInTheDocument();
		expect(screen.getByTitle('Open usage and cost')).toBeInTheDocument();
		expect(screen.getByRole('toolbar', { name: /crew roster/i })).toBeInTheDocument();
		expect(screen.getByTitle(/^Atlas/)).toBeInTheDocument();
	});

	it('selects a roster frame into the dock', async () => {
		const select = vi.fn();
		render(SquareWorldChrome, {
			props: { agents: [agent()], selectedId: null, canFly: false },
			events: { select }
		});
		await fireEvent.click(screen.getByTitle(/^Atlas/));
		expect(select).toHaveBeenCalledWith(expect.objectContaining({ detail: { target: 'agent:atlas' } }));
	});

	it('opens the dock spend tab from the cost glance', async () => {
		const dock = vi.fn();
		render(SquareWorldChrome, {
			props: { agents: [agent()], selectedId: null, canFly: false },
			events: { dock }
		});
		await fireEvent.click(screen.getByTitle('Open usage and cost'));
		expect(dock).toHaveBeenCalledWith(expect.objectContaining({ detail: { section: 'spend' } }));
	});
});
