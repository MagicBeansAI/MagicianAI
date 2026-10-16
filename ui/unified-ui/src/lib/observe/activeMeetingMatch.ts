import type { ActiveMeetingSession, UpcomingMeeting } from '$lib/stores/meetingsStore';

export interface MeetingListenMetadata {
	title: string | null;
	url: string | null;
	inferredEvent: UpcomingMeeting | null;
}

function normalizedMeetingUrl(value: string | null | undefined): string | null {
	const trimmed = value?.trim();
	if (!trimmed) return null;

	try {
		const withScheme = /^[a-z][a-z\d+.-]*:\/\//i.test(trimmed)
			? trimmed
			: `https://${trimmed}`;
		const url = new URL(withScheme);
		const host = url.host.toLowerCase().replace(/^www\./, '');
		const path = (url.pathname.replace(/\/+$/, '') || '/').toLowerCase();
		return `${host}${path}`;
	} catch {
		return trimmed
			.toLowerCase()
			.replace(/^[a-z][a-z\d+.-]*:\/\//i, '')
			.replace(/[?#].*$/, '')
			.replace(/\/+$/, '');
	}
}

function normalizedMeetingTitle(value: string | null | undefined): string | null {
	const normalized = value?.normalize('NFKC').trim().toLowerCase().replace(/\s+/g, ' ');
	return normalized || null;
}

/** Stable identity for keyed Upcoming rows, including the calendar account. */
export function upcomingMeetingKey(event: UpcomingMeeting): string {
	const account = event.account?.trim() ?? '';
	if (event.event_id?.trim()) return `${account}\0${event.event_id.trim()}`;
	return [account, event.title, event.start ?? '', event.meet_url ?? ''].join('\0');
}

/**
 * Associates a blank manual Listen action with the sole live calendar event.
 * Explicit user metadata always wins, and overlapping live events are never
 * guessed between. The resolved title/URL are sent to the backend so the
 * identity survives completion, refresh, and movement into Recent.
 */
export function resolveMeetingListenMetadata(
	rawTitle: string,
	rawUrl: string,
	events: UpcomingMeeting[]
): MeetingListenMetadata {
	const title = rawTitle.trim() || null;
	const url = rawUrl.trim() || null;
	if (title || url) return { title, url, inferredEvent: null };

	const liveEvents = events.filter((event) => event.live_now);
	if (liveEvents.length !== 1) return { title: null, url: null, inferredEvent: null };

	const inferredEvent = liveEvents[0];
	return {
		title: inferredEvent.title.trim() || null,
		url: inferredEvent.meet_url?.trim() || null,
		inferredEvent
	};
}

/**
 * Finds the active capture represented by an Upcoming calendar row.
 * URLs are authoritative. Title matching is only a fallback when either side
 * lacks a URL, so same-title meetings with different links remain actionable.
 */
export function findActiveSessionForUpcoming(
	event: UpcomingMeeting,
	sessions: ActiveMeetingSession[]
): ActiveMeetingSession | null {
	const eventUrl = normalizedMeetingUrl(event.meet_url);
	if (eventUrl) {
		const urlMatch = sessions.find(
			(session) => normalizedMeetingUrl(session.url) === eventUrl
		);
		if (urlMatch) return urlMatch;
	}

	const eventTitle = normalizedMeetingTitle(event.title);
	if (!eventTitle) return null;

	return (
		sessions.find((session) => {
			if (normalizedMeetingTitle(session.title) !== eventTitle) return false;
			const sessionUrl = normalizedMeetingUrl(session.url);
			return !eventUrl || !sessionUrl;
		}) ?? null
	);
}

const EARLY_CAPTURE_WINDOW_MS = 10 * 60 * 1_000;
const LATE_CAPTURE_WINDOW_MS = 30 * 60 * 1_000;

function eventIsNearNow(event: UpcomingMeeting, nowMs: number): boolean {
	const startMs = event.start ? Date.parse(event.start) : Number.NaN;
	const endMs = event.end ? Date.parse(event.end) : startMs;
	if (!Number.isFinite(startMs) || !Number.isFinite(endMs)) return false;
	return nowMs >= startMs - EARLY_CAPTURE_WINDOW_MS && nowMs <= endMs + LATE_CAPTURE_WINDOW_MS;
}

/**
 * Builds one-to-one Upcoming/session matches. The final temporal fallback only
 * applies when both sides are unambiguous and the active session has no usable
 * identity at all (for example, an older ad-hoc capture started with blank
 * metadata while a single calendar meeting was in progress).
 */
export function matchActiveSessionsToUpcoming(
	events: UpcomingMeeting[],
	sessions: ActiveMeetingSession[],
	nowMs = Date.now()
): Map<string, ActiveMeetingSession> {
	const matches = new Map<string, ActiveMeetingSession>();
	const claimedSessionIds = new Set<string>();

	for (const event of events) {
		const availableSessions = sessions.filter(
			(session) => !claimedSessionIds.has(session.session_id)
		);
		const match = findActiveSessionForUpcoming(event, availableSessions);
		if (!match) continue;
		matches.set(upcomingMeetingKey(event), match);
		claimedSessionIds.add(match.session_id);
	}

	const anonymousSessions = sessions.filter(
		(session) =>
			!claimedSessionIds.has(session.session_id) &&
			!normalizedMeetingTitle(session.title) &&
			!normalizedMeetingUrl(session.url)
	);
	const nearbyUnmatchedEvents = events.filter(
		(event) => !matches.has(upcomingMeetingKey(event)) && eventIsNearNow(event, nowMs)
	);

	if (anonymousSessions.length === 1 && nearbyUnmatchedEvents.length === 1) {
		matches.set(upcomingMeetingKey(nearbyUnmatchedEvents[0]), anonymousSessions[0]);
	}

	return matches;
}
