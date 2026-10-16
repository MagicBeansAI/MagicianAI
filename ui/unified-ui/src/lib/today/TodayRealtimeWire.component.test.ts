import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import TodayRealtimeWire, { type WireItem } from './TodayRealtimeWire.svelte';

const now = Date.now();
const fixtureItems: WireItem[] = [
	{ id: 'e1', kind: 'event', title: 'Task completed', summary: 'Task t1 completed', timestamp: now - 60_000, badge: 'task', severity: 'success', href: '/events?task_id=t1' },
	{ id: 'e2', kind: 'event', title: 'Step started', summary: 'Executing s2', timestamp: now - 120_000, badge: 'step', href: '/events' },
	{ id: 'i1', kind: 'insight', title: 'Memory learned', summary: 'Prefers terse replies', timestamp: now - 180_000, badge: 'learning insight', href: '/feed' },
	{ id: 'a1', kind: 'activity', title: 'Research agent: cycle', summary: 'Focused on inbox', timestamp: now - 240_000, badge: 'cycle', href: '/feed' },
	{ id: 'a2', kind: 'activity', title: 'Delivery', summary: 'Briefing published', timestamp: now - 300_000, badge: 'delivery', href: '/feed' }
];

describe('TodayRealtimeWire', () => {
	it('renders collapsed realtime wire bar with live indicator and compact 24h count', () => {
		render(TodayRealtimeWire);

		// Blinking green dot is present with live label/title
		expect(screen.getByLabelText(/Live stream active/i)).toBeInTheDocument();
		// "LIVE WIRE" text is removed
		expect(screen.queryByText('LIVE WIRE')).not.toBeInTheDocument();
		// Compact 24H count is displayed on the right (e.g. 128/24H or 35K/24H)
		expect(screen.getByText(/\d+[KM]?\/24H/i)).toBeInTheDocument();
		expect(screen.getByRole('button', { name: /Expand realtime wire/i })).toBeInTheDocument();
	});

	it('shows no placeholder activity before real items arrive', () => {
		render(TodayRealtimeWire);

		expect(screen.getByText('Awaiting incoming transmissions…')).toBeInTheDocument();
	});

	it('expands to reveal filter tabs and latest 5 items when toggle button is clicked', async () => {
		render(TodayRealtimeWire, { props: { initialItems: fixtureItems } });

		const toggleBtn = screen.getByRole('button', { name: /Expand realtime wire/i });
		await fireEvent.click(toggleBtn);

		// Filter tabs should be visible
		expect(screen.getByRole('tab', { name: /^All/i })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /^Events/i })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /^Insights/i })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: /^Activity/i })).toBeInTheDocument();

		// Deep links should be rendered
		expect(screen.getByText('/events stream')).toBeInTheDocument();
		expect(screen.getByText('/feed')).toBeInTheDocument();

		// Items should be displayed
		const cards = screen.getAllByRole('button');
		expect(cards.length).toBeGreaterThanOrEqual(5);
	});

	it('filters dispatches when clicking a category tab', async () => {
		render(TodayRealtimeWire, { props: { initialItems: fixtureItems } });

		const toggleBtn = screen.getByRole('button', { name: /Expand realtime wire/i });
		await fireEvent.click(toggleBtn);

		const eventsTab = screen.getByRole('tab', { name: /^Events/i });
		await fireEvent.click(eventsTab);

		expect(eventsTab).toHaveClass('active');
		// Should show EVENT kind tags
		const eventBadges = screen.getAllByText('EVENT');
		expect(eventBadges.length).toBeGreaterThan(0);
	});

	it('renders exact 35K/24H when 35,086 events are reported', () => {
		render(TodayRealtimeWire, {
			props: {
				eventCount24h: 35086
			}
		});

		expect(screen.getByText('35K/24H')).toBeInTheDocument();
		expect(screen.queryByText('35,086 events (24h)')).not.toBeInTheDocument();
	});
});
