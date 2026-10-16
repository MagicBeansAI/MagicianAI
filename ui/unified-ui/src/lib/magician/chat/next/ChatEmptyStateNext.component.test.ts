import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ChatSession } from '$lib/stores/chatStore';
import { EMPTY_STATE_PROMPTS } from '$lib/magician/chat/components/emptyStatePrompts';
import ChatEmptyStateNext from './ChatEmptyStateNext.svelte';

afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

function props(overrides: Record<string, unknown> = {}) {
	return {
		isReadOnly: false,
		recentSessions: [] as ChatSession[],
		onSuggest: vi.fn(),
		onOpenSession: vi.fn(),
		...overrides
	};
}

function session(id: string, title: string): ChatSession {
	return { id, title, updated_at: Date.now() - 60_000 } as unknown as ChatSession;
}

describe('ChatEmptyStateNext', () => {
	it('calls onSuggest with the card prompt on click', async () => {
		const p = props();
		render(ChatEmptyStateNext, p);
		const first = EMPTY_STATE_PROMPTS[0];
		await fireEvent.click(screen.getByRole('button', { name: new RegExp(first.label) }));
		expect(p.onSuggest).toHaveBeenCalledWith(first.prompt);
	});

	it('renders four suggestion cards from the shared prompt source', () => {
		const { container } = render(ChatEmptyStateNext, props());
		const cards = container.querySelectorAll('.cn-empty-card');
		expect(cards).toHaveLength(4);
		cards.forEach((card, i) => {
			expect(card.textContent).toContain(EMPTY_STATE_PROMPTS[i].label);
			expect(card.getAttribute('style')).toContain('--cn-tint');
		});
	});

	it('hides the suggestion cards when read-only', () => {
		const { container } = render(ChatEmptyStateNext, props({ isReadOnly: true }));
		expect(container.querySelectorAll('.cn-empty-card')).toHaveLength(0);
		expect(screen.getByRole('heading', { level: 3 })).toHaveTextContent('Nothing in this session');
	});

	it('opens a recent session from its chip', async () => {
		const p = props({ recentSessions: [session('s-1', 'Kyoto trip'), session('s-2', 'Groceries')] });
		render(ChatEmptyStateNext, p);
		await fireEvent.click(screen.getByRole('button', { name: /Groceries/ }));
		expect(p.onOpenSession).toHaveBeenCalledWith('s-2');
	});

	it('caps recent session chips at five', () => {
		const sessions = Array.from({ length: 7 }, (_, i) => session(`s-${i}`, `Session ${i}`));
		const { container } = render(ChatEmptyStateNext, props({ recentSessions: sessions }));
		expect(container.querySelectorAll('.cn-empty-recent')).toHaveLength(5);
	});

	it.each([
		[9, 'Good morning'],
		[14, 'Good afternoon'],
		[20, 'Good evening']
	])('greets by time of day at %i:00', (hour, greeting) => {
		vi.useFakeTimers({ toFake: ['Date'] });
		vi.setSystemTime(new Date(2026, 9, 1, hour, 0, 0));
		render(ChatEmptyStateNext, props());
		expect(screen.getByRole('heading', { level: 3 })).toHaveTextContent(greeting);
	});
});
