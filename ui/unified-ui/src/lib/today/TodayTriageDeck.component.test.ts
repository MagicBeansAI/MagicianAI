import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';
import type { ResurfacingCard } from './resurfacingQueries';

const store = vi.hoisted(() => ({ approve: vi.fn() }));
vi.mock('$lib/stores/channelNeedsYouStore', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	approveChannelFollowUp: store.approve
}));

import TodayTriageDeck, { type TriageCardData } from './TodayTriageDeck.svelte';

const followUpRecord = {
	annotation_id: 'fu-1',
	provider: 'gmail',
	account_alias: 'personal',
	account_email: 'me@example.test',
	thread_id: 'thread-1',
	lane: 'user_assist',
	label: 'needs_reply',
	confidence: 0.9,
	reason: null,
	proposed_action: null,
	subject: 'Contract renewal',
	sender: 'Dana',
	summary: 'Dana asks whether to renew.',
	evidence_message_id: null,
	evidence_message_at: null,
	created_at: 0,
	received_at: null,
	open_url: 'https://mail.google.com/thread-1',
	available_actions: [{ id: 'reply', label: 'Reply', needs_compose: true, confirm: false }]
} as ChannelFollowUp;

const worthRecord = {
	candidate_id: 'cand-1',
	line: 'Policy update',
	why_now: 'Effective soon',
	source_title: 'Card policy update',
	summary: 'The monthly cap changes.',
	source_kind: 'comm',
	source_ref: 'comm:1',
	detail_label: 'Summary',
	temporal_anchor_at: null,
	brief: null,
	brief_status: 'legacy',
	content_revision: 'rev-1',
	source_updated: false,
	recommended_action: null,
	actions: [
		{ kind: 'ask_presto', label: 'Ask Presto', requires_input: true, side_effect: 'none' },
		{ kind: 'create_task', label: 'Create task', requires_input: true, side_effect: 'creates_task' }
	]
} as ResurfacingCard;

const followUpCard: TriageCardData = {
	id: 'followup:fu-1',
	title: 'Contract renewal',
	category: 'FOR YOU · GMAIL',
	summary: 'Dana asks whether to renew.',
	originLane: 'dispatch',
	rawFollowUp: followUpRecord
};

const worthCard: TriageCardData = {
	id: 'worth:cand-1',
	title: 'Card policy update',
	category: 'WORTH A LOOK · COMM',
	summary: 'The monthly cap changes.',
	originLane: 'reading_room',
	rawWorth: worthRecord
};

