import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';
import {
	deckActEntries,
	deckActEntryResolvesCard,
	FOLLOW_UP_SNOOZE_LABEL,
	followUpActEntries,
	openWorthLinkInGesture,
	START_FOLLOW_UP_TASK_LABEL,
	takeWorthLinkOpenedInGesture,
	worthActEntries
} from './deckActMenu';
import type { ResurfacingCard } from './resurfacingQueries';

function followUp(overrides: Partial<ChannelFollowUp> = {}): ChannelFollowUp {
	return {
		annotation_id: 'fu-1',
		provider: 'gmail',
		subject: 'Reschedule sync',
		summary: 'Wants to move the review.',
		open_url: 'https://mail.google.com/thread',
		available_actions: [
			{ id: 'reply', label: 'Reply', icon: '↩', needs_compose: true, confirm: false },
			{ id: 'archive', label: 'Archive', needs_compose: false, confirm: true }
		],
		...overrides
	} as ChannelFollowUp;
}

function worth(overrides: Partial<ResurfacingCard> = {}): ResurfacingCard {
	return {
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
			{ kind: 'view_details', label: 'Details', requires_input: false, side_effect: 'none' },
			{ kind: 'open_source', label: 'Open source', requires_input: false, side_effect: 'none' },
			{ kind: 'ask_presto', label: 'Ask Presto', requires_input: true, side_effect: 'none' },
			{ kind: 'create_task', label: 'Create task', requires_input: true, side_effect: 'creates_task' },
			{ kind: 'summarize_deeper', label: 'Summarize deeper', requires_input: false, side_effect: 'none' }
		],
		...overrides
	} as ResurfacingCard;
}

describe('deck Act menu — follow-up card', () => {
	it('lists channel actions, open, task, snooze and details in that order', () => {
		const entries = followUpActEntries(followUp());
		expect(entries.map((entry) => entry.label)).toEqual([
			'Reply',
			'Archive',
			'Open in Gmail',
			START_FOLLOW_UP_TASK_LABEL,
			FOLLOW_UP_SNOOZE_LABEL,
			'Details'
		]);
		expect(entries.map((entry) => entry.id)).toEqual([
			'channel-reply',
			'channel-archive',
			'open-provider',
			'start-task',
			'snooze',
			'details'
		]);
		expect(entries[0]).toMatchObject({ kind: 'channel_action', icon: '↩' });
		expect(entries[2]).toMatchObject({ kind: 'open_provider', href: 'https://mail.google.com/thread' });
	});

	it('omits what the follow-up does not declare', () => {
		const entries = followUpActEntries(
			followUp({ available_actions: [], open_url: undefined, review_required: true })
		);
		// A stale draft is re-opened for review, never approved from the menu.
		expect(entries.map((entry) => entry.kind)).toEqual(['snooze', 'details']);
	});

	it('only snooze resolves a follow-up from the menu; the task asks first', () => {
		const resolving = followUpActEntries(followUp())
			.filter(deckActEntryResolvesCard)
			.map((entry) => entry.kind);
		expect(resolving).toEqual(['snooze']);
	});
});

describe('deck Act menu — Worth card', () => {
	it('lists Open, the card capabilities and Details, naming the agent', () => {
		const entries = worthActEntries(worth(), { agentName: 'Nova', chatThreadId: 'general' });
		expect(entries.map((entry) => entry.label)).toEqual([
			'Open',
			'Ask Nova',
			'Create task',
			'Summarize deeper',
			'Details'
		]);
		expect(entries.map((entry) => entry.label)).not.toContain('Ask Presto');
		expect(entries.find((entry) => entry.label === 'Ask Nova')).toMatchObject({
			id: 'ask-presto',
			kind: 'resurfacing_action',
			action: 'ask_presto'
		});
		expect(entries.filter(deckActEntryResolvesCard).map((entry) => entry.kind)).toEqual(['open']);
	});

	it('hides Ask <agent> without a chat thread', () => {
		const entries = worthActEntries(worth(), { agentName: 'Nova', chatThreadId: null });
		expect(entries.some((entry) => entry.kind === 'resurfacing_action' && entry.action === 'ask_presto')).toBe(false);
	});

	it('returns nothing for a card without a backing record', () => {
		expect(deckActEntries({}, { agentName: 'Nova' })).toEqual([]);
	});
});

describe('Worth Open inside the click gesture', () => {
	afterEach(() => vi.unstubAllGlobals());

	it('opens an external-only source at once and lets the host skip its repeat once', () => {
		const open = vi.fn();
		vi.stubGlobal('window', { open });
		const card = worth({ candidate_id: 'ext', open_url: 'https://example.com/doc', source_route: null });
		openWorthLinkInGesture(card);
		expect(open).toHaveBeenCalledWith('https://example.com/doc', '_blank', 'noopener');
		expect(takeWorthLinkOpenedInGesture('ext')).toBe(true);
		expect(takeWorthLinkOpenedInGesture('ext')).toBe(false);
	});

	it('leaves in-app routes and link-less cards to the host', () => {
		const open = vi.fn();
		vi.stubGlobal('window', { open });
		openWorthLinkInGesture(worth({ candidate_id: 'route', open_url: 'https://example.com', source_route: '/tasks?selected=t1' }));
		openWorthLinkInGesture(worth({ candidate_id: 'none', open_url: null, source_route: null }));
		expect(open).not.toHaveBeenCalled();
		expect(takeWorthLinkOpenedInGesture('route')).toBe(false);
	});
});
