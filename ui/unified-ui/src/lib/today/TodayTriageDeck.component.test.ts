import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import TodayTriageDeck, { type TriageCardData } from './TodayTriageDeck.svelte';

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
			primaryLabel: 'Open note',
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
		expect(screen.getByText('All Dispatches Cleared')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Open Broadsheet View/ })).toBeInTheDocument();
	});
});