describe('TodayTriageDeck', () => {
	const mockCards: TriageCardData[] = [
		{
			id: 'card-1',
			title: 'First Morning Dispatch',
			category: 'DISPATCH · GMAIL',
			summary: 'Summary of the first dispatch to review.',
			sender: 'Alice',
			originLane: 'dispatch'
		},
		{
			id: 'card-2',
			title: 'Second Resurfaced Note',
			category: 'READING ROOM · MEMORY',
			summary: 'Summary of the second note to review.',
			originLane: 'reading_room'
		}
	];

	it('renders active top card and tabs for All, For You, and Worth a Look', () => {
		render(TodayTriageDeck, {
			props: {
				cards: mockCards
			}
		});

		expect(screen.getByText('First Morning Dispatch')).toBeInTheDocument();
		expect(screen.getByText('Summary of the first dispatch to review.')).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /All/i })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /For You/i })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /Worth a Look/i })).toBeInTheDocument();
	});

	it('renders triage action buttons and advances deck on dismiss', async () => {
		render(TodayTriageDeck, {
			props: {
				cards: mockCards
			}
		});

		const dismissBtn = screen.getByRole('button', { name: /Dismiss/ });
		expect(dismissBtn).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Useful/ })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Seen/ })).toBeInTheDocument();

		// Click dismiss to advance to next card
		await fireEvent.click(dismissBtn);

		// Wait for animation transition
		await new Promise((r) => setTimeout(r, 300));
		expect(screen.getByText('Second Resurfaced Note')).toBeInTheDocument();
	});

	it('allows filtering by For You and Worth a Look tabs', async () => {
		render(TodayTriageDeck, {
			props: {
				cards: mockCards
			}
		});

		const worthTab = screen.getByRole('tab', { name: /Worth a Look/i });
		await fireEvent.click(worthTab);

		expect(screen.getByText('Second Resurfaced Note')).toBeInTheDocument();

		const forYouTab = screen.getByRole('tab', { name: /For You/i });
		await fireEvent.click(forYouTab);

		expect(screen.getByText('First Morning Dispatch')).toBeInTheDocument();
	});

	it('shows cleared state when all cards are triaged', async () => {
		render(TodayTriageDeck, {
			props: {
				cards: [mockCards[0]]
			}
		});

		const ackBtn = screen.getByRole('button', { name: /Seen/ });
		await fireEvent.click(ackBtn);

		await new Promise((r) => setTimeout(r, 300));
		expect(screen.getByText('All cards reviewed')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Open Broadsheet View/ })).toBeInTheDocument();
	});
    it('distinguishes loaded cards from server totals and prefetches the selected lane', async () => {
        const needMore = vi.fn();
        render(TodayTriageDeck, { props: { cards: mockCards, followUpTotal: 24, worthTotal: 12,
            hasMoreForYou: true, hasMoreWorth: true }, events: { needMore } });
        expect(screen.getByText('2 loaded · 36 total')).toBeInTheDocument();
        await fireEvent.click(screen.getByRole('tab', { name: /For You/i }));
        expect(screen.getByText('1 loaded · 24 total')).toBeInTheDocument();
        await waitFor(() => expect(needMore).toHaveBeenCalledWith(expect.objectContaining({ detail: 'for_you' })));
    });

    it('keeps loading and failed pages distinct from an empty deck', async () => {
        const { rerender } = render(TodayTriageDeck, { props: { cards: [], loadingMore: true, followUpTotal: 20 } });
        expect(screen.getByRole('status')).toHaveTextContent('Loading more cards');
        expect(screen.queryByText('All cards reviewed')).not.toBeInTheDocument();
        await rerender({ cards: [], loadingMore: false, loadError: 'Page unavailable', followUpTotal: 20 });
        expect(screen.getByText('Page unavailable')).toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Load more cards' })).toBeInTheDocument();
        expect(screen.getByRole('tab', { name: /For You 20/ })).toBeInTheDocument();
    });

    it('restores a swiped card when its action fails', async () => {
        render(TodayTriageDeck, { props: { cards: [mockCards[0]] }, events: {
            action: (event) => event.detail.complete?.(false)
        } });
        await fireEvent.click(screen.getByRole('button', { name: /Seen/ }));
        await new Promise(resolve => setTimeout(resolve, 300));
        expect(screen.getByText('First Morning Dispatch')).toBeInTheDocument();
        expect(screen.queryByText('All cards reviewed')).not.toBeInTheDocument();
    });

    it('lets a loaded lane work while the other lane is still loading', async () => {
        render(TodayTriageDeck, { props: { cards: [mockCards[0]], worthLoading: true } });
        await fireEvent.click(screen.getByRole('tab', { name: /For You/ }));
        expect(screen.getByText('1 loaded · 1 total')).toBeInTheDocument();
        expect(screen.queryByText('Loading Reading Room…')).not.toBeInTheDocument();
    });

    it('opens the Act menu instead of starting a follow-up task', async () => {
        const action = vi.fn();
        render(TodayTriageDeck, { props: { cards: [followUpCard] }, events: { action } });
        const act = screen.getByTestId('today-deck-act');
        expect(act).toHaveTextContent('⚡ Act');
        expect(screen.queryByText(/Do it/)).not.toBeInTheDocument();
        await fireEvent.click(act);
        const items = screen.getAllByRole('menuitem').map((item) => item.textContent?.trim());
        expect(items).toEqual(['Reply', 'Open in Gmail', 'Start follow-up task…', 'Snooze — hide from Today', 'Details']);
        expect(screen.getByTestId('today-deck-act-open-provider')).toHaveAttribute('href', 'https://mail.google.com/thread-1');
        expect(action).not.toHaveBeenCalled();
        expect(store.approve).not.toHaveBeenCalled();
        expect(screen.getByRole('heading', { name: 'Contract renewal' })).toBeInTheDocument();
    });

    it('asks for the instruction before approving, then drops the resolved card', async () => {
        store.approve.mockResolvedValue({ ok: true, taskId: 'task-9' });
        const resolved = vi.fn();
        render(TodayTriageDeck, { props: { cards: [followUpCard, mockCards[1]] }, events: { resolved } });
        await fireEvent.click(screen.getByTestId('today-deck-act'));
        await fireEvent.click(screen.getByTestId('today-deck-act-start-task'));
        const instruction = await screen.findByLabelText('Optional instruction for the agent');
        expect(store.approve).not.toHaveBeenCalled();
        // The modal isolates the page behind it, so the card reads as hidden.
        expect(screen.getByRole('heading', { name: 'Contract renewal', hidden: true })).toBeInTheDocument();
        await fireEvent.input(instruction, { target: { value: 'Draft a polite decline' } });
        await fireEvent.click(screen.getByRole('button', { name: 'Create follow-up task' }));
        await waitFor(() => expect(store.approve).toHaveBeenCalledTimes(1));
        expect(store.approve.mock.calls[0].slice(0, 2)).toEqual(['fu-1', 'Draft a polite decline']);
        await waitFor(() => expect(resolved).toHaveBeenCalledTimes(1));
        expect(resolved.mock.calls[0][0].detail.card.id).toBe('followup:fu-1');
        await screen.findByText('Second Resurfaced Note');
    });

    it('keeps the card on top when the menu or details are only opened', async () => {
        const action = vi.fn();
        render(TodayTriageDeck, { props: { cards: [followUpCard] }, events: { action } });
        await fireEvent.click(screen.getByTestId('today-deck-act'));
        await fireEvent.keyDown(window, { key: 'Escape' });
        expect(screen.queryByRole('menu')).not.toBeInTheDocument();
        await fireEvent.click(screen.getByTestId('today-deck-details'));
        expect(await screen.findByTestId('today-deck-detail')).toBeInTheDocument();
        expect(screen.getByRole('heading', { name: 'Contract renewal' })).toBeInTheDocument();
        expect(action).not.toHaveBeenCalled();
    });

    it('snoozes through the deck commit path and restores the card on failure', async () => {
        const action = vi.fn((event) => event.detail.complete?.(false));
        render(TodayTriageDeck, { props: { cards: [followUpCard] }, events: { action } });
        await fireEvent.click(screen.getByTestId('today-deck-act'));
        await fireEvent.click(screen.getByTestId('today-deck-act-snooze'));
        await new Promise(resolve => setTimeout(resolve, 300));
        expect(action).toHaveBeenCalledWith(expect.objectContaining({ detail: expect.objectContaining({ kind: 'snooze' }) }));
        expect(screen.getByRole('heading', { name: 'Contract renewal' })).toBeInTheDocument();
    });

    it('opens the Act menu on Enter and lists Worth actions with the agent name', async () => {
        const action = vi.fn();
        render(TodayTriageDeck, { props: { cards: [worthCard], agentName: 'Nova', chatThreadId: 'general' }, events: { action } });
        expect(screen.getByTestId('today-deck-act')).toHaveTextContent('Act');
        await fireEvent.keyDown(document.body, { key: 'Enter' });
        const items = (await screen.findAllByRole('menuitem')).map((item) => item.textContent?.trim());
        expect(items).toEqual(['Open', 'Ask Nova', 'Create task', 'Details']);
        expect(screen.queryByText('Ask Presto')).not.toBeInTheDocument();
        expect(action).not.toHaveBeenCalled();
        await fireEvent.click(screen.getByTestId('today-deck-act-open'));
        await new Promise(resolve => setTimeout(resolve, 300));
        expect(action).toHaveBeenCalledWith(expect.objectContaining({ detail: expect.objectContaining({ kind: 'primary' }) }));
    });

    it('does not submit a pending swipe after leaving the page or changing workspace', async () => {
        const action = vi.fn();
        const { unmount } = render(TodayTriageDeck, { props: { cards: [mockCards[0]] }, events: { action } });
        await fireEvent.click(screen.getByRole('button', { name: /Dismiss/ }));
        await unmount();
        await new Promise(resolve => setTimeout(resolve, 300));
        expect(action).not.toHaveBeenCalled();
    });

});
