import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import VibeEmptyState from './VibeEmptyState.svelte';
import { VIBE_EMPTY_SUGGESTIONS } from './vibeEmptySuggestions';

afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

function props(overrides: Record<string, unknown> = {}) {
	return {
		activeProjectName: null as string | null,
		recentRuns: [] as Array<{ id: string; title: string; updatedAt?: string; createdAt?: string }>,
		onSuggest: vi.fn(),
		onSelectRun: vi.fn(),
		...overrides
	};
}

function run(id: string, title: string) {
	return {
		id,
		title,
		updatedAt: new Date(Date.now() - 60_000).toISOString()
	};
}

describe('VibeEmptyState', () => {
	it('calls onSuggest with the card prompt on click', async () => {
		const p = props();
		render(VibeEmptyState, p);
		const first = VIBE_EMPTY_SUGGESTIONS[0];
		await fireEvent.click(screen.getByRole('button', { name: new RegExp(first.label) }));
		expect(p.onSuggest).toHaveBeenCalledWith(first.prompt);
	});

	it('renders four suggestion cards with colorful tints', () => {
		const { container } = render(VibeEmptyState, props());
		const cards = container.querySelectorAll('.vibe-empty-card');
		expect(cards).toHaveLength(4);
		cards.forEach((card, i) => {
			expect(card.textContent).toContain(VIBE_EMPTY_SUGGESTIONS[i].label);
			expect(card.getAttribute('style')).toContain('--cn-tint');
		});
	});

	it('shows project-specific subtitle when activeProjectName is provided', () => {
		render(VibeEmptyState, props({ activeProjectName: 'magic-portal' }));
		expect(screen.getByText('magic-portal')).toBeInTheDocument();
	});

	it('selects a recent run when its chip is clicked', async () => {
		const p = props({
			recentRuns: [run('run-1', 'Build habit tracker'), run('run-2', 'Add dark mode')]
		});
		render(VibeEmptyState, p);
		await fireEvent.click(screen.getByRole('button', { name: /Add dark mode/ }));
		expect(p.onSelectRun).toHaveBeenCalledWith('run-2');
	});

	it('caps recent run chips at five', () => {
		const runs = Array.from({ length: 8 }, (_, i) => run(`run-${i}`, `Build task ${i}`));
		const { container } = render(VibeEmptyState, props({ recentRuns: runs }));
		const chips = container.querySelectorAll('.vibe-empty-recent');
		expect(chips).toHaveLength(5);
	});
});
