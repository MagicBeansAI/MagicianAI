import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import TurnStage from './TurnStage.svelte';

afterEach(() => cleanup());

type Row = {
	key: string;
	label: string;
	status: 'running' | 'done' | 'failed' | 'waiting';
	tool?: string;
	target?: string;
	startedAt?: number;
};

const rows: Row[] = [
	{ key: 'tool:a', label: 'web_fetch returned', status: 'done', tool: 'web_fetch', target: 'rust-lang.org' },
	{ key: 'llm:1', label: 'Response ready', status: 'done' },
	{ key: 'tool:b', label: 'Calling read_file', status: 'running', tool: 'read_file', target: 'main.rs' }
];

describe('TurnStage', () => {
	it('shows one working chip for the running row and a dot per settled row', () => {
		const { container } = render(TurnStage, { rows });

		const chips = container.querySelectorAll('[data-testid="turn-stage-chip"]');
		expect(chips).toHaveLength(1);
		expect(chips[0]).toHaveTextContent('Calling read_file');
		expect(chips[0]).toHaveTextContent('main.rs');
		expect(chips[0].getAttribute('style')).toContain('--cn-tint');

		const dots = container.querySelectorAll('.cn-stage__dot');
		expect(dots).toHaveLength(2);
		expect(dots[0]).toHaveAttribute('data-status', 'done');
		expect(dots[0]).toHaveAttribute('aria-label', expect.stringContaining('web_fetch returned'));

		const live = container.querySelector('[aria-live="polite"]');
		expect(live).toHaveTextContent('Calling read_file');
	});

	it('toggles an expanded list of every row', async () => {
		render(TurnStage, { rows });

		const toggle = screen.getByRole('button', { name: /steps/i });
		expect(toggle).toHaveAttribute('aria-expanded', 'false');
		expect(screen.queryAllByRole('listitem')).toHaveLength(0);

		await fireEvent.click(toggle);

		expect(toggle).toHaveAttribute('aria-expanded', 'true');
		const items = screen.getAllByRole('listitem');
		expect(items).toHaveLength(3);
		expect(items[0]).toHaveTextContent('web_fetch returned');
		expect(items[0]).toHaveTextContent('rust-lang.org');
	});

	it('marks a failed row dot as failed', () => {
		const { container } = render(TurnStage, {
			rows: [
				{ key: 'tool:x', label: 'search failed', status: 'failed', tool: 'search' },
				{ key: 'pause:1', label: 'Waiting on your input', status: 'waiting' }
			]
		});

		const dots = container.querySelectorAll('.cn-stage__dot');
		expect(dots[0]).toHaveAttribute('data-status', 'failed');
		expect(dots[1]).toHaveAttribute('data-status', 'waiting');
	});

	it('shows a Finishing chip when nothing is running', () => {
		const { container } = render(TurnStage, {
			rows: rows.map((row) => ({ ...row, status: 'done' as const }))
		});

		const chips = container.querySelectorAll('[data-testid="turn-stage-chip"]');
		expect(chips).toHaveLength(1);
		expect(chips[0]).toHaveTextContent('Finishing…');
		expect(container.querySelectorAll('.cn-stage__dot')).toHaveLength(3);
	});

	it('says it is waiting on the reader, not finishing, when a step waits', () => {
		const { container } = render(TurnStage, {
			rows: [
				{ key: 'tool:x', label: 'Search', status: 'done' as const, tool: 'search' },
				{ key: 'pause:1', label: 'Waiting on your input', status: 'waiting' as const }
			]
		});

		const chip = container.querySelector('[data-testid="turn-stage-chip"]');
		expect(chip).toHaveTextContent('Waiting on your input');
		expect(chip).toHaveClass('cn-stage__chip--waiting');
	});

	it('shows dots for the latest 5 settled steps and folds the rest into +N', () => {
		const many: Row[] = Array.from({ length: 12 }, (_, i) => ({
			key: `tool:${i}`,
			label: `Step ${i}`,
			status: 'done' as const,
			tool: i % 2 ? 'web_fetch' : 'read_file'
		}));
		const { container } = render(TurnStage, { rows: many });

		const dots = container.querySelectorAll('.cn-stage__dots .cn-stage__dot');
		expect(dots).toHaveLength(5);
		expect(dots[0]).toHaveAttribute('aria-label', expect.stringContaining('Step 7'));
		expect(dots[4]).toHaveAttribute('aria-label', expect.stringContaining('Step 11'));
		const more = container.querySelector('[data-testid="turn-stage-more"]');
		expect(more).toHaveTextContent('+7');
		expect(more).toHaveAttribute('aria-label', '7 earlier steps');
	});

	it('has no +N with 5 or fewer settled steps', () => {
		const { container } = render(TurnStage, { rows });
		expect(container.querySelector('[data-testid="turn-stage-more"]')).toBeNull();
		expect(screen.queryByRole('button', { name: /show all/i })).toBeNull();
	});

	it('expands to the latest 5 rows and shows all on request', async () => {
		const many: Row[] = Array.from({ length: 9 }, (_, i) => ({
			key: `tool:${i}`,
			label: `Step ${i}`,
			status: i === 8 ? ('running' as const) : ('done' as const)
		}));
		render(TurnStage, { rows: many });

		await fireEvent.click(screen.getByRole('button', { name: /show 9 steps/i }));
		let items = screen.getAllByRole('listitem');
		expect(items).toHaveLength(5);
		expect(items[0]).toHaveTextContent('Step 4');
		expect(items[4]).toHaveTextContent('Step 8');

		const all = screen.getByRole('button', { name: 'Show all 9 steps' });
		await fireEvent.click(all);
		items = screen.getAllByRole('listitem');
		expect(items).toHaveLength(9);
		expect(items[0]).toHaveTextContent('Step 0');

		await fireEvent.click(screen.getByRole('button', { name: 'Show latest only' }));
		expect(screen.getAllByRole('listitem')).toHaveLength(5);

		// Collapsing resets to the latest window.
		await fireEvent.click(screen.getByRole('button', { name: 'Show all 9 steps' }));
		await fireEvent.click(screen.getByRole('button', { name: /hide/i }));
		await fireEvent.click(screen.getByRole('button', { name: /show 9 steps/i }));
		expect(screen.getAllByRole('listitem')).toHaveLength(5);
	});

	it("ticks the working chip's elapsed once a second, and drops it when the row settles", async () => {
		vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
		vi.setSystemTime(100_000);
		try {
			const running: Row = { key: 'llm:call-1', label: 'Thinking with gpt-6.1-sol', status: 'running', startedAt: 35_000 };
			const { container, rerender } = render(TurnStage, { rows: [running] });
			const elapsed = () => container.querySelector('[data-testid="turn-stage-elapsed"]');
			expect(elapsed()).toHaveTextContent('1:05');

			vi.advanceTimersByTime(1_000);
			await tick();
			expect(elapsed()).toHaveTextContent('1:06');

			await rerender({ rows: [{ ...running, status: 'done', startedAt: undefined }] });
			expect(elapsed()).toBeNull();
		} finally {
			vi.useRealTimers();
		}
	});

	it('shows no elapsed for a running row with no start', () => {
		const { container } = render(TurnStage, { rows });
		expect(container.querySelector('[data-testid="turn-stage-elapsed"]')).toBeNull();
	});
});
