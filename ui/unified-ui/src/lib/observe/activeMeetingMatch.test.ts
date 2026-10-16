import { describe, expect, it } from 'vitest';
import type { ActiveMeetingSession, UpcomingMeeting } from '$lib/stores/meetingsStore';
import {
	findActiveSessionForUpcoming,
	matchActiveSessionsToUpcoming,
	resolveMeetingListenMetadata,
	upcomingMeetingKey
} from './activeMeetingMatch';

function upcoming(overrides: Partial<UpcomingMeeting> = {}): UpcomingMeeting {
	return {
		event_id: 'calendar-event',
		title: 'Design review',
		start: '2026-07-15T10:00:00Z',
		end: '2026-07-15T10:30:00Z',
		meet_url: 'https://meet.google.com/abc-defg-hij',
		live_now: true,
		account: 'work',
		...overrides
	};
}

function active(overrides: Partial<ActiveMeetingSession> = {}): ActiveMeetingSession {
	return {
		session_id: 'session-1',
		mode: 'passive',
		status: 'listening',
		thread_id: 'meeting-design-review-2026-07-15',
		title: 'Design review',
		url: 'https://meet.google.com/abc-defg-hij',
		...overrides
	};
}

describe('Upcoming meeting active-session matching', () => {
	it('persists the sole live event identity for a blank manual listen action', () => {
		const event = upcoming();
		expect(resolveMeetingListenMetadata('', '  ', [event])).toEqual({
			title: 'Design review',
			url: 'https://meet.google.com/abc-defg-hij',
			inferredEvent: event
		});
	});

	it('does not override explicit listen metadata or guess between live events', () => {
		const first = upcoming({ event_id: 'first' });
		const second = upcoming({ event_id: 'second', title: 'Another review' });
		expect(resolveMeetingListenMetadata('Ad-hoc call', '', [first])).toEqual({
			title: 'Ad-hoc call',
			url: null,
			inferredEvent: null
		});
		expect(resolveMeetingListenMetadata('', '', [first, second])).toEqual({
			title: null,
			url: null,
			inferredEvent: null
		});
	});

	it('matches canonical meeting URLs despite presentation differences', () => {
		const session = active({ url: 'meet.google.com/abc-defg-hij/?authuser=1' });
		expect(findActiveSessionForUpcoming(upcoming(), [session])).toBe(session);
	});

	it('falls back to a normalized title when a URL is unavailable', () => {
		const session = active({ title: '  DESIGN   REVIEW ', url: null });
		expect(findActiveSessionForUpcoming(upcoming(), [session])).toBe(session);
	});

	it('does not merge same-title meetings with different URLs', () => {
		const session = active({ url: 'https://meet.google.com/other-room' });
		expect(findActiveSessionForUpcoming(upcoming(), [session])).toBeNull();
	});

	it('includes the account in keyed calendar identities', () => {
		expect(upcomingMeetingKey(upcoming({ account: 'work' }))).not.toBe(
			upcomingMeetingKey(upcoming({ account: 'personal' }))
		);
	});

	it('associates one identity-less capture with one event near the current time', () => {
		const event = upcoming({
			start: '2026-07-15T20:30:00+05:30',
			end: '2026-07-15T21:00:00+05:30'
		});
		const session = active({ title: null, url: null });
		const matches = matchActiveSessionsToUpcoming(
			[event],
			[session],
			Date.parse('2026-07-15T21:10:00+05:30')
		);
		expect(matches.get(upcomingMeetingKey(event))).toBe(session);
	});

	it('does not guess when identity-less capture matching is ambiguous', () => {
		const first = upcoming({ event_id: 'first' });
		const second = upcoming({ event_id: 'second', title: 'Second review' });
		const session = active({ title: null, url: null });
		const matches = matchActiveSessionsToUpcoming(
			[first, second],
			[session],
			Date.parse('2026-07-15T10:15:00Z')
		);
		expect(matches).toHaveLength(0);
	});

	it('does not associate an identity-less capture with a distant event', () => {
		const event = upcoming();
		const session = active({ title: null, url: null });
		const matches = matchActiveSessionsToUpcoming(
			[event],
			[session],
			Date.parse('2026-07-15T18:00:00Z')
		);
		expect(matches).toHaveLength(0);
	});
});
