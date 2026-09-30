import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import TodayNewspaperCard from './TodayNewspaperCard.svelte';
import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';

describe('TodayNewspaperCard', () => {
	const mockFollowUp: ChannelFollowUp = {
		annotation_id: 'fu-1',
		provider: 'gmail',
		account_email: 'test@example.com',
		sender: 'Sarah Connor',
		subject: 'Reschedule sync to 2:30 PM',
		summary: 'Wants to move our weekly architecture review to this afternoon.',
		reason: 'Scheduling conflict',
		evidence_message_id: 'msg-1',
		evidence_message_at: Date.now(),
		created_at: Date.now(),
		received_at: Date.now(),
		open_url: 'https://mail.google.com',
		proposed_action: {
			label: 'Confirm 2:30 PM'
		},
		available_actions: [
			{
				id: 'act-1',
				label: 'Confirm 2:30 PM',
				icon: 'check',
				needs_compose: false,
				confirm: true
			}
		],
		account_alias: 'primary',
		thread_id: 'thread-1',
		lane: 'user_assist',
		confidence: 0.9,
		label: 'needs_reply'
	};

	it('renders with editorial headline, kicker, and summary', () => {
		render(TodayNewspaperCard, {
			props: {
				followUp: mockFollowUp
			}
		});

		expect(screen.getByText('Reschedule sync to 2:30 PM')).toBeInTheDocument();
		expect(screen.getByText(/DISPATCH · GMAIL/)).toBeInTheDocument();
		expect(screen.getByText('Sarah Connor')).toBeInTheDocument();
		expect(screen.getByText('Wants to move our weekly architecture review to this afternoon.')).toBeInTheDocument();
	});

	it('renders at most 4 primary buttons', () => {
		render(TodayNewspaperCard, {
			props: {
				followUp: mockFollowUp
			}
		});

		// 1. Primary action — labelled for what it runs (approve), not for a
		// channel descriptor that needs compose/commit.
		expect(screen.getByRole('button', { name: /Do it/ })).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: /Confirm 2:30 PM/ })).not.toBeInTheDocument();
		// 2. Useful
		expect(screen.getByRole('button', { name: /Useful/ })).toBeInTheDocument();
		// 3. Seen (Acknowledge)
		expect(screen.getByRole('button', { name: /Seen/ })).toBeInTheDocument();
		// 4. Dismiss
		expect(screen.getByRole('button', { name: /Dismiss/ })).toBeInTheDocument();
	});

	it('opens progressive popover on dismiss caret click and closes on outside click', async () => {
		render(TodayNewspaperCard, {
			props: {
				followUp: mockFollowUp
			}
		});

		const caret = screen.getByRole('button', { name: /More dismissal and snooze options/ });
		await fireEvent.click(caret);

		expect(screen.getByText('Already handled')).toBeInTheDocument();
		expect(screen.getByText('Not relevant')).toBeInTheDocument();
		expect(screen.getByText("Shouldn't be flagged")).toBeInTheDocument();
		// Follow-up snooze has no duration on the wire, so no times are offered.
		expect(screen.getByText('Snooze — hide from Today')).toBeInTheDocument();
		expect(screen.queryByText('Tomorrow morning (8 AM)')).not.toBeInTheDocument();

		// Clicking outside closes the popover
		await fireEvent.click(document.body);
		expect(screen.queryByText('Already handled')).not.toBeInTheDocument();
	});

	it('closes progressive popover on Escape key', async () => {
		render(TodayNewspaperCard, {
			props: {
				followUp: mockFollowUp
			}
		});

		const caret = screen.getByRole('button', { name: /More dismissal and snooze options/ });
		await fireEvent.click(caret);
		expect(screen.getByText('Already handled')).toBeInTheDocument();

		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(screen.queryByText('Already handled')).not.toBeInTheDocument();
	});

	it('offers worth cards only the dismiss reasons and actions resurfacing accepts', async () => {
		render(TodayNewspaperCard, {
			props: {
				worth: {
					candidate_id: 'w-1',
					line: 'Revisit the pricing memo',
					why_now: 'Pricing review is this week',
					source_title: 'Pricing memo',
					summary: 'Draft from last month',
					source_kind: 'note',
					source_ref: 'note:1',
					detail_label: 'View',
					temporal_anchor_at: null,
					brief: null,
					brief_status: 'v2',
					content_revision: null,
					source_updated: false,
					recommended_action: { kind: 'create_task', label: 'Create task' },
					actions: []
				} as never
			}
		});

		expect(screen.getByRole('button', { name: /Open/ })).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: /More dismissal and snooze options/ }));
		expect(screen.getByText('Already handled')).toBeInTheDocument();
		expect(screen.queryByText("Shouldn't be flagged")).not.toBeInTheDocument();
		expect(screen.queryByText(/Snooze/)).not.toBeInTheDocument();
	});
});
