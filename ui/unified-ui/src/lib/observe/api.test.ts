import { describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import {
	enrollAmbientBrowser,
	fetchAmbientStatus,
	fetchRecentMeetings,
	fetchRecentWatchSessions,
	fetchUpcomingMeetings,
	joinMeeting,
	retargetScreenObservation,
	saveAmbientConfig,
	setMeetingPaused,
	startMeetingListen,
	startScreenObservation,
	stopMeetingSession,
	stopScreenObservation
} from './api';

describe('observe API', () => {
	it('normalizes recent and upcoming listing payloads', async () => {
		installFetchMock([
			{
				match: '/meetings/upcoming',
				handle: () =>
					jsonResponse({
						events: [{ event_id: 'event-1', title: 'Review', live_now: true }],
						errors: [{ account: 'personal', error: 'calendar unavailable' }]
					})
			},
			{
				match: '/meetings',
				handle: () => jsonResponse({ recent: [{ thread_id: 'thread-1' }] })
			}
		]);

		expect(await fetchRecentMeetings()).toEqual([{ thread_id: 'thread-1' }]);
		expect(await fetchUpcomingMeetings(true)).toEqual({
			events: [{ event_id: 'event-1', title: 'Review', live_now: true }],
			errors: [{ account: 'personal', error: 'calendar unavailable' }]
		});
	});

	it('sends complete listen and join requests', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/meetings/listen', handle: () => jsonResponse({}) },
			{ method: 'POST', match: '/meetings/join', handle: () => jsonResponse({}) }
		]);

		await startMeetingListen({ title: 'Planning', url: null, mic: true });
		await joinMeeting({ url: 'https://meet.example/abc', title: null });

		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
			title: 'Planning',
			url: null,
			mic: true
		});
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
			url: 'https://meet.example/abc',
			title: null
		});
	});

	it('encodes meeting control paths and propagates backend errors', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/pause', handle: () => jsonResponse({}) },
			{
				method: 'POST',
				match: '/stop',
				handle: () => jsonResponse({ error: 'capture already ended' }, { status: 409 })
			}
		]);

		await setMeetingPaused('session/with slash', false);
		expect(calls[0]?.url).toContain('/session%2Fwith%20slash/pause');
		await expect(stopMeetingSession('session-1')).rejects.toThrow('capture already ended');
	});

	it('caps recent screen sessions and treats malformed lists as empty', async () => {
		const sessions = Array.from({ length: 24 }, (_, index) => ({ id: `watch-${index}` }));
		installFetchMock([
			{ match: '/chat/sessions', handle: () => jsonResponse({ sessions }) }
		]);

		expect(await fetchRecentWatchSessions()).toHaveLength(20);
	});

	it('sends start, retarget, and stop screen-observation commands', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/observe/start', handle: () => jsonResponse({}) },
			{ method: 'POST', match: '/observe/retarget', handle: () => jsonResponse({}) },
			{ method: 'POST', match: '/observe/stop', handle: () => jsonResponse({}) }
		]);

		await startScreenObservation({
			purpose: 'Capture decisions',
			mode: 'watch',
			watch_for: 'deployment complete',
			deep_observation: true,
			audio: 'system'
		});
		await retargetScreenObservation({ watch_for: 'tests pass' });
		await stopScreenObservation();

		expect(JSON.parse(String(calls[0]?.init?.body))).toMatchObject({
			mode: 'watch',
			watch_for: 'deployment complete',
			audio: 'system'
		});
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({ watch_for: 'tests pass' });
		expect(calls[2]?.method).toBe('POST');
	});

	it('round-trips ambient status, consent, and pairing', async () => {
		const { calls } = installFetchMock([
			{
				match: '/ambient/status',
				handle: () =>
					jsonResponse({
						enabled: true,
						denylist: ['bank.example'],
						total_signals: 4,
						total_rejected: 1,
						last_signal_at: null,
						paired: true
					})
			},
			{ method: 'PUT', match: '/ambient/config', handle: () => jsonResponse({}) },
			{
				method: 'POST',
				match: '/ambient/enroll',
				handle: () => jsonResponse({ token: 'collector-token' })
			}
		]);

		expect(await fetchAmbientStatus()).toMatchObject({ enabled: true, paired: true });
		await saveAmbientConfig(false, ['bank.example']);
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
			enabled: false,
			denylist: ['bank.example']
		});
		expect(await enrollAmbientBrowser()).toBe('collector-token');
	});

	it('surfaces malformed or failed mutations instead of reporting success', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/meetings/join',
				handle: () => jsonResponse({}, { status: 503 })
			}
		]);

		await expect(
			joinMeeting({ url: 'https://meet.example/abc', title: null })
		).rejects.toThrow('HTTP 503');
	});
});
